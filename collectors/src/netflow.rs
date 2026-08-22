//! Netflow poller configuration + Etherscan route selection (spec 034,
//! NFL-2 / NFL-7).
//!
//! The poll cadence and per-wallet pacing are the Etherscan free-tier
//! rate-limit controls (NFL-7); the `balance` vs `tokenbalance` action tells
//! the poller which Etherscan endpoint a watch entry needs (NFL-2). Both live
//! here — not in the `mp-netflow` binary — so they are unit-testable without
//! the `live-http` feature (PD-4 keeps the strategy/feature path free of
//! network; this module is a collector-side config, never a decision input).
//!
//! Config is TOML with `serde(deny_unknown_fields)` (CONV-16): a typo is a
//! startup error, never a silently-ignored option. The API key comes from the
//! `MP_ETHERSCAN_KEY` environment variable only (PD-2) — never from config.

use serde::Deserialize;

/// Etherscan action for native ETH balance (no ERC-20 contract).
pub const ETHERSCAN_ACTION_BALANCE: &str = "balance";
/// Etherscan action for an ERC-20 token balance (with a contract address).
pub const ETHERSCAN_ACTION_TOKENBALANCE: &str = "tokenbalance";

/// Default poll cadence in seconds (NFL-7; the example config documents 300s).
pub const DEFAULT_POLL_INTERVAL_S: u64 = 300;
/// Default pacing between per-wallet fetches, in milliseconds.
pub const DEFAULT_MIN_POLL_GAP_MS: u64 = 250;

/// Etherscan `action` for a watch entry: native ETH `balance` vs ERC-20
/// `tokenbalance` (NFL-2). Empty `contract` ⇒ `balance`; otherwise
/// `tokenbalance`. The contract address, when present, is appended to the
/// request as `contractaddress` by the poller.
pub fn etherscan_action(contract: &str) -> &'static str {
    if contract.is_empty() {
        ETHERSCAN_ACTION_BALANCE
    } else {
        ETHERSCAN_ACTION_TOKENBALANCE
    }
}

/// One watched exchange wallet (NFL-3).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchEntry {
    /// Human label for logs only (never recorded in events — NFL-3).
    #[serde(default)]
    pub label: String,
    /// Opaque exchange wallet address.
    pub address: String,
    /// Asset symbol, e.g. "USDT" — becomes the envelope symbol.
    pub asset: String,
    /// ERC-20 contract address; empty = native ETH balance.
    #[serde(default)]
    pub contract: String,
}

/// Netflow poller configuration (NFL-7).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetflowConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// Poll cadence for all watch entries (default 300s).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_s: u64,
    /// Pacing between per-wallet fetches (Etherscan rate limits).
    #[serde(default = "default_min_poll_gap_ms")]
    pub min_poll_gap_ms: u64,
    /// Watch wallets. Empty = poll nothing but stay alive (a fresh config is
    /// not a dead collector).
    #[serde(default)]
    pub watchlist: Vec<WatchEntry>,
}

fn default_data_dir() -> String {
    "data".to_owned()
}
fn default_poll_interval() -> u64 {
    DEFAULT_POLL_INTERVAL_S
}
fn default_min_poll_gap_ms() -> u64 {
    DEFAULT_MIN_POLL_GAP_MS
}

/// Parse + validate the TOML config (deny_unknown_fields fail-closed, CONV-16).
/// A zero `poll_interval_s` would hot-loop Etherscan — refuse it (NFL-7).
pub fn parse_config(text: &str) -> Result<NetflowConfig, String> {
    let cfg: NetflowConfig =
        toml::from_str(text).map_err(|e| format!("parse netflow config: {e}"))?;
    if cfg.poll_interval_s == 0 {
        return Err("netflow config requires poll_interval_s > 0".into());
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_config() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/netflow.toml.example"))
            .expect("netflow.toml.example must ship with the crate")
    }

    #[test]
    fn nfl_2_balance_vs_tokenbalance_route() {
        assert_eq!(etherscan_action(""), ETHERSCAN_ACTION_BALANCE);
        assert_eq!(
            etherscan_action("0xdAC17F958D2ee523a2206206994597C13D831ec7"),
            ETHERSCAN_ACTION_TOKENBALANCE
        );
        // Native ETH has no contract; USDT on Ethereum mainnet does.
        assert_eq!(etherscan_action(""), "balance");
        assert_eq!(
            etherscan_action("0xdAC17F958D2ee523a2206206994597C13D831ec7"),
            "tokenbalance"
        );
    }

    #[test]
    fn nfl_7_example_config_parses_with_cadence_defaults() {
        let cfg = parse_config(&example_config()).expect("checked-in example must parse");
        assert_eq!(cfg.poll_interval_s, DEFAULT_POLL_INTERVAL_S);
        assert_eq!(cfg.min_poll_gap_ms, DEFAULT_MIN_POLL_GAP_MS);
        assert_eq!(cfg.data_dir, "data");
        assert!(
            cfg.watchlist.is_empty(),
            "example watchlist ships empty (operator fills it)"
        );
    }

    #[test]
    fn nfl_7_cadence_defaults_apply_when_fields_absent() {
        let cfg = parse_config("data_dir = \"data\"\n").unwrap();
        assert_eq!(cfg.poll_interval_s, DEFAULT_POLL_INTERVAL_S);
        assert_eq!(cfg.min_poll_gap_ms, DEFAULT_MIN_POLL_GAP_MS);
    }

    #[test]
    fn nfl_7_zero_cadence_is_rejected() {
        assert!(parse_config("poll_interval_s = 0\n").is_err());
    }

    #[test]
    fn nfl_7_unknown_fields_are_rejected_fail_closed() {
        assert!(parse_config("poll_interval_s = 300\nbogus_key = true\n").is_err());
    }
}
