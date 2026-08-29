//! Acceptance tests for spec 029 (liquidation aggregation + estimated bands).
//! Test names embed requirement IDs (CONV-21). Local fixtures, no network (CONV-23).

use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};
use mp_features::config::{LeverageTier, LiqEstBandsParams};
use mp_features::liquidation::{band_accuracy, LiqAgg, LiqEstBands, WhaleBandStudy};
use mp_features::{
    calibrate_leverage_weights, tier_leverages, FeatureEngine, Locality, TickFeature,
};

const SEC: i64 = 1_000_000_000;
const SYM: SymbolId = SymbolId(0);

fn liq(venue: Venue, recv: i64, price: f64, qty: f64, side: Side) -> EventEnvelope {
    EventEnvelope::new(
        venue,
        SYM,
        recv,
        recv,
        0,
        MarketEvent::Liquidation { price, qty, side },
    )
}

fn mark(venue: Venue, recv: i64, m: f64) -> EventEnvelope {
    EventEnvelope::new(
        venue,
        SYM,
        recv,
        recv,
        0,
        MarketEvent::MarkPrice { mark: m, index: m },
    )
}

fn oi(venue: Venue, recv: i64, contracts: f64, notional: f64) -> EventEnvelope {
    EventEnvelope::new(
        venue,
        SYM,
        recv,
        recv,
        0,
        MarketEvent::OpenInterest {
            oi_contracts: contracts,
            oi_notional: notional,
        },
    )
}

fn est_defaults() -> LiqEstBandsParams {
    LiqEstBandsParams::default()
}

/// Spec-028-style fixture: a Hyperliquid whale position (opaque 0x address,
/// signed size, real liq price — NaN sentinel when the venue omits it).
fn whale(recv: i64, size: f64, liq_price: f64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        SYM,
        recv,
        recv,
        0,
        MarketEvent::WhalePosition {
            address: "0xdeadbeef".into(),
            size,
            entry: 100.0,
            leverage: 50.0,
            liq_price,
        },
    )
}

#[test]
fn liq_1_agg_merges_across_venues_and_marks_sampled() {
    let mut agg = LiqAgg::new(250_000_000, 5 * SEC); // 250ms dedup, 5s window
    assert!(
        agg.sampled(),
        "cross-venue tape marked sampled/interpolated (LIQ-1)"
    );
    // The same liquidation echoed on Bybit then BinanceFutures within 250ms at
    // the same price ⇒ de-duplicated (one event), not double-counted.
    assert_eq!(
        agg.on_event(&liq(Venue::Bybit, 100, 50000.0, 2.0, Side::Sell)),
        Some(-100_000.0)
    );
    assert_eq!(
        agg.on_event(&liq(Venue::BinanceFutures, 150, 50000.0, 2.0, Side::Sell)),
        None,
        "cross-venue echo within dedup window de-duplicated (LIQ-1)"
    );
    // A distinct liquidation (different time + price, Okx) IS counted.
    assert_eq!(
        agg.on_event(&liq(Venue::Okx, 400, 50010.0, 3.0, Side::Buy)),
        Some(-100_000.0 + 150_030.0)
    );
}

#[test]
fn liq_2_est_bands_compute_from_oi_funding_leverage_and_mark_estimated() {
    let p = est_defaults();
    let mut b = LiqEstBands::new(p.maintenance_buffer, p.leverage_tiers.clone());
    assert!(b.estimated(), "bands are a model, not ground truth (LIQ-2)");
    // Mark alone ⇒ silent (no OI yet).
    assert_eq!(b.on_event(&mark(Venue::Bybit, 1, 100.0)), None);
    // Mark=100 + OI ⇒ estimate emits the fraction above the nearest long liq
    // level: for the max tier (50x), drop = (1 − 1/(50·buffer))/50.
    let mmr = 1.0 / (50.0 * p.maintenance_buffer);
    let drop = (1.0 - mmr) / 50.0;
    let got = b
        .on_event(&oi(Venue::Bybit, 2, 1000.0, 100_000.0))
        .expect("emits on OI once mark is known");
    assert!((got - drop).abs() < 1e-9, "frac {got} ≈ drop {drop}");
    // Full bands + at-risk exposed for RES-4 offline validation (LIQ-2/LIQ-6).
    assert!((b.long_liq_level() - 100.0 * (1.0 - drop)).abs() < 1e-6);
    assert!(b.short_liq_level() > 100.0);
    assert!(b.notional_at_risk() > 0.0);
}

