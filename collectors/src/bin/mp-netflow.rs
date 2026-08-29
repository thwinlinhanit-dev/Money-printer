//! Ethereum exchange-reserve balance poller (spec 034, NFL-1..6). Polls
//! Etherscan's public API (free key via `MP_ETHERSCAN_KEY` env, PD-2) for the
//! token/ETH balances of configured exchange hot wallets and records each
//! snapshot as a `NetflowSnapshot` event to
//! `data/raw/{date}_netflow_ethereum.log`.
//!
//! Build/run:
//!   cargo run -p mp-collectors --features live-http --bin mp-netflow -- \
//!     --config collectors/netflow.toml.example
//!
//! Balances are DATA ONLY (NFL-6) — research derives netflows from deltas
//! (NFL-5). Addresses stay opaque identifiers; labels are poller config for
//! human readability (NFL-3).

fn main() {
    #[cfg(not(feature = "live-http"))]
    {
        eprintln!("Error: 'live-http' feature required (REST poller).");
        eprintln!(
            "  cargo run -p mp-collectors --features live-http --bin mp-netflow -- --config collectors/netflow.toml.example"
        );
        std::process::exit(1);
    }

    #[cfg(feature = "live-http")]
    {
        tracing_subscriber::fmt::init();
        if let Err(e) = impl_::run() {
            tracing::error!(error = %e, "netflow collector failed");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "live-http")]
mod impl_ {
    use mp_collectors::binutil::{self, InstanceLock, PidFile};
    use mp_collectors::etherscan::EtherscanNormalizer;
    use mp_collectors::json::str_field;
    use mp_collectors::netflow::{etherscan_action, parse_config, NetflowConfig, WatchEntry};
    use mp_collectors::normalize::Normalizer;
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, StatusKind};
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// Etherscan API **V2** — the V1 route (`/api`) was deprecated venue-side
    /// (2025-08 migration; V1 now answers `status=0 "deprecated"`), which
    /// surfaced as silent per-wallet fetch failures until probed directly.
    pub const ETHERSCAN_API_URL: &str = "https://api.etherscan.io/v2/api";
    /// Chain id for Ethereum mainnet (V2 routes all chains through one host).
    pub const ETHERSCAN_CHAIN_ID: u32 = 1;

    fn config_from_args(args: &[String]) -> Result<NetflowConfig, String> {
        let path = binutil::flag(args, "--config")
            .ok_or_else(|| String::from("netflow requires --config path"))?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("read netflow config {path}: {e}"))?;
        parse_config(&text).map_err(|e| format!("{e} (in {path})"))
    }

    /// `MP_ETHERSCAN_KEY` from the environment (absent/empty is an error —
    /// the poller must not silently run keyless, PD-2).
    fn api_key_from_env() -> Result<String, String> {
        match std::env::var("MP_ETHERSCAN_KEY") {
            Ok(k) if !k.is_empty() => Ok(k),
            _ => Err(String::from(
                "MP_ETHERSCAN_KEY env var is not set (free key from \
                 https://etherscan.io/apis)",
            )),
        }
    }

    /// Build one balance-query URL (V2 route, mainnet chain id; token
    /// balance when a contract is set, native ETH balance otherwise).
    pub(crate) fn build_balance_url(key: &str, entry: &WatchEntry) -> String {
        let mut url = format!(
            "{ETHERSCAN_API_URL}?chainid={chain}&module=account&action={action}&address={addr}&tag=latest&apikey={key}",
            chain = ETHERSCAN_CHAIN_ID,
            action = etherscan_action(&entry.contract),
            addr = entry.address,
        );
        if !entry.contract.is_empty() {
            url.push_str(&format!("&contractaddress={}", entry.contract));
        }
        url
    }

    /// Fetch one wallet's balance and return the wrapped payload the
    /// normalizer consumes: `{"address", "asset", "result"}`.
    fn fetch_balance_blocking(key: &str, entry: &WatchEntry) -> Result<serde_json::Value, String> {
        let url = build_balance_url(key, entry);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("etherscan request: {e}"))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| format!("etherscan body: {e}"))?;
        if !status.is_success() {
            return Err(format!("etherscan HTTP {status}: {text}"));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("etherscan json: {e}"))?;
        // Etherscan returns status=0 with an empty result on errors — surface
        // the message so the GapDetected carries a reason.
        if str_field(&v, "status") != Some("1") {
            let msg = str_field(&v, "message").unwrap_or("unknown");
            let result = str_field(&v, "result").unwrap_or("");
            return Err(format!("etherscan status 0 ({msg}): {result}"));
        }
        Ok(serde_json::json!({
            "address": entry.address,
            "asset": entry.asset,
            "result": str_field(&v, "result").unwrap_or(""),
        }))
    }

    /// Fetch one watch entry's balance and normalize it into snapshots.
    fn fetch_and_record(
        normalizer: &mut EtherscanNormalizer,
        key: &str,
        entry: &WatchEntry,
        recv_ns: i64,
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), String> {
        let wrapped = fetch_balance_blocking(key, entry)?;
        let payload = serde_json::to_vec(&wrapped).map_err(|e| format!("reserialize: {e}"))?;
        normalizer
            .normalize(recv_ns, &payload, out)
            .map_err(|e| format!("netflow normalize: {e}"))
    }

    fn census_detected(sym: mp_core::SymbolId, ts_ns: i64, wallets: usize) -> EventEnvelope {
        EventEnvelope::new(
            mp_core::Venue::Ethereum,
            sym,
            ts_ns,
            ts_ns,
            0,
            mp_core::MarketEvent::Status {
                kind: StatusKind::Census,
                detail: format!("netflow census: {wallets} wallets"),
            },
        )
    }

    fn gap_detected(sym: mp_core::SymbolId, ts_ns: i64, detail: String) -> EventEnvelope {
        EventEnvelope::new(
            mp_core::Venue::Ethereum,
            sym,
            ts_ns,
            ts_ns,
            0,
            mp_core::MarketEvent::Status {
                kind: StatusKind::GapDetected,
                detail,
            },
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
        let key = api_key_from_env()?;

        let mut normalizer = EtherscanNormalizer::new();
        let raw_dir = Path::new(&config.data_dir).join("raw");
        std::fs::create_dir_all(&raw_dir)?;
        let _instance_lock = InstanceLock::acquire(&raw_dir, "netflow_ethereum")?;
        let _pid_file = PidFile::write(&raw_dir, "netflow_ethereum")?;

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count = 0usize;
        let mut last_written_recv_ns: i64 = 0;
        let mut last_poll = Instant::now() - Duration::from_secs(config.poll_interval_s);
        let mut last_heartbeat = Instant::now();

        loop {
            let mut event_buffer: Vec<EventEnvelope> = Vec::new();
            let mut any = false;
            let recv_ns = binutil::now_ns();

            if last_heartbeat.elapsed() >= Duration::from_secs(15) {
                last_heartbeat = Instant::now();
                binutil::touch_heartbeat(&raw_dir, "netflow_ethereum");
            }

            // NFL-7: a failed poll is surfaced as Status::GapDetected — gaps
            // are data, never silently swallowed.
            let mut fail = false;

            if last_poll.elapsed() >= Duration::from_secs(config.poll_interval_s) {
                last_poll = Instant::now();
                let poll_recv = binutil::now_ns();
                let mut wallets = 0usize;
                for entry in &config.watchlist {
                    let before = event_buffer.len();
                    if let Err(e) =
                        fetch_and_record(&mut normalizer, &key, entry, poll_recv, &mut event_buffer)
                    {
                        tracing::warn!(
                            label = %entry.label,
                            address = %entry.address,
                            error = %e,
                            "netflow fetch failed"
                        );
                        fail = true;
                    } else {
                        wallets += event_buffer.len() - before;
                    }
                    if config.min_poll_gap_ms > 0 {
                        std::thread::sleep(Duration::from_millis(config.min_poll_gap_ms));
                    }
                    if last_heartbeat.elapsed() >= Duration::from_secs(15) {
                        last_heartbeat = Instant::now();
                        binutil::touch_heartbeat(&raw_dir, "netflow_ethereum");
                    }
                }
                // A completed poll is itself a data point: keep the raw log
                // fresh even when every wallet fetch returned empty.
                let sym = mp_core::SymbolId(0);
                event_buffer.push(census_detected(sym, recv_ns, wallets));
                any = true;
            }

            if fail {
                let sym = mp_core::SymbolId(0);
                event_buffer.push(gap_detected(sym, recv_ns, "netflow poll failed".into()));
                any = true;
            }

            if !event_buffer.is_empty() {
                let date = binutil::utc_date_str();
                if date != current_date {
                    if let Some(ref mut w) = log_writer {
                        let _ = w.flush();
                    }
                    let log_path = raw_dir.join(format!("{date}_netflow_ethereum.log"));
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
}

#[cfg(test)]
#[cfg(feature = "live-http")]
mod tests {
    use super::impl_::build_balance_url;
    use mp_collectors::netflow::{etherscan_action, WatchEntry};

    #[test]
    fn nfl_url_uses_v2_route_with_mainnet_chainid() {
        // Etherscan deprecated the V1 `/api` route (answers status=0
        // "deprecated"); the poller MUST hit /v2/api with a chain id or
        // every wallet fetch fails closed.
        let usdt = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
        let e = WatchEntry {
            label: "t".into(),
            address: "0xABC".into(),
            asset: "USDT".into(),
            contract: usdt.into(),
        };
        let url = build_balance_url("KEY", &e);
        assert!(url.starts_with("https://api.etherscan.io/v2/api"), "{url}");
        assert!(url.contains("chainid=1"), "{url}");
        assert!(
            url.contains("action=tokenbalance"),
            "token contract => tokenbalance"
        );
        assert!(url.contains(&format!("contractaddress={usdt}")));

        // Native ETH entry (empty contract) → plain balance action, no
        // contractaddress param.
        let eth = WatchEntry {
            label: "e".into(),
            address: "0xDEF".into(),
            asset: "ETH".into(),
            contract: String::new(),
        };
        let url = build_balance_url("K", &eth);
        assert!(url.contains(&format!("action={}", etherscan_action(""))));
        assert!(!url.contains("contractaddress"));
    }
}
