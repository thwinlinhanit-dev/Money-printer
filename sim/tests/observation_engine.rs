//! Research-lab observation engine — integration tests (spec 054).
//!
//! The full vertical slice the hardening requires: RAW events → feature
//! engine → strategy (signal) → observation → forward outcomes, through the
//! PRODUCTION backtester (SIM-5). Plus reproducibility, no-lookahead, and a
//! golden fixture hash.
//!
//! Requirement ids: REL-8 (immutable observations), REL-9 (quality-gated
//! recording), REL-10 (determinism), REL-12/13 (outcomes), REL-14
//! (no-lookahead).

use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};
use mp_features::catalog::Cvd;
use mp_features::FeatureEngine;
use mp_sim::{Backtester, SimConfig};
use mp_strategies::CoinFlipStrategy;

const MS: i64 = 1_000_000;
/// Fixed deterministic instant (2026-07-19T00:00:00Z) — replay-constant.
const T0: i64 = 1_784_505_600_000_000_000;

fn trade(recv: i64, price: f64, qty: f64, side: Side) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        recv,
        recv,
        recv as u64,
        MarketEvent::Trade {
            price,
            qty,
            side,
            trade_id: recv as u64,
        },
    )
}

/// A trade feed spanning `n` events, 100ms apart, prices 100..103 (wavy).
fn feed(n: usize) -> Vec<EventEnvelope> {
    (0..n)
        .map(|i| {
            trade(
                i as i64 * 100 * MS,
                100.0 + (i % 7) as f64 * 0.5,
                1.0,
                if i % 2 == 0 { Side::Buy } else { Side::Sell },
            )
        })
        .collect()
}

fn engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit)));
    e
}

/// One deterministic instrumented run: CoinFlip on cvd.bybit, observations on.
fn run(n_events: usize, seed: u64) -> Backtester {
    let mut bt = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig {
            latency_ns: 50 * MS,
            ..SimConfig::default()
        },
        seed,
    );
    bt.enable_observations("params-hash-test".into(), 1, T0);
    bt.run(&feed(n_events)).unwrap();
    bt
}

#[test]
fn rel_8_raw_to_outcome_integration() {
    let mut bt = run(60, 42);
    let obs = bt.observations();
    assert!(!obs.is_empty(), "the coinflip signal must produce observations");
    // Quality gate: fires before 5 samples are blocked, recorded after.
    assert!(bt.blocked_observations() > 0, "cold-start fires must be blocked");
    for o in obs {
        assert_eq!(o.identity.signal_id, "coinflip", "identity is the signal id");
        assert_eq!(o.identity.params_hash, "params-hash-test");
        assert!(
            o.feature_snapshot.contains_key("cvd.bybit"),
            "snapshot carries the feature that caused the fire: {:?}",
            o.feature_snapshot
        );
        assert!(o.outcomes.is_empty(), "outcomes attach AFTER the run");
        assert_eq!(o.identity.data_schema_version, mp_core::SCHEMA_VER);
    }
    // Attach 1s + 2s horizons post-run: events late enough in the 6s feed
    // have closed windows; the earliest do not (never fabricated).
    bt.attach_outcomes(&[1_000_000_000, 2_000_000_000]);
    let with_outcome = bt
        .observations()
        .iter()
        .filter(|o| !o.outcomes.is_empty())
        .count();
    assert!(with_outcome > 0, "closed windows must produce outcomes");
    assert!(
        with_outcome < bt.observations().len(),
        "open windows must NOT be fabricated into outcomes"
    );
    for o in bt.observations() {
        for oc in &o.outcomes {
            assert!(oc.gross_return.is_finite() && oc.net_return.is_finite());
            // Net is gross minus the round-trip cost (taker + maker fee).
            assert!(oc.net_return <= oc.gross_return + 1e-12);
            assert!(oc.entry_price > 0.0 && oc.exit_price > 0.0);
        }
    }
}