#[test]
fn liq_3_features_are_deterministic() {
    fn run() -> Vec<f64> {
        let mut e = FeatureEngine::new(SEC);
        let p = est_defaults();
        let buffer = p.maintenance_buffer;
        let tiers = p.leverage_tiers;
        e.register_tick(|| Box::new(LiqAgg::new(250_000_000, 5 * SEC)))
            .register_tick(move || Box::new(LiqEstBands::new(buffer, tiers.clone())));
        let events: Vec<EventEnvelope> = vec![
            liq(Venue::Bybit, 100, 50000.0, 2.0, Side::Sell),
            liq(Venue::Okx, 400, 50010.0, 3.0, Side::Buy),
            mark(Venue::Bybit, 500, 100.0),
            oi(Venue::Bybit, 600, 1000.0, 100_000.0),
            liq(Venue::Bybit, 700, 50020.0, 1.0, Side::Sell),
            liq(Venue::BinanceFutures, 720, 50020.0, 1.0, Side::Sell),
        ];
        let mut out = Vec::new();
        for ev in &events {
            for u in e.on_event(ev) {
                if u.value.is_finite() {
                    out.push(u.value);
                }
            }
        }
        out
    }
    // Same events, two fresh engines ⇒ identical value stream (LIQ-3 golden).
    assert_eq!(run(), run());
}

#[test]
fn liq_4_catalog_registration_and_locality() {
    let mut e = FeatureEngine::new(SEC);
    let p = est_defaults();
    let buffer = p.maintenance_buffer;
    let tiers = p.leverage_tiers;
    e.register_tick(|| Box::new(LiqAgg::new(250_000_000, SEC)))
        .register_tick(move || Box::new(LiqEstBands::new(buffer, tiers.clone())));
    // LIQ-4/FEA-9: both are runnable online (no offline-only feature).
    assert!(e.offline_only_features().is_empty());
    assert_eq!(LiqEstBands::new(1.5, Vec::new()).locality(), Locality::Both);
    assert_eq!(LiqAgg::new(0, 0).locality(), Locality::Both);
    // FEA-5: OI before any mark ⇒ recompute is None (no emit, never NaN).
    let ups = e.on_event(&oi(Venue::Bybit, 1, 1000.0, 100_000.0));
    assert!(ups.iter().all(|u| u.value.is_finite()));
    assert_eq!(e.nan_suppressed(), 0);
}
#[test]
fn liq_5_no_new_event_variant() {
    // The features derive from existing Liquidation / OpenInterest / MarkPrice
    // events only — THIS spec added no event variant (LIQ-5). The schema was
    // amended to 3 by the owner-approved specs 028/030/031 (WhalePosition /
    // MacroPoint / Option* variants, CONV-20), never by 029: the only 2→3
    // additions are those append-only variants. Schema 4 (2026-08-18) came
    // from owner-approved specs 033/034 (TradeWithAddr / NetflowSnapshot /
    // Venue::Ethereum), never from 029. Schema 5 (2026-08-22) came from
    // owner-approved spec 040 (Venue::Cboe for the IBIT options chain).
    // Schema 6 (2026-08-26) came from owner-approved specs 046/047
    // (Venue::DeFiLlama + Venue::Coinalyze appended, CONV-20).
    // Guard the exact version so a future amendment updates this test
    // deliberately.
    assert_eq!(mp_core::SCHEMA_VER, 6);
}

