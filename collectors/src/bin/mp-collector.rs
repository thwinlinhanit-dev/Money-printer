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
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn flag(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    }

    fn has_flag(args: &[String], name: &str) -> bool {
        args.iter().any(|a| a == name)
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
                vec![format!(
                    r#"{{"method":"SUBSCRIBE","params":["{s}@aggTrade","{s}@markPrice@1s","{s}@depth@100ms"],"id":1}}"#
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
