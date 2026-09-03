//! Tests for Zero-Cost Mode collector configs (docs/ZERO_COST_MODE.md).
//!
//! Verifies that the TOML configs in `collectors/zero_cost/` parse correctly,
//! produce the expected stream set (trades + activeAssetCtx, no l2Book), and
//! carry the free-tier backoff tuning.

use std::path::Path;

/// Parse a TOML collector config file and return its key fields.
fn parse_collector_config(path: &Path) -> toml::Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    text.parse::<toml::Value>()
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

#[test]
fn zero_cost_btc_config_parses() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("zero_cost");
    let config = parse_collector_config(&dir.join("hyperliquid-btc.toml"));

    assert_eq!(config["venue"].as_str().unwrap(), "hyperliquid");
    assert_eq!(config["symbol"].as_str().unwrap(), "BTC");
    assert!(config["swing_only"].as_bool().unwrap());
    assert_eq!(config["backpressure"].as_str().unwrap(), "drop_oldest");
    assert_eq!(config["channel_capacity"].as_integer().unwrap(), 10_000);
}

#[test]
fn zero_cost_eth_config_parses() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("zero_cost");
    let config = parse_collector_config(&dir.join("hyperliquid-eth.toml"));

    assert_eq!(config["venue"].as_str().unwrap(), "hyperliquid");
    assert_eq!(config["symbol"].as_str().unwrap(), "ETH");
    assert!(config["swing_only"].as_bool().unwrap());
    assert_eq!(config["backpressure"].as_str().unwrap(), "drop_oldest");
    assert_eq!(config["channel_capacity"].as_integer().unwrap(), 10_000);
}

#[test]
fn zero_cost_configs_have_free_tier_backoff() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("zero_cost");

    for name in &["hyperliquid-btc.toml", "hyperliquid-eth.toml"] {
        let config = parse_collector_config(&dir.join(name));
        let base = config["backoff_base_ms"]
            .as_integer()
            .expect("backoff_base_ms must be set");
        let cap = config["backoff_cap_ms"]
            .as_integer()
            .expect("backoff_cap_ms must be set");
        assert!(
            base >= 500,
            "{name}: free-tier backoff_base_ms must be >= 500, got {base}"
        );
        assert!(
            cap >= 60_000,
            "{name}: free-tier backoff_cap_ms must be >= 60000, got {cap}"
        );
    }
}

/// Verify that zero_cost configs are a strict subset of the swing configs
/// (same venue/symbol/capacity/backpressure) but with different backoff tuning.
#[test]
fn zero_cost_configs_match_swing_except_backoff() {
    let zero_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("zero_cost");
    let swing_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("swing");

    for (zc_name, swing_name) in &[
        ("hyperliquid-btc.toml", "hyperliquid-btc.toml"),
        ("hyperliquid-eth.toml", "hyperliquid-eth.toml"),
    ] {
        let zc = parse_collector_config(&zero_dir.join(zc_name));
        let sw = parse_collector_config(&swing_dir.join(swing_name));

        // Core fields must match
        assert_eq!(zc["venue"], sw["venue"], "{zc_name} venue mismatch");
        assert_eq!(zc["symbol"], sw["symbol"], "{zc_name} symbol mismatch");
        assert_eq!(
            zc["swing_only"], sw["swing_only"],
            "{zc_name} swing_only mismatch"
        );
        assert_eq!(
            zc["channel_capacity"], sw["channel_capacity"],
            "{zc_name} channel_capacity mismatch"
        );
        assert_eq!(
            zc["backpressure"], sw["backpressure"],
            "{zc_name} backpressure mismatch"
        );

        // Backoff: zero_cost must have it, swing must not (uses defaults)
        assert!(
            zc["backoff_base_ms"].as_integer().is_some(),
            "{zc_name}: zero_cost must have backoff_base_ms"
        );
        assert!(
            sw.get("backoff_base_ms").is_none(),
            "{swing_name}: swing config must NOT have backoff_base_ms (uses defaults)"
        );
    }
}

/// The zero-cost stream set for Hyperliquid must be trades + activeAssetCtx
/// (no l2Book). This is verified via the `swing_only` flag which the
/// collector's `hl_channels()` function uses to drop l2Book.
#[test]
fn zero_cost_configs_drop_book_via_swing_only() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("zero_cost");

    for name in &["hyperliquid-btc.toml", "hyperliquid-eth.toml"] {
        let config = parse_collector_config(&dir.join(name));
        assert!(
            config["swing_only"].as_bool().unwrap(),
            "{name}: swing_only must be true to drop l2Book under Zero-Cost"
        );
    }
}