#[test]
fn liq_6_est_bands_validated_against_hyperliquid_ground_truth() {
    // RES-4 offline event study (LIQ-6): WhaleBandStudy replays recorded
    // Hyperliquid mark/OI into the LiqEstBands state and pairs every spec 028
    // WhalePosition real liq price with the sign-aware model estimate. One
    // deterministic 50x tier + buffer 1.5 keeps the expected levels exact.
    let buffer = 1.5;
    let tiers = vec![LeverageTier {
        leverage: 50.0,
        weight: 1.0,
    }];
    let mut study = WhaleBandStudy::new(buffer, tiers.clone());
    // No mark/OI yet ⇒ no estimate ⇒ a position records nothing.
    assert_eq!(study.on_event(&whale(0, 2.0, 97.0)), None);
    // Hyperliquid mark+OI seed the per-symbol band state.
    assert_eq!(study.on_event(&mark(Venue::Hyperliquid, 1, 100.0)), None);
    assert_eq!(
        study.on_event(&oi(Venue::Hyperliquid, 2, 1000.0, 100_000.0)),
        None
    );
    let mmr = 1.0 / (50.0 * buffer);
    let drop = (1.0 - mmr) / 50.0;
    let long_est = 100.0 * (1.0 - drop);
    let short_est = 100.0 * (1.0 + drop);
    // Sign-aware pairing: longs → long_liq_level (downside), shorts →
    // short_liq_level (upside). on_event returns the new observation index.
    assert_eq!(study.on_event(&whale(3, 2.0, 97.0)), Some(0)); // long
    assert_eq!(study.on_event(&whale(4, 2.0, 99.0)), Some(1)); // long
    assert_eq!(study.on_event(&whale(5, -1.5, 103.0)), Some(2)); // short
    assert_eq!(study.on_event(&whale(6, -1.5, 100.5)), Some(3)); // short
    let obs = study.observations();
    assert_eq!(obs.len(), 4);
    for o in &obs[..2] {
        assert!(o.is_long);
        assert!((o.estimate - long_est).abs() < 1e-9, "long estimate exact");
    }
    for o in &obs[2..] {
        assert!(!o.is_long);
        assert!(
            (o.estimate - short_est).abs() < 1e-9,
            "short estimate exact"
        );
    }
    assert_eq!(obs[0].realized, 97.0);
    assert_eq!(obs[3].realized, 100.5);
    // Coverage: long est <= real (97 → 98.03 ✗, 99 → 98.03 ✓); short est >= real
    // (103 → 101.97 ✗, 100.5 → 101.97 ✓) ⇒ 2/4, never NaN.
    let total = study.accuracy();
    assert_eq!(total.n, 4);
    assert_eq!(total.coverage, 0.5);
    assert!(total.mean_relative_error > 0.0 && total.mean_relative_error.is_finite());
    // Long side is definitionally band_accuracy on the same pairs (LIQ-6 reuse).
    let pairs: Vec<(f64, f64)> = obs[..2].iter().map(|o| (o.estimate, o.realized)).collect();
    let (mre, cov) = band_accuracy(&pairs);
    let long = study.long_accuracy();
    assert_eq!(long.n, 2);
    assert_eq!(long.coverage, cov);
    assert!((long.mean_relative_error - mre).abs() < 1e-12);
    assert_eq!(study.short_accuracy().n, 2);
    // Fail-closed (CONV-8): NaN liq price (venue omitted), zero size, and NaN
    // size (corrupt — never classified as a short) record nothing; a position
    // on an unseeded symbol records nothing.
    assert_eq!(study.on_event(&whale(7, 2.0, f64::NAN)), None);
    assert_eq!(study.on_event(&whale(8, 0.0, 95.0)), None);
    assert_eq!(study.on_event(&whale(9, f64::NAN, 95.0)), None);
    assert_eq!(study.n_observations(), 4);
    let mut unseeded = WhaleBandStudy::new(buffer, tiers);
    assert_eq!(unseeded.on_event(&whale(1, 2.0, 97.0)), None);
    // Non-Hyperliquid mark/OI never feed the state: same events, Bybit venue ⇒
    // no estimate ⇒ nothing recorded (same-venue ground truth, not a mix).
    let mut cross_venue = WhaleBandStudy::new(
        buffer,
        vec![LeverageTier {
            leverage: 50.0,
            weight: 1.0,
        }],
    );
    assert_eq!(cross_venue.on_event(&mark(Venue::Bybit, 1, 100.0)), None);
    assert_eq!(
        cross_venue.on_event(&oi(Venue::Bybit, 2, 1000.0, 100_000.0)),
        None
    );
    assert_eq!(cross_venue.on_event(&whale(3, 2.0, 97.0)), None);
    // Deterministic (LIQ-3/CONV-9): identical event stream ⇒ identical
    // observations and identical accuracy.
    let events: Vec<EventEnvelope> = vec![
        mark(Venue::Hyperliquid, 1, 100.0),
        oi(Venue::Hyperliquid, 2, 1000.0, 100_000.0),
        whale(3, 2.0, 97.0),
        whale(5, -1.5, 103.0),
    ];
    let mut a = WhaleBandStudy::new(
        buffer,
        vec![LeverageTier {
            leverage: 50.0,
            weight: 1.0,
        }],
    );
    let mut b = WhaleBandStudy::new(
        buffer,
        vec![LeverageTier {
            leverage: 50.0,
            weight: 1.0,
        }],
    );
    for ev in &events {
        a.on_event(ev);
        b.on_event(ev);
    }
    assert_eq!(a.observations(), b.observations());
    assert_eq!(a.accuracy(), b.accuracy());
    // Empty study ⇒ zeros, never NaN (fail-closed, CONV-8).
    let empty = WhaleBandStudy::new(buffer, vec![]);
    assert_eq!(empty.accuracy(), Default::default());
    assert_eq!(band_accuracy(&[]), (0.0, 0.0));
}