#[test]
fn rel_10_reproducibility_same_inputs_same_observations() {
    let mut a = run(60, 7);
    let mut b = run(60, 7);
    a.attach_outcomes(&[1_000_000_000]);
    b.attach_outcomes(&[1_000_000_000]);
    let ja = serde_json::to_string(a.observations()).unwrap();
    let jb = serde_json::to_string(b.observations()).unwrap();
    assert_eq!(ja, jb, "identical replays ⇒ byte-identical observations+outcomes");
    // A different seed ⇒ different coin flips ⇒ different observations.
    let mut c = run(60, 8);
    c.attach_outcomes(&[1_000_000_000]);
    assert_ne!(
        serde_json::to_string(c.observations()).unwrap(),
        ja,
        "a different seed changes the coin flips ⇒ different observations"
    );
}

#[test]
fn rel_14_no_lookahead_horizon_beyond_series_is_never_fabricated() {
    let mut bt = run(60, 3);
    // A horizon far beyond the end of the recorded series: NOTHING may get an
    // outcome — an open window is never turned into a fabricated return.
    bt.attach_outcomes(&[10_000 * 86_400_000_000_000]); // ~10,000 days
    assert!(
        bt.observations().iter().all(|o| o.outcomes.is_empty()),
        "an outcome window beyond the series end must stay empty"
    );
}

#[test]
fn rel_14_recorder_is_write_only_decision_log_unchanged() {
    // The recorder must never perturb the decision log (golden determinism):
    // the same run with and without observations hashes identically.
    let mut plain = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig {
            latency_ns: 50 * MS,
            ..SimConfig::default()
        },
        99,
    );
    plain.run(&feed(60)).unwrap();

    let mut instrumented = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig {
            latency_ns: 50 * MS,
            ..SimConfig::default()
        },
        99,
    );
    instrumented.enable_observations("p".into(), 1, T0);
    instrumented.run(&feed(60)).unwrap();

    assert_eq!(
        plain.decision_log().hash(),
        instrumented.decision_log().hash(),
        "observation recording must not change the decision log"
    );
    assert!(!instrumented.observations().is_empty());
}

/// Golden fixture (REL-10/CONV-12): the FNV-1a hash of the serialized
/// observation set (with 1s outcomes attached) for a FIXED feed and seed.
/// If this changes, the observation/outcome semantics changed — intentionally
/// or not. Regenerate by printing the hash in a throwaway run after an
/// intentional change.
#[test]
fn rel_10_golden_observation_hash_is_stable() {
    let mut bt = run(40, 1234);
    bt.attach_outcomes(&[1_000_000_000]);
    let json = serde_json::to_string(bt.observations()).unwrap();
    let hash = mp_core::fnv1a_64_str(&json);
    // Frozen 2026-09-04 after the REL-13 series-coverage guard: a fixed feed
    // + seed must always produce this exact observation set hash.
    const GOLDEN: u64 = 18245435220829170398;
    assert_eq!(hash, GOLDEN, "golden observation hash changed");
}

