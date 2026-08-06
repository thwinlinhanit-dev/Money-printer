//! Macro data collector (spec 030, MAC-1..8). Polls FRED daily economic
//! series (rates, DXY) via the public FRED API and records `MacroPoint`
//! events to `data/raw/{date}_fred_macro.log`. Correlation-grade, not
//! execution-grade (MAC-5).
//!
//! Build/run:
//!   FRED_API_KEY=... cargo run -p mp-collectors --features live-http --bin mp-macro -- \
//!     --config collectors/macro.toml.example
//!
//! The API key comes from the `FRED_API_KEY` env var only (MAC-2, PD-2) —
//! never from config or the repo. The HIP-3 half of spec 030 runs through
//! the existing hyperliquid collector: `mp-collector --venue hyperliquid
//! --symbol BTC --hip3-symbols "xyz:XYZ100,xyz:SP500"`.

fn main() {
    #[cfg(not(feature = "live-http"))]
    {
        eprintln!("Error: 'live-http' feature required (REST poller).");
        eprintln!(
            "  cargo run -p mp-collectors --features live-http --bin mp-macro -- --config collectors/macro.toml.example"
        );
        std::process::exit(1);
    }

    #[cfg(feature = "live-http")]
    {
        tracing_subscriber::fmt::init();
        if let Err(e) = impl_::run() {
            tracing::error!(error = %e, "macro collector failed");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "live-http")]
mod impl_ {
    use mp_collectors::binutil::{self, InstanceLock, PidFile};
    use mp_collectors::fred::rest::{api_key_from_env, fetch_observations_blocking};
    use mp_collectors::fred::FredNormalizer;
    use mp_collectors::Normalizer;
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, MarketEvent, StatusKind, Venue};
    use serde::Deserialize;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// Default series set (spec 030 Decisions, owner pick): DXY, 10Y, 2Y,
    /// SOFR, Fed Funds Effective. Configurable via `series` / `--series`.
    pub const DEFAULT_SERIES: [&str; 5] = ["DTWEXBGS", "DGS10", "DGS2", "SOFR", "DFF"];

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MacroConfig {
        #[serde(default = "default_data_dir")]
        data_dir: String,
        /// FRED series ids to record. NO keys here — the key is env-only
        /// (MAC-2).
        #[serde(default = "default_series")]
        series: Vec<String>,
        /// Daily poll cadence (MAC-4; default 1/day).
        #[serde(default = "default_poll_cadence")]
        poll_cadence_s: u64,
        /// Initial backfill window in days (observation_start = today - N).
        #[serde(default = "default_backfill")]
        backfill_days: u64,
    }

    fn default_data_dir() -> String {
        "data".to_owned()
    }
    fn default_series() -> Vec<String> {
        DEFAULT_SERIES.iter().map(|s| s.to_string()).collect()
    }
    fn default_poll_cadence() -> u64 {
        86_400
    }
    fn default_backfill() -> u64 {
        30
    }

    fn config_from_args(args: &[String]) -> Result<MacroConfig, String> {
        if let Some(path) = binutil::flag(args, "--config") {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("read macro config {path}: {e}"))?;
            let cfg: MacroConfig =
                toml::from_str(&text).map_err(|e| format!("parse macro config {path}: {e}"))?;
            if cfg.series.is_empty() || cfg.poll_cadence_s == 0 {
                return Err("macro config requires a non-empty series list and cadence > 0".into());
            }
            return Ok(cfg);
        }
        let series = binutil::flag(args, "--series")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_else(default_series);
        Ok(MacroConfig {
            data_dir: default_data_dir(),
            series,
            poll_cadence_s: default_poll_cadence(),
            backfill_days: default_backfill(),
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
        // MAC-2: `--hip3-symbols` is documented guidance — HIP-3 collection
        // runs through the existing hyperliquid collector (MAC-1), not here.
        if let Some(hip3) = binutil::flag(&args, "--hip3-symbols") {
            eprintln!(
                "note: HIP-3 TradFi symbols ({hip3}) run through the existing hyperliquid collector:\n  \
                 mp-collector --venue hyperliquid --symbol BTC --hip3-symbols \"{hip3}\" (spec 030 MAC-1)"
            );
        }

        let api_key = api_key_from_env()?;
        let mut normalizer = FredNormalizer::new();
        let raw_dir = Path::new(&config.data_dir).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let _instance_lock = InstanceLock::acquire(&raw_dir, "fred_macro")?;
        let _pid_file = PidFile::write(&raw_dir, "fred_macro")?;

        // Per-series observation watermark (last recorded date), so daily
        // polls only record new observations and a restart backfills exactly
        // the gap. Bookkeeping file, not market data (W-6 unaffected).
        let watermark_dir = raw_dir.join("fred");
        std::fs::create_dir_all(&watermark_dir)?;
        let watermark_of = |series: &str| watermark_dir.join(format!("{series}.watermark"));

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count = 0usize;
        let mut last_written_recv_ns: i64 = 0;

        let mut last_poll = Instant::now() - Duration::from_secs(config.poll_cadence_s);
        let mut last_heartbeat = Instant::now();

        loop {
            let mut event_buffer: Vec<EventEnvelope> = Vec::new();
            let mut any = false;
            let recv_ns = binutil::now_ns();

            if last_heartbeat.elapsed() >= Duration::from_secs(15) {
                last_heartbeat = Instant::now();
                binutil::touch_heartbeat(&raw_dir, "fred_macro");
            }

            if last_poll.elapsed() >= Duration::from_secs(config.poll_cadence_s) {
                last_poll = Instant::now();
                let poll_recv = binutil::now_ns();
                let mut any_failed = false;
                for series in &config.series {
                    // Resume from the watermark (or the backfill window).
                    let start = match std::fs::read_to_string(watermark_of(series)) {
                        Ok(d) => Some(d.trim().to_owned()),
                        Err(_) => {
                            let days = config.backfill_days;
                            let secs = binutil::now_ns() / 1_000_000_000 - (days as i64) * 86_400;
                            Some(secs_to_date(secs))
                        }
                    };
                    match fetch_observations_blocking(series, &api_key, start.as_deref()) {
                        Ok(resp) => {
                            let observations = resp
                                .get("observations")
                                .cloned()
                                .unwrap_or(serde_json::Value::Array(vec![]));
                            let wrapped = serde_json::json!({
                                "series_id": series,
                                "observations": observations,
                            });
                            let payload = serde_json::to_vec(&wrapped)
                                .map_err(|e| format!("reserialize: {e}"))?;
                            let before = event_buffer.len();
                            // SAFETY: `wrapped` is a plain JSON value (CONV-13).
                            normalizer
                                .normalize(poll_recv, &payload, &mut event_buffer)
                                .map_err(|e| format!("fred normalize: {e}"))?;
                            any = true;
                            // Advance the watermark to the latest observation date.
                            if let Some(latest) = latest_observation_date(&resp) {
                                let _ = std::fs::write(watermark_of(series), format!("{latest}\n"));
                            }
                            tracing::info!(
                                series = %series,
                                recorded = event_buffer.len() - before,
                                "FRED observations recorded"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(series = %series, error = %e, "FRED fetch failed");
                            any_failed = true;
                        }
                    }
                }
                if any_failed {
                    let sym = normalizer
                        .symbols()
                        .lookup(Venue::Fred, "")
                        .unwrap_or(mp_core::SymbolId(0));
                    let gap = EventEnvelope::new(
                        Venue::Fred,
                        sym,
                        recv_ns,
                        recv_ns,
                        0,
                        MarketEvent::Status {
                            kind: StatusKind::GapDetected,
                            detail: "fred poll failed (MAC-4 gap surfaced)".to_string(),
                        },
                    );
                    event_buffer.push(gap);
                    any = true;
                }
            }

            if !event_buffer.is_empty() {
                let date = binutil::utc_date_str();
                if date != current_date {
                    if let Some(ref mut w) = log_writer {
                        let _ = w.flush();
                    }
                    let log_path = raw_dir.join(format!("{date}_fred_macro.log"));
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

    /// Extract the latest observation date (YYYY-MM-DD) from a FRED response.
    fn latest_observation_date(resp: &serde_json::Value) -> Option<String> {
        resp.get("observations")?
            .as_array()?
            .iter()
            .filter_map(|o| o.get("date")?.as_str().map(str::to_owned))
            .max()
    }

    /// `YYYY-MM-DD` for a unix timestamp (binary edge only).
    fn secs_to_date(secs: i64) -> String {
        let d = secs.div_euclid(86_400);
        let mut y = 1970i64;
        let mut rem = d;
        loop {
            let days_yr = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                366
            } else {
                365
            };
            if rem < days_yr {
                break;
            }
            rem -= days_yr;
            y += 1;
        }
        let months = [
            31,
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            },
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
        let mut m = 0i64;
        while m < 12 && rem >= months[m as usize] {
            rem -= months[m as usize];
            m += 1;
        }
        format!("{y:04}-{:02}-{:02}", m + 1, rem + 1)
    }
}