#[test]
fn liq_7_check_config_rejects_unknown_fields() {
    use mp_features::FeaturesConfig;
    // LIQ-7/CONV-16: an unknown top-level key is an error, never a silent default.
    assert!(FeaturesConfig::from_toml("liq_agg_bogus = 1").is_err());
    let cfg = FeaturesConfig::from_toml(
        "[liq_agg]\ndedup_window_ns = 100000000\n[liq_est_bands]\nmaintenance_buffer = 2.0\n",
    )
    .unwrap();
    assert_eq!(cfg.liq_agg.dedup_window_ns, 100_000_000);
    assert_eq!(cfg.liq_est_bands.maintenance_buffer, 2.0);
    assert_eq!(cfg.liq_est_bands.leverage_tiers.len(), 6); // default tier set when unspecified
                                                           // The shipped example config parses — validates the [liq_agg] and
                                                           // [liq_est_bands] sections incl. the nested leverage_tiers array-of-tables.
    let ex = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("features.toml.example");
    let example_toml = std::fs::read_to_string(&ex).expect("example config present");
    let parsed = FeaturesConfig::from_toml(&example_toml).unwrap();
    assert_eq!(
        parsed.liq_est_bands.leverage_tiers.len(),
        6,
        "default tier set in example"
    );
    assert_eq!(parsed.liq_agg.agg_window_ns, 1_000_000_000);
}

#[test]
fn liq_9_no_new_network_dependency() {
    // LIQ-9/PD-4: the features read recorded events via the engine only — no
    // network dependency in the features crate.
    let manifest = include_str!("../Cargo.toml");
    for net in [
        "reqwest",
        "hyper",
        "tokio-tungstenite",
        "tungstenite",
        "ureq",
    ] {
        assert!(
            !manifest.contains(net),
            "features must not depend on {net} (LIQ-9)"
        );
    }
}

// ---- LIQ-11: leverage-tier weight calibration from spec 028 -------------

#[test]
fn liq_11_leverage_weights_calibrate_from_spec_028_notional_distribution() {
    // Default tier set {1,2,5,10,20,50}. Geometric-midpoint boundaries:
    //   sqrt(2)=1.4142, sqrt(10)=3.1623, sqrt(50)=7.0711, sqrt(200)=14.1421,
    //   sqrt(1000)=31.6228.
    let tiers = tier_leverages(&LiqEstBandsParams::default().leverage_tiers);
    assert_eq!(tiers, vec![1.0, 2.0, 5.0, 10.0, 20.0, 50.0]);

    // (leverage, notional=|size|·entry) samples from spec 028 positions:
    //   lev 2   → tier 2   (2.0 < sqrt(10))        notional 200
    //   lev 5   → tier 5   (3.16 < 5.0 < 7.07)     notional 100
    //   lev 10  → tier 10  (7.07 < 10.0 < 14.14)   notional 100
    //   lev 100 → tier 50  (100.0 > 31.62)         notional 100
    let samples = [(2.0, 200.0), (5.0, 100.0), (10.0, 100.0), (100.0, 100.0)];
    let cal = calibrate_leverage_weights(&samples, &tiers);

    // Total notional 500 ⇒ weights are notional SHARES: 0.4 / 0.2 / 0.2 / 0.2.
    let by_lev: std::collections::BTreeMap<u64, &mp_features::LeverageTierCalibration> =
        cal.iter().map(|c| (c.leverage as u64, c)).collect();
    assert_eq!(by_lev.len(), 6, "one bucket per tier, sorted ascending");
    assert_eq!(by_lev[&2].count, 1);
    assert_eq!(by_lev[&2].notional, 200.0);
    assert!((by_lev[&2].weight - 0.4).abs() < 1e-12);
    for &t in &[5.0, 10.0, 50.0] {
        assert!((by_lev[&(t as u64)].weight - 0.2).abs() < 1e-12, "tier {t}");
    }
    assert_eq!(by_lev[&1].count, 0);
    assert_eq!(by_lev[&1].weight, 0.0);
    assert_eq!(by_lev[&20].count, 0);
    // Σ weights ≈ 1 (the model's OI-spread invariant).
    let sum: f64 = cal.iter().map(|c| c.weight).sum();
    assert!((sum - 1.0).abs() < 1e-12);
}