/// Golden DIRTY fixture (REL-28): the observation engine driven through the
/// quality-vocabulary's failure modes in a fixed order — insufficient
/// history (cold start), a >staleness-window GAP, and a NaN (invalid) value
/// — alongside a healthy segment. Asserts blocked fires are counted, non-
/// Healthy observations never exist, and the whole state hash is stable.
#[test]
fn rel_28_golden_dirty_fixture_gaps_invalid_insufficient_history() {
    use mp_features::data_quality::DataQualityState;
    use mp_features::observation::ObservationEngine;
    use mp_features::signal_identity::SignalResearchIdentity;
    use mp_features::FeatureUpdate;
    use std::collections::BTreeMap;

    const HOUR: i64 = 3_600_000_000_000;
    fn upd(sym: SymbolId, val: f64, ts: i64) -> FeatureUpdate {
        FeatureUpdate {
            symbol: sym,
            feature: SymbolId(9),
            name: "funding.rate".into(),
            venue: Venue::Bybit,
            value: val,
            ts_ns: ts,
            ver: 1,
        }
    }
    let btc = SymbolId(0);
    let mut e = ObservationEngine::with_default_quality(
        SignalResearchIdentity::new("dirty_fixture", 1, "p", "c"),
        T0,
    );
    let mut states: Vec<&'static str> = Vec::new();
    let mut recorded: Vec<mp_features::SignalObservation> = Vec::new();
    let mut blocked = 0u64;
    let mut fire = |e: &mut ObservationEngine,
                    states: &mut Vec<&'static str>,
                    recorded: &mut Vec<_>,
                    blocked: &mut u64,
                    ts: i64| {
        states.push(match e.quality(btc, ts) {
            DataQualityState::Healthy => "Healthy",
            DataQualityState::InsufficientHistory => "InsufficientHistory",
            DataQualityState::Stale => "Stale",
            DataQualityState::Invalid => "Invalid",
            DataQualityState::Missing => "Missing",
            DataQualityState::Gap => "Gap",
        });
        match e.record(
            ts,
            btc,
            Venue::Bybit,
            mp_features::observation::Direction::Long,
            BTreeMap::from([("funding.rate".to_string(), 0.0001)]),
        ) {
            Some(o) => recorded.push(o),
            None => *blocked += 1,
        }
    };

    // 1. Cold start: 3 samples ⇒ InsufficientHistory ⇒ fire blocked.
    for i in 0..3 {
        e.on_feature_update(&upd(btc, 0.0001, T0 + i));
    }
    fire(&mut e, &mut states, &mut recorded, &mut blocked, T0 + 3);
    // 2. Enough history ⇒ Healthy ⇒ recorded.
    for i in 3..5 {
        e.on_feature_update(&upd(btc, 0.0001, T0 + i));
    }
    fire(&mut e, &mut states, &mut recorded, &mut blocked, T0 + 5);
    // 3. A 4-hour hole through the 1h staleness window ⇒ the resumption
    //    sample flags a GAP ⇒ fire blocked.
    e.on_feature_update(&upd(btc, 0.0001, T0 + 4 * HOUR));
    fire(&mut e, &mut states, &mut recorded, &mut blocked, T0 + 4 * HOUR + 1);
    // 4. min_samples fresh post-resumption observations heal the Gap ⇒
    //    recorded again.
    for i in 1..=5 {
        e.on_feature_update(&upd(btc, 0.0001, T0 + 4 * HOUR + i));
    }
    fire(&mut e, &mut states, &mut recorded, &mut blocked, T0 + 4 * HOUR + 6);
    // 5. A NaN poisons the stream ⇒ Invalid ⇒ fire blocked, forever.
    e.on_feature_update(&upd(btc, f64::NAN, T0 + 4 * HOUR + 7));
    fire(&mut e, &mut states, &mut recorded, &mut blocked, T0 + 4 * HOUR + 8);

    // The dirty path is exactly as designed: two recorded observations,
    // three blocked fires, and the state walk Missing→…→Gap→Invalid.
    assert_eq!(recorded.len(), 2);
    assert_eq!(blocked, 3);
    assert!(recorded
        .iter()
        .all(|o| o.quality == DataQualityState::Healthy));
    assert_eq!(
        states,
        vec![
            "InsufficientHistory",
            "Healthy",
            "Gap",
            "Healthy",
            "Invalid"
        ]
    );
    // Frozen golden hash over the recorded observations + blocked count +
    // state walk: any semantic change to the quality machinery shows here.
    let artifact = serde_json::json!({
        "recorded": recorded,
        "blocked": blocked,
        "states": states,
    });
    let hash = mp_core::fnv1a_64_str(&artifact.to_string());
    // Frozen 2026-09-07 with the full 6-state quality vocabulary (Missing/Gap
    // added): fixed dirty feed ⇒ this exact recorded-set + state-walk hash.
    const GOLDEN_DIRTY: u64 = 17618162958652494096;
    assert_eq!(hash, GOLDEN_DIRTY, "golden dirty-fixture hash changed");
}