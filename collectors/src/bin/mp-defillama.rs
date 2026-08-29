//! DeFiLlama regime collector (spec 046, DEF-1..8). Polls the **keyless**
//! public DeFiLlama API daily and records `MacroPoint` regime series
//! (stablecoin supply, all-chains TVL, DEX volume) to
//! `data/raw/{date}_defillama_macro.log` enveloped with [`Venue::DeFiLlama`].
//! Regime-grade, never execution-grade (DEF-4).
//!
//! Build/run:
//!   cargo run -p mp-collectors --features live-http --bin mp-defillama -- \
//!     --config collectors/defillama.toml.example
//!
//! No API key exists (keyless API) — nothing to read from env (PD-2 trivially
//! satisfied). Snapshot-day idempotency (DEF-8): a watermark under
//! `data/raw/defillama/` skips an already-recorded snapshot day.
//! v1 note: no per-instance lock file beyond the standard pair below.

fn main() {
    #[cfg(not(feature = "live-http"))]
    {
        eprintln!("Error: 'live-http' feature required (REST poller).");
        eprintln!(
            "  cargo run -p mp-collectors --features live-http --bin mp-defillama -- --config collectors/defillama.toml.example"
        );
        std::process::exit(1);
    }

    #[cfg(feature = "live-http")]
    {
        tracing_subscriber::fmt::init();
        if let Err(e) = impl_::run() {
            tracing::error!(error = %e, "defillama collector failed");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "live-http")]
mod impl_ {
    use mp_collectors::binutil::{self, InstanceLock, PidFile};
    use mp_collectors::defillama::{rest, DefiLlamaNormalizer};
    use mp_collectors::Normalizer;
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, StatusKind, Venue};
    use serde::Deserialize;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DefiLlamaConfig {
        #[serde(default = "default_data_dir")]
        data_dir: String,
        /// Daily poll cadence (default 1/day).
        #[serde(default = "default_poll_cadence")]
        poll_cadence_s: u64,
    }

    fn default_data_dir() -> String {
        "data".to_owned()
    }
    fn default_poll_cadence() -> u64 {
        86_400
    }

    fn config_from_args(args: &[String]) -> Result<DefiLlamaConfig, String> {
        if let Some(path) = binutil::flag(args, "--config") {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("read defillama config {path}: {e}"))?;
            let cfg: DefiLlamaConfig =
                toml::from_str(&text).map_err(|e| format!("parse defillama config {path}: {e}"))?;
            if cfg.poll_cadence_s == 0 {
                return Err("defillama config requires cadence > 0".into());
            }
            return Ok(cfg);
        }
        Ok(DefiLlamaConfig {
            data_dir: default_data_dir(),
            poll_cadence_s: default_poll_cadence(),
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

        let mut normalizer = DefiLlamaNormalizer::new();
        let raw_dir = Path::new(&config.data_dir).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let _instance_lock = InstanceLock::acquire(&raw_dir, "defillama_macro")?;
        let _pid_file = PidFile::write(&raw_dir, "defillama_macro")?;

        // Snapshot-day watermark (DEF-8): holds recorded `YYYY-MM-DD`;
        // equal-to-today ⇒ skip the fetch entirely.
        let wm_path = raw_dir.join("defillama").join("snapshot_date.watermark");
        std::fs::create_dir_all(raw_dir.join("defillama"))?;
        let recorded_today = || -> bool {
            std::fs::read_to_string(&wm_path)
                .map(|d| d.trim() == rest::today_date())
                .unwrap_or(false)
        };

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count = 0usize;
        let mut last_written_recv_ns: i64 = 0;

        let mut last_poll = Instant::now() - Duration::from_secs(config.poll_cadence_s);
        let mut last_heartbeat = Instant::now();

        loop {
            let mut event_buffer: Vec<EventEnvelope> = Vec::new();
            let mut any = false;

            if last_heartbeat.elapsed() >= Duration::from_secs(15) {
                last_heartbeat = Instant::now();
                binutil::touch_heartbeat(&raw_dir, "defillama_macro");
            }

            if last_poll.elapsed() >= Duration::from_secs(config.poll_cadence_s) {
                last_poll = Instant::now();
                let poll_recv = binutil::now_ns();
                if recorded_today() {
                    tracing::debug!("defillama snapshot already recorded today (watermark)");
                } else {
                    match serde_json::to_vec(&rest::fetch_snapshot_blocking()) {
                        Ok(payload) => {
                            normalizer
                                .normalize(poll_recv, &payload, &mut event_buffer)
                                .map_err(|e| format!("defillama normalize: {e}"))?;
                            any = true;
                            let _ = std::fs::write(&wm_path, format!("{}\n", rest::today_date()));
                            tracing::info!(
                                recorded = event_buffer.len(),
                                "DeFiLlama snapshot recorded"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "DeFiLlama fetch failed");
                            let sym = normalizer
                                .symbols()
                                .lookup(Venue::DeFiLlama, "")
                                .unwrap_or(mp_core::SymbolId(0));
                            let gap = EventEnvelope::new(
                                Venue::DeFiLlama,
                                sym,
                                poll_recv,
                                poll_recv,
                                0,
                                mp_core::MarketEvent::Status {
                                    kind: StatusKind::GapDetected,
                                    detail: "defillama poll failed (DEF-3 gap surfaced)"
                                        .to_string(),
                                },
                            );
                            event_buffer.push(gap);
                            any = true;
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
                    let log_path = raw_dir.join(format!("{date}_defillama_macro.log"));
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
                std::thread::sleep(Duration::from_millis(1_000));
            }
        }
    }
}
