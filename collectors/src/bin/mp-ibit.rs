//! IBIT options-chain poller (spec 040, IBI-1..9). Polls CBOE's free delayed
//! quotes endpoint (HTTP REST, ~15-min tape lag, no auth) and records each
//! chain snapshot as OptionTicker/OptionTrade events (venue=Cboe,
//! underlying=IBIT) to `{data_dir}/raw/cboe/{date}/`.
//!
//! Build/run:
//!   cargo run -p mp-collectors --features live-http --bin mp-ibit -- \
//!     --config collectors/ibit.toml.example
//!
//! Runs as its OWN process (IBI-9): a slow CBOE poll cycle can never block
//! or delay the Deribit real-time WebSocket collector (spec 031).
//!
//! Parse-canary (IBI-1 review decision): a poll whose quotes array is
//! non-empty but yields zero parsed contracts is schema drift — surfaced as
//! `Status::GapDetected`, never a clean empty recording.
//!
//! Verbatim raw capture (COL-9): every poll's response body is appended to
//! `raw/cboe/{date}/ibit_chain.ndjson` (one JSON document per line;
//! uncompressed .ndjson in v1 — same decision as spec 031).

fn main() {
    #[cfg(not(feature = "live-http"))]
    {
        eprintln!("Error: 'live-http' feature required (REST poller).");
        eprintln!(
            "  cargo run -p mp-collectors --features live-http --bin mp-ibit -- --config collectors/ibit.toml.example"
        );
        std::process::exit(1);
    }

    #[cfg(feature = "live-http")]
    {
        tracing_subscriber::fmt::init();
        if let Err(e) = impl_::run() {
            tracing::error!(error = %e, "ibit collector failed");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "live-http")]
mod impl_ {
    use mp_collectors::binutil::{self, InstanceLock, PidFile};
    use mp_collectors::ibit::{parse_canary, parse_config, quotes_present, IbitConfig};
    use mp_collectors::normalize::Normalizer;
    use mp_collectors::staleness::Staleness;
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, StatusKind};
    use std::io::Write;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn config_from_args(args: &[String]) -> Result<IbitConfig, String> {
        let path = binutil::flag(args, "--config")
            .ok_or_else(|| String::from("ibit requires --config path"))?;
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("read ibit config {path}: {e}"))?;
        parse_config(&text).map_err(|e| format!("{e} (in {path})"))
    }

    /// One blocking GET of the delayed chain; returns the raw body.
    fn fetch_chain_blocking(url: &str) -> Result<Vec<u8>, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .get(url)
            .send()
            .map_err(|e| format!("cboe get: {e}"))?;
        let status = resp.status();
        let body = resp
            .bytes()
            .map_err(|e| format!("cboe body: {e}"))?
            .to_vec();
        if !status.is_success() {
            return Err(format!("cboe HTTP {status}: {} bytes", body.len()));
        }
        Ok(body)
    }

    fn status_event(
        sym: mp_core::SymbolId,
        ts_ns: i64,
        kind: StatusKind,
        detail: String,
    ) -> EventEnvelope {
        EventEnvelope::new(
            mp_core::Venue::Cboe,
            sym,
            ts_ns,
            ts_ns,
            0,
            mp_core::MarketEvent::Status { kind, detail },
        )
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

        let mut normalizer = mp_collectors::ibit::CboeChainNormalizer::new();
        let raw_dir = Path::new(&config.data_dir).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let _instance_lock = InstanceLock::acquire(&raw_dir, "ibit_cboe")?;
        let _pid_file = PidFile::write(&raw_dir, "ibit_cboe")?;

        // COL-2 staleness watchdog: no fresh chain for 3× the poll cadence
        // (min 60s) means the tape — or our fetch — went dark.
        let stale_threshold_ns =
            (config.poll_interval_s.saturating_mul(3).max(60)) as i64 * 1_000_000_000;
        let mut staleness = Staleness::new(stale_threshold_ns);

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut raw_writer: Option<std::fs::File> = None;
        let mut last_symbol_count = 0usize;
        let mut last_written_recv_ns: i64 = 0;
        let mut last_poll = Instant::now() - Duration::from_secs(config.poll_interval_s);
        let mut last_heartbeat = Instant::now();

        loop {
            let mut event_buffer: Vec<EventEnvelope> = Vec::new();
            let recv_ns = binutil::now_ns();

            if last_heartbeat.elapsed() >= Duration::from_secs(15) {
                last_heartbeat = Instant::now();
                binutil::touch_heartbeat(&raw_dir, "ibit_cboe");
            }

            if last_poll.elapsed() >= Duration::from_secs(config.poll_interval_s) {
                last_poll = Instant::now();
                match fetch_chain_blocking(&config.url) {
                    Ok(body) => {
                        // COL-9: verbatim frame FIRST — a parse failure can
                        // never destroy the raw evidence.
                        if config.raw_capture {
                            if let Some(ref mut w) = raw_writer {
                                let _ = w.write_all(&body);
                                let _ = w.write_all(b"\n");
                                let _ = w.flush();
                            }
                        }
                        let had_quotes = quotes_present(&body);
                        let before = event_buffer.len();
                        match normalizer.normalize(recv_ns, &body, &mut event_buffer) {
                            Ok(()) => {
                                let parsed = event_buffer.len() - before;
                                if parse_canary(had_quotes, parsed) {
                                    // IBI-1: schema drift is DATA, not silence.
                                    event_buffer.push(status_event(
                                        mp_core::SymbolId(0),
                                        recv_ns,
                                        StatusKind::GapDetected,
                                        format!(
                                            "ibit parse canary: quotes present but 0 contracts parsed ({} bytes)",
                                            body.len()
                                        ),
                                    ));
                                } else {
                                    staleness.observe("chain", recv_ns);
                                    event_buffer.push(status_event(
                                        mp_core::SymbolId(0),
                                        recv_ns,
                                        StatusKind::Census,
                                        format!("ibit chain snapshot: {parsed} events"),
                                    ));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "ibit normalize failed");
                                event_buffer.push(status_event(
                                    mp_core::SymbolId(0),
                                    recv_ns,
                                    StatusKind::GapDetected,
                                    format!("ibit normalize failed: {e}"),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ibit fetch failed");
                        event_buffer.push(status_event(
                            mp_core::SymbolId(0),
                            recv_ns,
                            StatusKind::GapDetected,
                            format!("ibit fetch failed: {e}"),
                        ));
                    }
                }
                // COL-2: report any stream that has gone stale.
                for topic in staleness.stale_streams(binutil::now_ns()) {
                    event_buffer.push(status_event(
                        mp_core::SymbolId(0),
                        recv_ns,
                        StatusKind::Stale,
                        format!("ibit stream stale: {topic}"),
                    ));
                }
            }
            // @@WRITE@@
        }
    }
}
