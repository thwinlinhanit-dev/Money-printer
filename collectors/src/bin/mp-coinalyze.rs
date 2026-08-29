//! Coinalyze cross-exchange validation collector (spec 047, COZ-1..10).
//! Polls `api.coinalyze.net/v1` history endpoints and records the LAST
//! datapoint of each symbol's series as `MacroPoint` rows enveloped with
//! [`Venue::Coinalyze`], written to `data/raw/{date}_coinalyze_macro.log`.
//! **Validation/context grade â€” never alpha** (COZ-9; predicted-funding is
//! public telegraphy). Per-row idempotency (COZ-6): a watermark file per
//! `(series_id)` records the last recorded date; older/equal dates skip.
//!
//! Key from env `COINALYZE_API_KEY` only (COZ-1, PD-2).
//! Endpoint/field names are config-driven and MUST be verified against the
//! live docs at deployment (venue schema drift = pitfall #2); a wrong guess
//! yields zero honest rows rather than fabricated ones.

fn main() {
    #[cfg(not(feature = "live-http"))]
    {
        eprintln!("Error: 'live-http' feature required (REST poller).");
        eprintln!(
            "  COINALYZE_API_KEY=... cargo run -p mp-collectors --features live-http --bin mp-coinalyze -- \\\n     --config collectors/coinalyze.toml.example"
        );
        std::process::exit(1);
    }

    #[cfg(feature = "live-http")]
    {
        tracing_subscriber::fmt::init();
        if let Err(e) = impl_::run() {
            tracing::error!(error = %e, "coinalyze collector failed");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "live-http")]
mod impl_ {
    use mp_collectors::binutil::{self, InstanceLock, PidFile};
    use mp_collectors::coinalyze::{rest, CoinalyzeNormalizer};
    use mp_collectors::Normalizer;
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, StatusKind, Venue};
    use serde::Deserialize;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// One configured pull: endpoint + payload field + series prefix and its
    /// cadence in hours (spec: OI/funding/liq hourly, long/short daily).
    #[derive(Debug, Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Pull {
        endpoint: String,
        field: String,
        value_field: String,
        prefix: String,
        #[serde(default)]
        extra_query: String,
        every_hours: u64,
    }

    fn default_pulls() -> Vec<Pull> {
        vec![
            Pull {
                endpoint: "/open-interest-history".into(),
                field: "history".into(),
                value_field: "c".into(),
                prefix: "AGG_OI".into(),
                extra_query: "&interval=1hour".into(),
                every_hours: 1,
            },
            Pull {
                endpoint: "/funding-rate-history".into(),
                field: "history".into(),
                value_field: "c".into(),
                prefix: "AGG_FUNDING".into(),
                extra_query: "&interval=1hour".into(),
                every_hours: 1,
            },
            Pull {
                endpoint: "/liquidation-history".into(),
                field: "history".into(),
                value_field: "l+s".into(),
                prefix: "AGG_LIQ".into(),
                extra_query: "&interval=1hour".into(),
                every_hours: 1,
            },
            Pull {
                endpoint: "/long-short-ratio-history".into(),
                field: "history".into(),
                value_field: "r".into(),
                prefix: "AGG_LS".into(),
                extra_query: "&interval=daily".into(),
                every_hours: 24,
            },
        ]
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SymbolPair {
        coinalyze: String,
        suffix: String,
    }

    fn default_symbols() -> Vec<SymbolPair> {
        vec![
            SymbolPair {
                coinalyze: "BTCUSDT_PERP.A".into(),
                suffix: "BTC".into(),
            },
            SymbolPair {
                coinalyze: "ETHUSDT_PERP.A".into(),
                suffix: "ETH".into(),
            },
        ]
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CoinalyzeConfig {
        #[serde(default = "default_data_dir")]
        data_dir: String,
        /// Scan tick (how often due-pulls are checked); small and cheap.
        #[serde(default = "default_tick_s")]
        tick_s: u64,
        #[serde(default = "default_request_interval_ms")]
        request_interval_ms: u64,
        #[serde(default = "default_symbols")]
        symbols: Vec<SymbolPair>,
        #[serde(default = "default_pulls")]
        pulls: Vec<Pull>,
    }

    fn default_data_dir() -> String {
        "data".to_owned()
    }
    fn default_tick_s() -> u64 {
        60
    }
    fn default_request_interval_ms() -> u64 {
        1_000
    }

    fn validate(cfg: &CoinalyzeConfig) -> Result<(), String> {
        if cfg.tick_s == 0
            || cfg.request_interval_ms == 0
            || cfg.symbols.is_empty()
            || cfg.pulls.is_empty()
        {
            return Err(
                "coinalyze config requires positive tick_s/request_interval_ms, non-empty symbols and pulls".into(),
            );
        }
        for p in &cfg.pulls {
            if p.every_hours == 0 {
                return Err(format!("pull {} needs every_hours > 0", p.prefix));
            }
        }
        Ok(())
    }

    fn config_from_args(args: &[String]) -> Result<CoinalyzeConfig, String> {
        if let Some(path) = binutil::flag(args, "--config") {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("read coinalyze config {path}: {e}"))?;
            let cfg: CoinalyzeConfig =
                toml::from_str(&text).map_err(|e| format!("parse coinalyze config {path}: {e}"))?;
            validate(&cfg)?;
            return Ok(cfg);
        }
        let cfg = CoinalyzeConfig {
            data_dir: default_data_dir(),
            tick_s: default_tick_s(),
            request_interval_ms: default_request_interval_ms(),
            symbols: default_symbols(),
            pulls: default_pulls(),
        };
        validate(&cfg)?;
        Ok(cfg)
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

        // COZ-1: env key only; refuse to run keyless.
        let api_key = rest::api_key_from_env()?;

        let mut normalizer = CoinalyzeNormalizer::new();
        let mut pacer = rest::RequestPacer::new(Duration::from_millis(config.request_interval_ms));
        let raw_dir = Path::new(&config.data_dir).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let _instance_lock = InstanceLock::acquire(&raw_dir, "coinalyze_macro")?;
        let _pid_file = PidFile::write(&raw_dir, "coinalyze_macro")?;

        // Per-(pull,symbol) watermarks (COZ-6): last recorded `YYYY-MM-DD`;
        // rows older/equal are skipped on every later poll/restart.
        let wm_dir = raw_dir.join("coinalyze");
        std::fs::create_dir_all(&wm_dir)?;
        let wm_path = |prefix: &str, suffix: &str| -> PathBuf {
            wm_dir.join(format!("{prefix}_{suffix}.watermark"))
        };
        let mut last_recorded: Vec<Vec<Option<String>>> =
            vec![vec![None; config.symbols.len()]; config.pulls.len()];
        for (pi, pull) in config.pulls.iter().enumerate() {
            for (si, s) in config.symbols.iter().enumerate() {
                last_recorded[pi][si] = std::fs::read_to_string(wm_path(&pull.prefix, &s.suffix))
                    .ok()
                    .map(|d| d.trim().to_owned())
                    .filter(|d| !d.is_empty());
            }
        }
        let mut next_run_hour: Vec<u64> = vec![0; config.pulls.len()];
        let epoch_hour_now = || (binutil::now_ns() / 1_000_000_000 / 3_600) as u64;

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count = 0usize;
        let mut last_written_recv_ns: i64 = 0;
        let mut last_heartbeat = Instant::now();

        loop {
            let mut event_buffer: Vec<EventEnvelope> = Vec::new();

            if last_heartbeat.elapsed() >= Duration::from_secs(15) {
                last_heartbeat = Instant::now();
                binutil::touch_heartbeat(&raw_dir, "coinalyze_macro");
            }

            let hour = epoch_hour_now();
            for (pi, pull) in config.pulls.iter().enumerate() {
                if hour < next_run_hour[pi] {
                    continue;
                }
                next_run_hour[pi] = hour + pull.every_hours.max(1);
                let pairs: Vec<(&str, &str)> = config
                    .symbols
                    .iter()
                    .map(|s| (s.coinalyze.as_str(), s.suffix.as_str()))
                    .collect();
                match rest::fetch_last_datapoints_blocking(
                    &api_key,
                    &pull.endpoint,
                    &pull.field,
                    &pull.value_field,
                    &pull.prefix,
                    &pairs,
                    &pull.extra_query,
                    &mut pacer,
                ) {
                    Ok(snapshot) => {
                        let payload = serde_json::to_vec(&snapshot)
                            .map_err(|e| format!("reserialize: {e}"))?;
                        let poll_recv = binutil::now_ns();
                        normalizer
                            .normalize(poll_recv, &payload, &mut event_buffer)
                            .map_err(|e| format!("coinalyze normalize: {e}"))?;
                        tracing::info!(
                            prefix = %pull.prefix,
                            rows = event_buffer.len(),
                            "Coinalyze datapoints normalized"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(prefix = %pull.prefix, error = %e, "Coinalyze fetch failed");
                        let recv_ns = binutil::now_ns();
                        let sym = normalizer
                            .symbols()
                            .lookup(Venue::Coinalyze, "")
                            .unwrap_or(mp_core::SymbolId(0));
                        event_buffer.push(EventEnvelope::new(
                            Venue::Coinalyze,
                            sym,
                            recv_ns,
                            recv_ns,
                            0,
                            mp_core::MarketEvent::Status {
                                kind: StatusKind::GapDetected,
                                detail: format!("coinalyze {} poll failed", pull.prefix),
                            },
                        ));
                    }
                }
                // COZ-6 dedup: drop rows already recorded (same or older
                // date). MacroPoint.date is ns; watermarks are YYYY-MM-DD.
                let date_str_of =
                    |date_ns: i64| rest::date_of_unix_s(date_ns.div_euclid(1_000_000_000));
                for si in 0..config.symbols.len() {
                    let suffix = config.symbols[si].suffix.clone();
                    event_buffer.retain(|ev| match &ev.body {
                        mp_core::MarketEvent::MacroPoint {
                            series_id, date, ..
                        } => {
                            series_id.ends_with(&format!("_{suffix}"))
                                && last_recorded[pi][si]
                                    .as_deref()
                                    .is_none_or(|r| date_str_of(*date).as_str() > r)
                        }
                        _ => true,
                    });
                    // Record new maxima + persist watermarks.
                    for ev in &event_buffer {
                        if let mp_core::MarketEvent::MacroPoint {
                            series_id, date, ..
                        } = &ev.body
                        {
                            if series_id == &format!("{}_{}", pull.prefix, suffix) {
                                let ds = date_str_of(*date);
                                let newer = last_recorded[pi][si]
                                    .as_deref()
                                    .is_none_or(|r| ds.as_str() > r);
                                if newer {
                                    last_recorded[pi][si] = Some(ds.clone());
                                    let _ = std::fs::write(
                                        wm_path(&pull.prefix, &suffix),
                                        format!("{ds}\n"),
                                    );
                                }
                            }
                        }
                    }
                }
            }

            if !event_buffer.is_empty() {
                let date = binutil::utc_date_str();
                if date != current_date {
                    if let Some(ref mut w) = log_writer {
                        let _ = w.flush();
                    }
                    let log_path = raw_dir.join(format!("{date}_coinalyze_macro.log"));
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

            std::thread::sleep(Duration::from_secs(config.tick_s));
        }
    }
}
