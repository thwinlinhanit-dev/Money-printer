//! Config↔code sync (CONV-16) + spec 042 engine wiring (WCG-7).
//!
//! Pins `features.toml.example` against `FeaturesConfig` (deny_unknown_fields
//! means a drifted example FAILS here, never in production), and proves
//! `engine_from_config` registers the full four-family cohort set when
//! `[cohort] enabled = true`.

use mp_core::{EventEnvelope, MarketEvent, SymbolId, Venue};
use mp_features::config::FeaturesConfig;
use mp_features::engine_from_config;

const EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/features.toml.example"
));

fn parse_example() -> FeaturesConfig {
    toml::from_str(EXAMPLE).expect("features.toml.example must parse against FeaturesConfig")
}

#[test]
fn cfg_example_stays_in_sync_with_structs() {
    let cfg = parse_example();
    // Spec 042/043/045 sections present with documented defaults.
    assert!(!cfg.cohort.enabled);
    assert_eq!(
        cfg.cohort.inner.smart_flow_window_ns, 86_400_000_000_000,
        "smart-flow window default is 24h"
    );
    assert_eq!(cfg.cohort.inner.snapshot_max_age_ns, 7 * 86_400_000_000_000);
    assert!(!cfg.netflow_flow.enabled);
    assert_eq!(cfg.netflow_flow.inner.stale_after_ns, 600_000_000_000);
    assert!(!cfg.accumulation.enabled);
    assert_eq!(cfg.accumulation.inner.cooldown_ns, 4 * 3_600_000_000_000);
}

#[test]
fn wcg_7_engine_registers_all_four_cohort_families_when_enabled() {
    let mut cfg = parse_example();
    cfg.cohort.enabled = true;
    // No snapshot_path: registration happens regardless; emissions stay
    // fail-closed (WCG-8) until an operator provides one — asserted below.
    let mut e = engine_from_config(&cfg).expect("config must build");
    // Any event triggers global instantiation + name interning.
    let ev = EventEnvelope::new(
        Venue::Hyperliquid,
        SymbolId(9),
        41 * 86_400_000_000_000,
        41 * 86_400_000_000_000,
        1,
        MarketEvent::WhalePosition {
            address: "0xdeadbeef".into(),
            size: 100.0,
            entry: 50.0,
            leverage: 2.0,
            liq_price: f64::NAN,
        },
    );
    e.on_event(&ev);
    for name in [
        "cohort.whale_ratio",
        "cohort.net_delta.smart_money",
        "cohort.net_delta.whale",
        "cohort.net_delta.retail",
        "cohort.net_delta.dormant",
        "cohort.smart_flow.24h",
        "cohort.concentration",
    ] {
        assert!(e.name_to_id(name).is_some(), "{name} must be registered");
    }
}

#[test]
fn wcg_8_engine_without_snapshot_emits_nothing_cohort_family() {
    let mut cfg = parse_example();
    cfg.cohort.enabled = true;
    let mut e = engine_from_config(&cfg).expect("config must build");
    let ev = EventEnvelope::new(
        Venue::Hyperliquid,
        SymbolId(9),
        41 * 86_400_000_000_000,
        41 * 86_400_000_000_000,
        1,
        MarketEvent::WhalePosition {
            address: "0xdeadbeef".into(),
            size: 100.0,
            entry: 50.0,
            leverage: 2.0,
            liq_price: f64::NAN,
        },
    );
    let updates = e.on_event(&ev);
    assert!(
        updates.iter().all(|u| !u.name.starts_with("cohort.")),
        "no snapshot loaded → every cohort.* emission suppressed (WCG-8)"
    );
}

#[test]
fn wcg_12_cohort_features_enter_signal_catalog_at_hypothesis_stage() {
    use mp_features::signal_catalog::{SignalCatalog, SignalStage};

    // WCG-12: the cohort family must live in the signal catalog (spec 025)
    // at the Hypothesis stage — promotion to Tested needs the RES-4 event
    // study (n >= 30, positive expectancy), never a default. The catalog
    // vocabulary is pinned to the exact ids the engine emits (WCG-7).
    let ids = [
        "cohort.whale_ratio",
        "cohort.net_delta.smart_money",
        "cohort.net_delta.whale",
        "cohort.net_delta.retail",
        "cohort.net_delta.dormant",
        "cohort.smart_flow.24h",
        "cohort.concentration",
    ];
    let mut cat = SignalCatalog::new();
    for id in ids {
        let rec = cat
            .register(
                id,
                format!("{id}: wallet-cohort aggregate predicts forward returns"),
                "wcg-v1",
            )
            .unwrap_or_else(|e| panic!("{id} must register in a fresh catalog: {e}"));
        assert_eq!(
            rec.stage,
            SignalStage::Hypothesis,
            "{id} enters at Hypothesis (WCG-12)"
        );
    }
    // One catalog entry per signal: a duplicate id is refused.
    assert!(cat.register(ids[0], "duplicate", "dup").is_err());
}
