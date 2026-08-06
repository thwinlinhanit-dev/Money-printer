//! Hyperliquid whale-position collector (spec 028, WHL-1..9). Polls the
//! public Hyperliquid REST (no auth, WHL-1) for the top-N leaderboard
//! addresses + a configured watchlist, records each address's perp positions
//! as `WhalePosition` events to `data/raw/{date}_hyperliquid_positions.log`.
//!
//! Build/run:
//!   cargo run -p mp-collectors --features live-http --bin mp-whale -- \
//!     --config collectors/whale_positions.toml.example
//!
//! Positions are DATA ONLY (WHL-5) — never fed to strategies before
//! event-study grading. Addresses stay opaque 0x ids (WHL-3).

fn main() {
    #[cfg(not(feature = "live-http"))]
    {
        eprintln!("Error: 'live-http' feature required (REST poller).");
        eprintln!(
            "  cargo run -p mp-collectors --features live-http --bin mp-whale -- --config collectors/whale_positions.toml.example"
        );
        std::process::exit(1);
    }

    #[cfg(feature = "live-http")]
    {
        tracing_subscriber::fmt::init();
        if let Err(e) = impl_::run() {
            tracing::error!(error = %e, "whale collector failed");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "live-http")]
mod impl_ {
    use mp_collectors::binutil::{self, InstanceLock, PidFile};
    use mp_collectors::hyperliquid_positions::rest::{
        fetch_clearinghouse_state_blocking, fetch_leaderboard_blocking,
    };
    use mp_collectors::hyperliquid_positions::{gap_detected, HyperliquidPositionsNormalizer};
    use mp_collectors::Normalizer;
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, Venue};
    use serde::Deserialize;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WhaleConfig {
        #[serde(default = "default_data_dir")]
        data_dir: String,
        /// How many top addresses to pull from the leaderboard each top-N poll.
        #[serde(default = "default_top_n")]
        top_n: usize,
        /// Leaderboard poll cadence (WHL-7 default 60s).
        #[serde(default = "default_top_interval")]
        top_poll_interval_s: u64,
        /// Watchlist addresses (opaque 0x) polled on the faster cadence.
        #[serde(default)]
        watchlist: Vec<String>,
        /// Watchlist poll cadence (WHL-7 default 30s).
        #[serde(default = "default_watch_interval")]
        watch_poll_interval_s: u64,
        /// Leaderboard time window, e.g. "7d".
        #[serde(default = "default_window")]
        leaderboard_window: String,
        /// Optional coin filter: record only these coins' positions (empty = all).
        #[serde(default)]
        symbols: Vec<String>,
        /// Pacing between per-address REST fetches (Hyperliquid rate limits).
        #[serde(default = "default_poll_gap")]
        min_poll_gap_ms: u64,
    }

    fn default_data_dir() -> String {
        "data".to_owned()
    }
    fn default_top_n() -> usize {
        50
    }
    fn default_top_interval() -> u64 {
        60
    }
    fn default_watch_interval() -> u64 {
        30
    }
    fn default_window() -> String {
        "7d".to_owned()
    }
    fn default_poll_gap() -> u64 {
        150
    }

    fn config_from_args(args: &[String]) -> Result<WhaleConfig, String> {
        if let Some(path) = binutil::flag(args, "--config") {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("read whale config {path}: {e}"))?;
            let cfg: WhaleConfig =
                toml::from_str(&text).map_err(|e| format!("parse whale config {path}: {e}"))?;
            if cfg.top_n == 0 || cfg.top_poll_interval_s == 0 || cfg.watch_poll_interval_s == 0 {
                return Err("whale config requires top_n > 0 and non-zero poll intervals".into());
            }
            return Ok(cfg);
        }
        let watchlist = binutil::flag(args, "--watchlist")
            .map(|s| {
                s.split(',')
                    .map(|a| a.trim().to_owned())
                    .filter(|a| !a.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let symbols = binutil::flag(args, "--symbols")
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_owned())
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(WhaleConfig {
            data_dir: default_data_dir(),
            top_n: binutil::flag(args, "--top-n")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(default_top_n),
            top_poll_interval_s: default_top_interval(),
            watchlist,
            watch_poll_interval_s: default_watch_interval(),
            leaderboard_window: default_window(),
            symbols,
            min_poll_gap_ms: default_poll_gap(),
        })
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args: Vec<String> = std::env::args().collect();
        if binutil::has_flag(&args, "--version") {
            binutil::version_exit();
        }
        let config = config_from_args(&args)?;
        if binutil::has_flag(&args, "--check-config") {
            binutil::check_config_exit();
        }

        let mut normalizer = HyperliquidPositionsNormalizer::new();
        let raw_dir = Path::new(&config.data_dir).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let _instance_lock = InstanceLock::acquire(&raw_dir, "hyperliquid_positions")?;
        let _pid_file = PidFile::write(&raw_dir, "hyperliquid_positions")?;

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count = 0usize;
        let mut last_written_recv_ns: i64 = 0;

        let mut last_top_poll = Instant::now() - Duration::from_secs(config.top_poll_interval_s);
        let mut last_watch_poll =
            Instant::now() - Duration::from_secs(config.watch_poll_interval_s);
        let mut last_heartbeat = Instant::now();

        loop {
            let mut event_buffer: Vec<EventEnvelope> = Vec::new();
            let mut any = false;
            let recv_ns = binutil::now_ns();

            if last_heartbeat.elapsed() >= Duration::from_secs(15) {
                last_heartbeat = Instant::now();
                binutil::touch_heartbeat(&raw_dir, "hyperliquid_positions");
            }

            // WHL-7: a missed poll window (fetch or parse failure) is surfaced
            // as Status::GapDetected — gaps are data, never silently swallowed.
            let mut fail_top = false;
            let mut fail_watch = false;

            if last_top_poll.elapsed() >= Duration::from_secs(config.top_poll_interval_s) {
                last_top_poll = Instant::now();
                match fetch_leaderboard_blocking(&config.leaderboard_window) {
                    Ok(addresses) => {
                        let poll_recv = binutil::now_ns();
                        for addr in addresses.iter().take(config.top_n) {
                            if let Err(e) = fetch_and_record(
                                &mut normalizer,
                                addr,
                                &config.symbols,
                                poll_recv,
                                &mut event_buffer,
                            ) {
                                tracing::warn!(address = %addr, error = %e, "top-N position fetch failed");
                                fail_top = true;
                            }
                            pace(config.min_poll_gap_ms);
                        }
                        any = true;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "leaderboard poll failed");
                        fail_top = true;
                    }
                }
            }

            if last_watch_poll.elapsed() >= Duration::from_secs(config.watch_poll_interval_s) {
                last_watch_poll = Instant::now();
                let poll_recv = binutil::now_ns();
                for addr in &config.watchlist {
                    if let Err(e) = fetch_and_record(
                        &mut normalizer,
                        addr,
                        &config.symbols,
                        poll_recv,
                        &mut event_buffer,
                    ) {
                        tracing::warn!(address = %addr, error = %e, "watchlist position fetch failed");
                        fail_watch = true;
                    }
                    pace(config.min_poll_gap_ms);
                }
                any = true;
            }

            if fail_top || fail_watch {
                let sym = normalizer
                    .symbols()
                    .lookup(Venue::Hyperliquid, "")
                    .unwrap_or(mp_core::SymbolId(0));
                event_buffer.push(gap_detected(
                    sym,
                    recv_ns,
                    format!("whale poll failed: top={fail_top} watch={fail_watch}"),
                ));
                any = true;
            }

            if !event_buffer.is_empty() {
                let date = binutil::utc_date_str();
                if date != current_date {
                    if let Some(ref mut w) = log_writer {
                        let _ = w.flush();
                    }
                    let log_path = raw_dir.join(format!("{date}_hyperliquid_positions.log"));
                    tracing::info!(path = %log_path.display(), "rotating log file");
                    let (w, truncated) = EventLogWriter::open(&log_path)?;
                    if truncated {
                        tracing::warn!(path = %log_path.display(), "recovered torn tail");
                    }
                    log_writer = Some(w);
                    current_date = date;
                    last_symbol_count = 0;
                    last_written_recv_ns = 0;
                }
                if let Some(ref mut w) = log_writer {
                    let symbols = normalizer.symbols();
                    if symbols.len() != last_symbol_count {
                        w.write_symbols(symbols.metas())?;
                        last_symbol_count = symbols.len();
                    }
                    last_written_recv_ns =
                        mp_collectors::monotonicize(&mut event_buffer, last_written_recv_ns);
                    for ev in &event_buffer {
                        w.append(ev)?;
                    }
                    let _ = w.flush();
                }
            }

            if !any {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    /// Fetch one address's `clearinghouseState` and normalize it into
    /// `WhalePosition` events. An empty `symbols` filter records everything;
    /// otherwise only matching coins are kept (WHL-8 `--symbols`).
    fn fetch_and_record(
        normalizer: &mut HyperliquidPositionsNormalizer,
        address: &str,
        symbols: &[String],
        recv_ns: i64,
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), String> {
        let wrapped = fetch_clearinghouse_state_blocking(address)?;
        let payload = serde_json::to_vec(&wrapped).map_err(|e| format!("reserialize: {e}"))?;
        let before = out.len();
        // SAFETY: `wrapped` is a plain JSON value we just serialized (CONV-13).
        normalizer
            .normalize(recv_ns, &payload, out)
            .map_err(|e| format!("whale normalize: {e}"))?;
        if symbols.is_empty() {
            return Ok(());
        }
        // Coin filter: drop positions whose interned symbol is not in `symbols`.
        let table = normalizer.symbols();
        let mut keep: Vec<EventEnvelope> = Vec::new();
        for ev in out.drain(before..) {
            let coin = table
                .get(ev.symbol)
                .map(|m| m.venue_symbol.clone())
                .unwrap_or_default();
            if symbols.iter().any(|s| s == &coin) {
                keep.push(ev);
            }
        }
        out.extend(keep);
        Ok(())
    }

    /// Pace between per-address fetches to respect Hyperliquid rate limits
    /// (WHL-1: "rate-limited per Hyperliquid docs").
    fn pace(ms: u64) {
        if ms > 0 {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}
