//! 24/7 live collector (SPEC-011). Default: Binance Futures market data +
//! Hyperliquid trade stream for whale tracking (`--no-whale` to disable).
//!
//! Run:
//!   cargo run -p mp-collectors --features live-ws --bin mp-collector -- --symbol BTCUSDT

#[cfg(feature = "live-ws")]
mod inner {
    use mp_collectors::ws::{endpoints, WsEndpoint, WsTransport};
    use mp_collectors::{Backoff, Collector, CollectorConfig, DriveOutcome, Normalizer};
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, Venue};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn flag(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    }

    fn has_flag(args: &[String], name: &str) -> bool {
        args.iter().any(|a| a == name)
    }

    /// Process-lifetime exclusive lock so two collectors cannot write the same
    /// `{venue}_{symbol}` log. Held open for the whole run; released on exit.
    struct InstanceLock {
        _file: File,
        path: PathBuf,
    }

    impl InstanceLock {
        fn acquire(raw_dir: &Path, venue: &str, symbol: &str) -> io::Result<Self> {
            std::fs::create_dir_all(raw_dir)?;
            let path = raw_dir.join(format!(".lock_{venue}_{symbol}"));
            let mut file = exclusive_lock_file(&path).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "another mp-collector already owns {venue}/{symbol} \
                         (lock {}): {e}",
                        path.display()
                    ),
                )
            })?;
            let _ = writeln!(file, "pid={} venue={venue} symbol={symbol}", std::process::id());
            let _ = file.flush();
            Ok(Self { _file: file, path })
        }
    }

    impl Drop for InstanceLock {
        fn drop(&mut self) {
            // Best-effort cleanup; exclusive handle release is the real unlock.
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn exclusive_lock_file(path: &Path) -> io::Result<File> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // share_mode(0) = exclusive; second process gets ERROR_SHARING_VIOLATION.
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .share_mode(0)
                .open(path)
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // O_EXCL|O_CREAT when missing; if exists, try open + fail if stale
            // not recoverable without flock — remove stale only if create_new works
            // after a failed exclusive create by rewriting via create_new on a
            // temp and rename is racy. Prefer: open existing exclusive via
            // flock(LOCK_EX|LOCK_NB) using libc when available; without libc,
            // use create_new and refuse if the file already exists (operator
            // deletes stale lock after crash).
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .open(path)
            {
                Ok(f) => Ok(f),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "lock file exists — another collector is running, or a stale \
                     lock remains after a crash (delete the .lock_* file if sure)",
                )),
                Err(e) => Err(e),
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
        }
    }

    fn utc_date_str() -> String {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let days = d / 86400;
        let mut y = 1970i64;
        let mut rem = days as i64;
        loop {
            let days_yr = if is_leap(y) { 366 } else { 365 };
            if rem < days_yr {
                break;
            }
            rem -= days_yr;
            y += 1;
        }
        let months = [
            31,
            if is_leap(y) { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut m = 0usize;
        while m < 12 && rem >= months[m] {
            rem -= months[m];
            m += 1;
        }
        format!("{:04}{:02}{:02}", y, m + 1, rem + 1)
    }

    fn is_leap(y: i64) -> bool {
        (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
    }

    fn hl_coin(symbol: &str) -> String {
        symbol
            .trim_end_matches("USDT")
            .trim_end_matches("USD")
            .trim_end_matches("PERP")
            .to_string()
    }

    fn subscribe_for(venue: &str, symbol: &str) -> Vec<String> {
        match venue {
            "bybit" => vec![format!(
                r#"{{"op":"subscribe","args":["publicTrade.{symbol}","tickers.{symbol}","liquidation.{symbol}"]}}"#
            )],
            "binance" => {
                let s = symbol.to_lowercase();
                // Futures streams: trades, L2 depth, mark+funding (1s), liquidations.
                // OI is REST-only on Binance — enable whale (HL) or a future REST poller.
                vec![format!(
                    r#"{{"method":"SUBSCRIBE","params":["{s}@aggTrade","{s}@markPrice@1s","{s}@depth@100ms","{s}@forceOrder"],"id":1}}"#
                )]
            }
            "hyperliquid" => {
                let coin = hl_coin(symbol);
                // Subscribe to all channels the normalizer supports
                vec![
                    format!(r#"{{"method":"subscribe","subscription":{{"type":"trades","coin":"{coin}"}}}}"#),
                    format!(r#"{{"method":"subscribe","subscription":{{"type":"l2Book","coin":"{coin}"}}}}"#),
                    format!(r#"{{"method":"subscribe","subscription":{{"type":"activeAssetCtx","coin":"{coin}"}}}}"#),
                ]
            }
            "okx" => vec![format!(
                r#"{{"op":"subscribe","args":[{{"channel":"trades","instId":"{symbol}"}}]}}"#
            )],
            _ => vec![],
        }
    }

    fn endpoint_for(venue: &str) -> Result<&'static str, String> {
        Ok(match venue {
            "bybit" => endpoints::BYBIT_LINEAR,
            "binance" => endpoints::BINANCE_FUTURES,
            "okx" => endpoints::OKX_PUBLIC,
            "hyperliquid" => endpoints::HYPERLIQUID,
            other => return Err(format!("unsupported venue: {other}")),
        })
    }

    fn parse_venue(s: &str) -> Result<Venue, String> {
        Ok(match s {
            "bybit" => Venue::Bybit,
            "binance" => Venue::BinanceFutures,
            "okx" => Venue::Okx,
            "hyperliquid" => Venue::Hyperliquid,
            other => return Err(format!("unsupported venue: {other}")),
        })
    }

    struct Stream {
        name: String,
        endpoint: WsEndpoint,
        venue: Venue,
        collector: Collector<Box<dyn Normalizer>>,
        transport: Option<WsTransport>,
        backoff: Backoff,
    }

    impl Stream {
        fn new(name: String, venue_str: &str, symbol: &str, seed: u64) -> Result<Self, String> {
            let venue = parse_venue(venue_str)?;
            let url = endpoint_for(venue_str)?.to_string();
            let subscribe = subscribe_for(venue_str, symbol);
            if subscribe.is_empty() {
                return Err(format!("no subscribe frames for {venue_str}"));
            }
            Ok(Self {
                name,
                endpoint: WsEndpoint { url, subscribe },
                venue,
                collector: Collector::new(
                    mp_collectors::normalizer_for(venue),
                    CollectorConfig::default(),
                ),
                transport: None,
                // 250ms base, 30s cap — COL-1 full-jitter backoff
                backoff: Backoff::new(250, 30_000, seed),
            })
        }

        fn ensure_connected(&mut self) {
            if self.transport.is_some() {
                return;
            }
            match WsTransport::connect(self.endpoint.clone(), 1024) {
                Ok(t) => {
                    tracing::info!(stream = %self.name, venue = ?self.venue, "connected");
                    self.backoff.reset();
                    self.transport = Some(t);
                }
                Err(e) => {
                    let delay = self.backoff.next_delay_ms();
                    tracing::warn!(
                        stream = %self.name,
                        error = %e,
                        delay_ms = delay,
                        "connect failed"
                    );
                    std::thread::sleep(Duration::from_millis(delay));
                }
            }
        }

        /// Poll once; appends events to `out`. Returns true if work happened.
        fn poll(&mut self, out: &mut Vec<EventEnvelope>) -> bool {
            self.ensure_connected();
            let Some(t) = self.transport.as_mut() else {
                return false;
            };
            let before = out.len();
            let outcome = self.collector.drive(t, out);
            match outcome {
                DriveOutcome::Exhausted => out.len() > before,
                DriveOutcome::Disconnected | DriveOutcome::ParseFailureLimit => {
                    tracing::warn!(stream = %self.name, ?outcome, "reconnecting");
                    self.transport = None;
                    let delay = self.backoff.next_delay_ms();
                    std::thread::sleep(Duration::from_millis(delay));
                    true
                }
            }
        }
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args: Vec<String> = std::env::args().collect();
        // Product default: Binance for market-data WS.
        let venue = flag(&args, "--venue").unwrap_or_else(|| "binance".to_string());
        let symbol = flag(&args, "--symbol").unwrap_or_else(|| "BTCUSDT".to_string());
        // Hyperliquid whale tape ON by default; disable with --no-whale.
        let enable_whale = !has_flag(&args, "--no-whale");

        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut streams = vec![Stream::new("primary".into(), &venue, &symbol, 1)?];
        if enable_whale && venue != "hyperliquid" {
            streams.push(Stream::new("whale".into(), "hyperliquid", &symbol, 2)?);
            tracing::info!(
                coin = %hl_coin(&symbol),
                "hyperliquid whale stream enabled (--no-whale to disable)"
            );
        }

        let raw_dir = Path::new("data").join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        // Held for process lifetime — prevents dual-writer log corruption.
        let _instance_lock = InstanceLock::acquire(&raw_dir, &venue, &symbol)?;
        tracing::info!(venue = %venue, symbol = %symbol, "instance lock acquired");

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count: usize = 0;

        loop {
            let mut event_buffer = Vec::new();
            let mut any = false;
            for s in &mut streams {
                if s.poll(&mut event_buffer) {
                    any = true;
                }
            }

            if !event_buffer.is_empty() {
                any = true;
                let date = utc_date_str();
                if date != current_date {
                    if let Some(ref mut w) = log_writer {
                        let _ = w.flush();
                    }
                    let log_path = raw_dir.join(format!("{date}_{venue}_{symbol}.log"));
                    tracing::info!(path = %log_path.display(), "rotating log file");
                    let (w, truncated) = EventLogWriter::open(&log_path)?;
                    if truncated {
                        tracing::warn!(path = %log_path.display(), "recovered torn tail");
                    }
                    log_writer = Some(w);
                    current_date = date;
                    last_symbol_count = 0;
                }

                if let Some(ref mut w) = log_writer {
                    for s in &streams {
                        let n = s.collector.normalizer().symbols().len();
                        if n != last_symbol_count {
                            w.write_symbols(s.collector.normalizer().symbols().metas())?;
                            last_symbol_count = n;
                        }
                    }
                    for ev in &event_buffer {
                        w.append(ev)?;
                    }
                    let _ = w.flush();
                }
            }

            if !any {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn main() {
    #[cfg(not(feature = "live-ws"))]
    {
        eprintln!("Error: 'live-ws' feature required.");
        eprintln!(
            "  cargo run -p mp-collectors --features live-ws --bin mp-collector -- --symbol BTCUSDT"
        );
        std::process::exit(1);
    }

    #[cfg(feature = "live-ws")]
    {
        tracing_subscriber::fmt::init();
        if let Err(e) = inner::run() {
            tracing::error!(error = %e, "collector failed");
            std::process::exit(1);
        }
    }
}
