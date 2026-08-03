//! 24/7 live collector.  One process owns one `(venue, symbol)` recording;
//! cross-venue observations must run as separate processes and merge only at
//! replay (spec 024).
//!
//! Run:
//!   cargo run -p mp-collectors --features live-ws --bin mp-collector -- --symbol BTCUSDT

#[cfg(feature = "live-ws")]
mod inner {
    use mp_collectors::ws::{endpoints, WsEndpoint, WsTransport};
    use mp_collectors::{
        Backoff, BackpressurePolicy, BinanceNormalizer, Collector, CollectorConfig, DriveOutcome,
        Normalizer, RateBudget, Staleness,
    };
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, EventProvenance, MarketEvent, SnapshotSource, StatusKind, Venue};
    use serde::Deserialize;
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Seconds a stream may stay silent before the watchdog forces a
    /// reconnect (COL-2). Depth@100ms + markPrice@1s mean anything quiet
    /// longer than this is a dead subscription, not a slow venue.
    const STALE_AFTER_NS: i64 = 15_000_000_000;

    fn flag(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileConfig {
        venue: String,
        symbol: String,
        #[serde(default = "default_data_dir")]
        data_dir: String,
        #[serde(default = "default_channel_capacity")]
        channel_capacity: usize,
        #[serde(default)]
        backpressure: Option<String>,
    }

    fn default_data_dir() -> String { "data".to_owned() }
    fn default_channel_capacity() -> usize { 10_000 }

    fn config_from_args(args: &[String]) -> Result<FileConfig, String> {
        if let Some(path) = flag(args, "--config") {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("read collector config {path}: {error}"))?;
            let config: FileConfig = toml::from_str(&text)
                .map_err(|error| format!("parse collector config {path}: {error}"))?;
            if config.symbol.is_empty() || config.venue.is_empty() || config.channel_capacity == 0 {
                return Err("collector config requires non-empty venue/symbol and channel_capacity > 0".into());
            }
            return Ok(config);
        }
        Ok(FileConfig {
            venue: flag(args, "--venue").unwrap_or_else(|| "binance".to_string()),
            symbol: flag(args, "--symbol").unwrap_or_else(|| "BTCUSDT".to_string()),
            data_dir: default_data_dir(),
            channel_capacity: default_channel_capacity(),
            backpressure: None,
        })
    }

    /// SIGTERM/Ctrl+C handler per spec 019 COL-19: a dedicated
    /// `tokio::signal::ctrl_c()` future flips this flag; the (synchronous)
    /// poll loop checks it each iteration, flushes, and exits with status 0.
    /// One-shot install; repeated calls return the same flag.
    fn shutdown_flag() -> Arc<AtomicBool> {
        static FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
        FLAG.get_or_init(|| {
            let flag = Arc::new(AtomicBool::new(false));
            let f = flag.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                if let Ok(rt) = rt {
                    rt.block_on(async move {
                        if tokio::signal::ctrl_c().await.is_ok() {
                            f.store(true, Ordering::SeqCst);
                        }
                    });
                }
            });
            flag
        })
        .clone()
    }

    /// PID file per spec 019 COL-18/19: `{data_dir}/mp-collector-{venue}.pid`
    /// on this platform (Linux systemd target uses the same data-relative
    /// path so operators find it next to the logs). Removed on clean exit.
    struct PidFile {
        path: PathBuf,
    }

    impl PidFile {
        fn write(raw_dir: &Path, venue: &str) -> io::Result<Self> {
            let path = raw_dir.join(format!("mp-collector-{venue}.pid"));
            std::fs::write(&path, format!("{}\n", std::process::id()))?;
            Ok(Self { path })
        }
    }

    impl Drop for PidFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
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

    /// Wall-clock as ns for the binary edge: recv stamping, rate-budget
    /// refill, watchdog timers — never a feature/decision value (PD-3).
    fn now_ns() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64
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
            // Binance uses combined-stream URL (streams baked into path) — no
            // SUBSCRIBE frame needed. Empty here on purpose.
            "binance" => vec![],
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

    /// Public WS URL for a venue. Binance is special: futures combined-stream
    /// URL with symbol streams embedded (reliable; avoids SUBSCRIBE race on /ws).
    fn endpoint_for(venue: &str, symbol: &str) -> Result<String, String> {
        Ok(match venue {
            "bybit" => endpoints::BYBIT_LINEAR.to_string(),
            "binance" => {
                let s = symbol.to_lowercase();
                // depth@100ms = incremental depth update stream (U/u/pu continuity, Spec 020).
                // markPrice@1s carries mark + funding rate. forceOrder = liqs.
                // OI remains REST-only on Binance.
                format!(
                    "{base}?streams={s}@aggTrade/{s}@markPrice@1s/{s}@depth@100ms/{s}@forceOrder",
                    base = endpoints::BINANCE_FUTURES_COMBINED
                )
            }
            "okx" => endpoints::OKX_PUBLIC.to_string(),
            "hyperliquid" => endpoints::HYPERLIQUID.to_string(),
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
        symbol: String,
        endpoint: WsEndpoint,
        venue: Venue,
        /// For Binance streams, the symbol name used to seed the book via REST.
        binance_symbol: Option<String>,
        collector: Collector<Box<dyn Normalizer>>,
        transport: Option<WsTransport>,
        backoff: Backoff,
        connection_id: u64,
        channel_capacity: usize,
        backpressure: BackpressurePolicy,
        /// COL-2 staleness watchdog (last `recv_ts_ns` of any valid event).
        staleness: Staleness,
        /// COL-21 REST rate budget for snapshot reseeds (futures weights/min).
        #[cfg(feature = "live-http")]
        rest_budget: RateBudget,
    }

    impl Stream {
        fn new(name: String, venue_str: &str, symbol: &str, seed: u64, channel_capacity: usize, backpressure: BackpressurePolicy) -> Result<Self, String> {
            let venue = parse_venue(venue_str)?;
            let url = endpoint_for(venue_str, symbol)?;
            let subscribe = subscribe_for(venue_str, symbol);
            // Binance embeds streams in the URL (empty subscribe is OK).
            if subscribe.is_empty() && venue_str != "binance" {
                return Err(format!("no subscribe frames for {venue_str}"));
            }
            Ok(Self {
                name,
                symbol: symbol.to_owned(),
                endpoint: WsEndpoint::new(url, subscribe),
                venue,
                binance_symbol: if venue_str == "binance" {
                    Some(symbol.to_string())
                } else {
                    None
                },
                collector: Collector::new(
                    mp_collectors::normalizer_for(venue),
                    CollectorConfig::default(),
                ),
                transport: None,
                // 250ms base, 30s cap — COL-1 full-jitter backoff
                backoff: Backoff::new(250, 30_000, seed),
                connection_id: 0,
                channel_capacity,
                backpressure,
                staleness: Staleness::new(STALE_AFTER_NS),
                #[cfg(feature = "live-http")]
                rest_budget: RateBudget::binance_futures(now_ns()),
            })
        }

        fn provenance(&self, body: &MarketEvent, snapshot_source: SnapshotSource) -> EventProvenance {
            let stream = match body {
                MarketEvent::Trade { .. } => "trade",
                MarketEvent::BookDelta { .. } | MarketEvent::BookSnapshot { .. } => "book",
                MarketEvent::Funding { .. } => "funding",
                MarketEvent::MarkPrice { .. } => "mark_price",
                MarketEvent::OpenInterest { .. } => "open_interest",
                MarketEvent::Liquidation { .. } => "liquidation",
                MarketEvent::IndexPrice { .. } => "index_price",
                MarketEvent::Status { .. } => "status",
            };
            EventProvenance {
                stream: stream.to_owned(),
                subscription: self.endpoint.url.clone(),
                connection_id: self.connection_id,
                snapshot_source,
            }
        }

        fn stamp(&self, events: &mut [EventEnvelope], snapshot_source: SnapshotSource) {
            for event in events {
                let source = if matches!(event.body, MarketEvent::BookSnapshot { .. }) {
                    snapshot_source
                } else {
                    SnapshotSource::None
                };
                event.provenance = self.provenance(&event.body, source);
            }
        }

        fn ensure_connected(&mut self, seed_buf: &mut Vec<EventEnvelope>) {
            if self.transport.is_some() {
                return;
            }
            match WsTransport::connect_with_policy(self.endpoint.clone(), self.channel_capacity, self.backpressure) {
                Ok(t) => {
                    tracing::info!(stream = %self.name, venue = ?self.venue, "connected");
                    self.backoff.reset();
                    self.connection_id = self.connection_id.saturating_add(1);
                    self.transport = Some(t);

                    // Seed Binance book from REST immediately after WS connect,
                    // BEFORE any depthUpdate messages are processed (COL-22).
                    // Synthetic-seeding from the first delta was removed spec 020:
                    // without a successful REST seed, depth deltas drop until
                    // the reseed loop below fetches one.
                    #[cfg(feature = "live-http")]
                    if let Some(sym) = self.binance_symbol.clone() {
                        let now_ns = mp_collectors::binance::wall_now_ns();
                        let before = seed_buf.len();
                        let norm = self.collector.normalizer_mut();
                        if let Some(bn) = norm.as_any_mut().and_then(|a| a.downcast_mut::<BinanceNormalizer>()) {
                            match mp_collectors::binance::inject_rest_depth_seed_budgeted(
                                bn, &sym, now_ns, seed_buf, Some(&mut self.rest_budget),
                            ) {
                                Ok(()) => {}
                                Err(e) => tracing::warn!(
                                    stream = %self.name,
                                    error = %e,
                                    "REST depth seed failed; depth deltas drop until a reseed succeeds"
                                ),
                            }
                        }
                        self.stamp(&mut seed_buf[before..], SnapshotSource::Rest);
                    }
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

        /// COL-2 watchdog check: if this stream produced no valid events inside
        /// `STALE_AFTER_NS`, emit `Status::Stale` and drop the transport so the
        /// loop reconnects. Returns true when it forced a reconnect.
        fn check_stale(&mut self, out: &mut Vec<EventEnvelope>, now_recv_ns: i64) -> bool {
            if self.staleness.stale_streams(now_recv_ns).is_empty() {
                return false;
            }
            tracing::warn!(stream = %self.name, symbol = %self.symbol, "stream stale; reconnecting (COL-2)");
            // Reset the observation so we don't emit one Stale per iteration.
            self.staleness.observe(&self.symbol, now_recv_ns);
            let symbol = self
                .collector
                .normalizer()
                .symbols()
                .lookup(self.venue, &self.symbol)
                .unwrap_or(mp_core::SymbolId(0));
            let stale = EventEnvelope::new(
                self.venue,
                symbol,
                now_recv_ns,
                now_recv_ns,
                0,
                MarketEvent::Status {
                    kind: StatusKind::Stale,
                    detail: format!("no valid event for {} ns", STALE_AFTER_NS),
                },
            );
            let provenance = self.provenance(&stale.body, SnapshotSource::None);
            out.push(stale.with_provenance(provenance));
            self.collector.normalizer_mut().reset_books();
            self.transport = None;
            true
        }

        /// Poll once; appends events to `out`. Returns true if work happened.
        /// `now_recv_ns` is the wall clock stamped at the loop edge (the only
        /// place a collector may read the OS clock — recv stamping).
        fn poll(&mut self, out: &mut Vec<EventEnvelope>, now_recv_ns: i64) -> bool {
            self.ensure_connected(out);
            // COL-24: a pu gap asks the driver to re-seed from REST. Do it on
            // this same loop iteration (no sleep inline) so trades keep flowing.
            #[cfg(feature = "live-http")]
            if let Some(sym) = self.binance_symbol.clone() {
                let norm = self.collector.normalizer_mut();
                if let Some(bn) = norm.as_any_mut().and_then(|a| a.downcast_mut::<BinanceNormalizer>()) {
                    if bn.needs_reseed() {
                        let before = out.len();
                        match mp_collectors::binance::reseed_if_needed(bn, &sym, 0, out, Some(&mut self.rest_budget)) {
                            Ok(true) => {
                                self.stamp(&mut out[before..], SnapshotSource::Rest);
                            }
                            Ok(false) => {}
                            Err(e) => tracing::warn!(
                                stream = %self.name,
                                error = %e,
                                "depth re-seed failed; book stays desynced until next attempt"
                            ),
                        }
                    }
                }
            }
            let before = out.len();
            let (outcome, metrics) = {
                let Some(t) = self.transport.as_mut() else {
                    // Disconnected right now: still tick the staleness watchdog —
                    // a silently dead socket is exactly the case COL-2 covers.
                    self.check_stale(out, now_recv_ns);
                    return !out.is_empty();
                };
                let outcome = self.collector.drive(t, out);
                (outcome, t.take_metrics())
            };
            let new_events = &mut out[before..];
            self.stamp(new_events, SnapshotSource::WebSocket);
            // COL-2: note valid events for the staleness watchdog. The ws
            // transport stamps recv_ts_ns at socket read; fall back to the loop
            // edge when an event has none (REST-injected events use 0).
            for ev in new_events.iter() {
                let ts = if ev.recv_ts_ns > 0 { ev.recv_ts_ns } else { now_recv_ns };
                self.staleness.observe(&self.symbol, ts);
            }
            // COL-2: stream silent past the threshold ⇒ declare stale, force
            // reconnect (book untrusted from here until a fresh seed).
            if self.check_stale(out, now_recv_ns) {
                return true;
            }
            if metrics.dropped_frames > 0 {
                // Conservatively invalidate the book after *any* frame loss:
                // a dropped depth update is indistinguishable from a dropped
                // trade here, and a stale book is worse than no book (BKP-3).
                self.collector.normalizer_mut().reset_books();
                let symbol = self
                    .collector
                    .normalizer()
                    .symbols()
                    .lookup(self.venue, &self.symbol)
                    .unwrap_or(mp_core::SymbolId(0));
                let now_ns = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as i64;
                let status = EventEnvelope::new(
                    self.venue,
                    symbol,
                    now_ns,
                    now_ns,
                    0,
                    MarketEvent::Status {
                        kind: mp_core::StatusKind::BackpressureDrop {
                            dropped: metrics.dropped_frames,
                        },
                        detail: format!("queue_high_water={}", metrics.queue_high_water),
                    },
                );
                let provenance = self.provenance(&status.body, SnapshotSource::None);
                out.push(status.with_provenance(provenance));
            }
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
        let config = config_from_args(&args)?;
        let venue = config.venue;
        let symbol = config.symbol;
        let backpressure = match config.backpressure.as_deref() {
            Some(value) => BackpressurePolicy::from_toml(value)
                .ok_or_else(|| format!("invalid backpressure policy: {value}"))?,
            None => BackpressurePolicy::default(),
        };

        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut streams = vec![Stream::new("primary".into(), &venue, &symbol, 1, config.channel_capacity, backpressure)?];

        let raw_dir = Path::new(&config.data_dir).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        // Held for process lifetime — prevents dual-writer log corruption.
        let _instance_lock = InstanceLock::acquire(&raw_dir, &venue, &symbol)?;
        tracing::info!(venue = %venue, symbol = %symbol, "instance lock acquired");
        // COL-18/19: PID file for systemd/monitoring; removed on clean exit.
        let _pid_file = PidFile::write(&raw_dir, &venue)?;
        let shutdown = shutdown_flag();

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count: usize = 0;
        #[cfg(feature = "live-http")]
        let mut last_oi_poll = std::time::Instant::now();
        #[cfg(feature = "live-http")]
        let oi_poll_interval = Duration::from_secs(30);

        let heartbeat_path = raw_dir.join(format!("mp-collector-{venue}-{symbol}.heartbeat"));
        let mut last_heartbeat = std::time::Instant::now();
        let heartbeat_interval = Duration::from_secs(15);

        loop {
            let mut event_buffer = Vec::new();
            let mut any = false;

            if last_heartbeat.elapsed() >= heartbeat_interval || current_date.is_empty() {
                last_heartbeat = std::time::Instant::now();
                let ts_sec = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let content = format!(
                    "ts={ts_sec} pid={} venue={venue} symbol={symbol}\n",
                    std::process::id()
                );
                let _ = std::fs::write(&heartbeat_path, content);
            }

            #[cfg(feature = "live-http")]
            if venue == "binance" && (last_oi_poll.elapsed() >= oi_poll_interval || current_date.is_empty()) {
                last_oi_poll = std::time::Instant::now();
                // COL-21: the OI poll shares the stream's REST budget so
                // snapshot reseeds and OI fetches together respect the venue limit.
                if !streams[0].rest_budget.try_take(now_ns(), 1.0) {
                    tracing::debug!("OI poll skipped: REST rate budget empty (COL-21)");
                } else if let Ok(oi_body) = mp_collectors::binance::fetch_open_interest_blocking(&symbol) {
                    let now_ns = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as i64;
                    let sym_id = streams[0]
                        .collector
                        .normalizer()
                        .symbols()
                        .lookup(Venue::BinanceFutures, &symbol)
                        .unwrap_or(mp_core::SymbolId(0));
                    let event = EventEnvelope::new(
                        Venue::BinanceFutures,
                        sym_id,
                        now_ns,
                        now_ns,
                        0,
                        oi_body,
                    );
                    let provenance = streams[0].provenance(&event.body, SnapshotSource::None);
                    event_buffer.push(event.with_provenance(provenance));
                    any = true;
                }
            }

            // Loop-edge wall clock: recv stamping + watchdog timers only
            // (PD-3: never a decision/feature value).
            let loop_now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i64;
            for s in &mut streams {
                if s.poll(&mut event_buffer, loop_now_ns) {
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
                    let symbols = streams[0].collector.normalizer().symbols();
                    if symbols.len() != last_symbol_count {
                        w.write_symbols(symbols.metas())?;
                        last_symbol_count = symbols.len();
                    }
                    for ev in &event_buffer {
                        w.append(ev)?;
                    }
                    let _ = w.flush();
                }
            }

            // COL-19: graceful shutdown — the current event_buffer is already
            // flushed above; fsync via flush(), PID/lock freed by their Drops.
            if shutdown.load(Ordering::SeqCst) {
                tracing::info!(venue = %venue, symbol = %symbol, "SIGTERM/Ctrl+C received; flushed and exiting (COL-19)");
                if let Some(ref mut w) = log_writer {
                    let _ = w.flush();
                }
                return Ok(());
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