#[test]
fn liq_11_leverage_weights_boundary_ties_fail_closed_and_deterministic() {
    let tiers = tier_leverages(&LiqEstBandsParams::default().leverage_tiers);

    // Boundary samples bucket to the HIGHER tier (conservative, closer to
    // liquidation): sqrt(10) ≈ 3.1623 is the 2/5 boundary ⇒ tier 5;
    // sqrt(2) ≈ 1.4142 is the 1/2 boundary ⇒ tier 2.
    let cal =
        calibrate_leverage_weights(&[(10.0_f64.sqrt(), 100.0), (2.0_f64.sqrt(), 100.0)], &tiers);
    let by_lev: std::collections::BTreeMap<u64, &mp_features::LeverageTierCalibration> =
        cal.iter().map(|c| (c.leverage as u64, c)).collect();
    assert_eq!(by_lev[&5].count, 1, "boundary sample → higher tier");
    assert_eq!(by_lev[&2].count, 1);
    assert_eq!(by_lev[&5].weight, 0.5);

    // Notional weighting (not count): one huge low-leverage position weighs
    // more than three small high-leverage ones.
    let samples = [(2.0, 300.0), (10.0, 100.0), (10.0, 100.0), (10.0, 100.0)];
    let cal = calibrate_leverage_weights(&samples, &tiers);
    let by_lev: std::collections::BTreeMap<u64, &mp_features::LeverageTierCalibration> =
        cal.iter().map(|c| (c.leverage as u64, c)).collect();
    assert_eq!(by_lev[&2].count, 1);
    assert_eq!(by_lev[&10].count, 3);
    assert!((by_lev[&2].weight - 0.5).abs() < 1e-12, "300/(300+300)");
    assert!((by_lev[&10].weight - 0.5).abs() < 1e-12);

    // Fail-closed (CONV-8): NaN/inf leverage, NaN/zero/negative notional,
    // and non-positive leverage are skipped — never counted.
    let dirty = [
        (f64::NAN, 100.0),
        (10.0, f64::NAN),
        (10.0, 0.0),
        (10.0, -50.0),
        (-2.0, 100.0),
        (f64::INFINITY, 100.0),
        (2.0, 100.0),
    ];
    let cal = calibrate_leverage_weights(&dirty, &tiers);
    let by_lev: std::collections::BTreeMap<u64, &mp_features::LeverageTierCalibration> =
        cal.iter().map(|c| (c.leverage as u64, c)).collect();
    assert_eq!(
        by_lev[&2].count, 1,
        "only the clean (2.0, 100.0) sample counts"
    );
    assert_eq!(by_lev[&2].weight, 1.0);

    // Order independence (CONV-9/10): shuffling samples changes nothing.
    let a = calibrate_leverage_weights(&[(2.0, 200.0), (10.0, 100.0), (100.0, 100.0)], &tiers);
    let b = calibrate_leverage_weights(&[(100.0, 100.0), (2.0, 200.0), (10.0, 100.0)], &tiers);
    assert_eq!(a, b);

    // Empty samples or empty tiers ⇒ all-zero / empty, never NaN.
    let empty = calibrate_leverage_weights(&[], &tiers);
    assert!(empty.iter().all(|c| c.weight == 0.0 && c.count == 0));
    assert!(calibrate_leverage_weights(&[(2.0, 100.0)], &[]).is_empty());
    // Single tier: everything lands in it.
    let one = calibrate_leverage_weights(&[(2.0, 100.0), (100.0, 50.0)], &[50.0]);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].leverage, 50.0);
    assert_eq!(one[0].weight, 1.0);
    assert_eq!(one[0].count, 2);
    // Unsorted + duplicate tier leverages canonicalize to [1, 10, 50].
    let dedup = calibrate_leverage_weights(&[(5.0, 100.0)], &[50.0, 10.0, 10.0, 1.0]);
    assert_eq!(
        dedup.iter().map(|c| c.leverage).collect::<Vec<_>>(),
        vec![1.0, 10.0, 50.0]
    );
}

#[test]
fn liq_11c_rendered_calibration_section_parses_as_features_config() {
    // The EXACT `[liq_est_bands]` section the research job renders
    // (research/calibrate_leverage.py render_toml) must parse as a
    // features.toml (LIQ-7 deny_unknown_fields): the operator's calibrated
    // override is consumable by the SAME parser the live model uses (FEA-4).
    // Zero-weight tiers are kept on purpose — a tier's level still shapes the
    // nearest cascade level even at weight 0 (LIQ-2).
    let rendered = r#"[liq_est_bands]
# Calibrated from recorded spec 028 real leverage distribution (spec 029 LIQ-11):
# n=4 positions, total_notional=500.0, config_hash=deadbeefdeadbeef
maintenance_buffer = 1.5
[[liq_est_bands.leverage_tiers]]
leverage = 1.0
weight = 0.0
[[liq_est_bands.leverage_tiers]]
leverage = 2.0
weight = 0.4
[[liq_est_bands.leverage_tiers]]
leverage = 5.0
weight = 0.2
[[liq_est_bands.leverage_tiers]]
leverage = 10.0
weight = 0.2
[[liq_est_bands.leverage_tiers]]
leverage = 20.0
weight = 0.0
[[liq_est_bands.leverage_tiers]]
leverage = 50.0
weight = 0.2
"#;
    let cfg =
        mp_features::FeaturesConfig::from_toml(rendered).expect("rendered section parses (LIQ-7)");
    let tiers = &cfg.liq_est_bands.leverage_tiers;
    assert_eq!(tiers.len(), 6);
    assert!((cfg.liq_est_bands.maintenance_buffer - 1.5).abs() < 1e-12);
    assert!((tiers[1].weight - 0.4).abs() < 1e-12);
    assert!((tiers[5].weight - 0.2).abs() < 1e-12);
    // And that parsed set is exactly what the model would be calibrated on
    // (one-code-path): re-calibrating the same samples yields the same weights.
    let samples = [(2.0, 200.0), (5.0, 100.0), (10.0, 100.0), (100.0, 100.0)];
    let cal = calibrate_leverage_weights(&samples, &tier_leverages(tiers));
    assert!((cal[1].weight - 0.4).abs() < 1e-12);
    assert!((cal[3].weight - 0.2).abs() < 1e-12);
}

proptest::proptest! {
    #[test]
    fn liq_8_proptest_band_math(
        m in 100.0_f64..1_000_000.0,
        lev in 1.0_f64..200.0,
        oi_notional in 1.0_f64..1e9,
        buffer in 1.0_f64..4.0,
    ) {
        // For any valid (mark, leverage, OI, buffer): the emitted fraction is
        // finite and in [0,1]; the long level lies strictly between 0 and mark
        // (inclusive at the degenerate lev=1/buffer=1 corner). No NaN/inf can
        // escape (CONV-8); higher leverage ⇒ nearer level holds by construction.
        let mut b = LiqEstBands::new(buffer, vec![LeverageTier { leverage: lev, weight: 1.0 }]);
        let _ = b.on_event(&mark(Venue::Bybit, 1, m));
        let out = b.on_event(&oi(Venue::Bybit, 2, 1.0, oi_notional));
        if let Some(f) = out {
            assert!(f.is_finite());
            assert!((0.0..=1.0).contains(&f));
            assert!(b.long_liq_level().is_finite());
            assert!(b.long_liq_level() > 0.0 && b.long_liq_level() <= m);
            assert!(b.notional_at_risk() >= 0.0);
        }
        // Fail-closed: no mark yet ⇒ no emission (never NaN), and no OI ⇒ none.
        let mut c = LiqEstBands::new(buffer, vec![LeverageTier { leverage: lev, weight: 1.0 }]);
        assert!(c.on_event(&oi(Venue::Bybit, 2, 1.0, oi_notional)).is_none());
        assert!(c.on_event(&mark(Venue::Bybit, 3, m)).is_none());
    }
}
