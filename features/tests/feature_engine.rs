//! Acceptance tests for spec 004. Test names embed requirement IDs (CONV-21).

use mp_core::{EventEnvelope, MarketEvent, Side, SnapshotReason, SymbolId, Venue};
use mp_features::catalog::*;
use mp_features::{Cond, FeatureEngine, Op, Rule, Screener};
use smallvec::smallvec;

const SEC: i64 = 1_000_000_000;

fn trade(recv: i64, price: f64, qty: f64, side: Side) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        recv,
        recv,
        0,
        MarketEvent::Trade {
            price,
            qty,
            side,
            trade_id: 0,
        },
    )
}

fn engine_with_all() -> FeatureEngine {
    let mut e = FeatureEngine::new(SEC); // 1s bars
    e.register_tick(|| Box::new(Cvd::new("bybit")))
        .register_tick(|| Box::new(WhalePrint::new(100_000.0)))
        .register_tick(|| Box::new(OiDelta::new()))
        .register_tick(|| Box::new(FundingPassthrough::new("bybit")))
        .register_bar(|| Box::new(BarDelta::new("1s")))
        .register_bar(|| Box::new(RealizedVol::new("1s", 2)))
        .register_bar(|| Box::new(DonchianBreakout::new(3)));
    e
}

fn value(ups: &[mp_features::FeatureUpdate], feat: &str) -> Option<f64> {
    ups.iter()
        .rev()
        .find(|u| u.feature == feat)
        .map(|u| u.value)
}

#[test]
fn fea_1_cvd_accumulates_signed_volume() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(Cvd::new("bybit")));
    let u1 = e.on_event(&trade(1, 100.0, 2.0, Side::Buy));
    assert_eq!(value(&u1, "cvd.bybit"), Some(2.0));
    let u2 = e.on_event(&trade(2, 100.0, 0.5, Side::Sell));
    assert_eq!(value(&u2, "cvd.bybit"), Some(1.5));
}

fn trade_hl(recv: i64, price: f64, qty: f64, side: Side) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        SymbolId(0),
        recv,
        recv,
        0,
        MarketEvent::Trade {
            price,
            qty,
            side,
            trade_id: 0,
        },
    )
}

#[test]
fn fea_1_whale_print_thresholds_notional() {
    let mut e = FeatureEngine::new(SEC);
    // Product default: Hyperliquid whale tracker.
    e.register_tick(|| Box::new(WhalePrint::new(100_000.0)));
    // Bybit trade must not emit on the HL-scoped feature.
    assert!(e.on_event(&trade(1, 100.0, 2000.0, Side::Sell)).is_empty());
    // 100 * 500 = 50k < 100k → no emit.
    assert!(e.on_event(&trade_hl(1, 100.0, 500.0, Side::Buy)).is_empty());
    // 100 * 2000 = 200k ≥ 100k, sell → negative signed notional.
    let u = e.on_event(&trade_hl(2, 100.0, 2000.0, Side::Sell));
    assert_eq!(value(&u, "whale_print.hyperliquid"), Some(-200_000.0));
}

#[test]
fn fea_1_liq_cluster_sums_within_window() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(LiqCluster::new(10 * SEC, 1_000_000.0)));
    let liq = |recv: i64, price: f64, qty: f64, side: Side| {
        EventEnvelope::new(
            Venue::Bybit,
            SymbolId(0),
            recv,
            recv,
            0,
            MarketEvent::Liquidation { price, qty, side },
        )
    };
    // First long liquidation (Sell side): 600k notional, below threshold.
    assert!(e.on_event(&liq(1, 30_000.0, 20.0, Side::Sell)).is_empty());
    // Second within window: +600k more ⇒ 1.2M ≥ threshold, negative (longs).
    let u = e.on_event(&liq(2, 30_000.0, 20.0, Side::Sell));
    assert_eq!(value(&u, "liq.cluster"), Some(-1_200_000.0));
}

#[test]
fn fea_1_oi_delta_and_funding_passthrough() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(OiDelta::new()))
        .register_tick(|| Box::new(FundingPassthrough::new("bybit")));
    let oi = |recv: i64, oi: f64| {
        EventEnvelope::new(
            Venue::Bybit,
            SymbolId(0),
            recv,
            recv,
            0,
            MarketEvent::OpenInterest {
                oi_contracts: oi,
                oi_notional: f64::NAN,
            },
        )
    };
    assert!(e.on_event(&oi(1, 1000.0)).is_empty()); // first reading: no delta
    let u = e.on_event(&oi(2, 1050.0));
    assert_eq!(value(&u, "oi.delta"), Some(50.0));

    let f = EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        3,
        3,
        0,
        MarketEvent::Funding {
            rate: 0.0001,
            interval_s: 0,
            next_funding_ts_ns: 0,
        },
    );
    assert_eq!(value(&e.on_event(&f), "funding.bybit"), Some(0.0001));
}

#[test]
fn fea_1_bar_delta_on_close() {
    let mut e = FeatureEngine::new(SEC);
    e.register_bar(|| Box::new(BarDelta::new("1s")));
    // Two trades in bucket 0, then one in bucket 1 closes bar 0.
    e.on_event(&trade(0, 100.0, 3.0, Side::Buy));
    e.on_event(&trade(SEC / 2, 100.0, 1.0, Side::Sell));
    let u = e.on_event(&trade(SEC + 1, 100.0, 1.0, Side::Buy)); // closes bar 0
    assert_eq!(value(&u, "delta.bar.1s"), Some(2.0)); // 3 buy - 1 sell
}

#[test]
fn fea_3_realized_vol_and_breakout_warmup_suppressed() {
    let mut e = engine_with_all();
    // Feed trades across 5 buckets so 4 bars close (closes happen on the trade
    // that opens the next bucket).
    let mut all = Vec::new();
    for i in 0..6 {
        all.extend(e.on_event(&trade(i * SEC + 1, 100.0 + i as f64, 1.0, Side::Buy)));
    }
    // RealizedVol(w=2) warms only after 2 returns; breakout(n=3) after 3 bars.
    // With 5 closed bars there should be at least one rv and one breakout value.
    assert!(value(&all, "vol.rv.1s.2").is_some(), "rv should warm");
    assert!(value(&all, "breakout.3").is_some(), "breakout should warm");
}

#[test]
fn fea_3_no_output_before_warmup() {
    let mut e = FeatureEngine::new(SEC);
    e.register_bar(|| Box::new(DonchianBreakout::new(3)));
    // Only 2 bars close (3 buckets) → breakout still cold → no breakout updates.
    let mut ups = Vec::new();
    for i in 0..3 {
        ups.extend(e.on_event(&trade(i * SEC + 1, 100.0, 1.0, Side::Buy)));
    }
    assert!(value(&ups, "breakout.3").is_none());
}

#[test]
fn fea_4_online_offline_identity() {
    // The "offline" run consumes an owned Vec; the "online" run feeds the same
    // events one at a time. Identical output ⇒ one-code-path guarantee.
    let events: Vec<EventEnvelope> = (0..20)
        .map(|i| {
            trade(
                i * SEC / 3 + 1,
                100.0 + (i % 5) as f64,
                1.0 + (i % 3) as f64,
                if i % 2 == 0 { Side::Buy } else { Side::Sell },
            )
        })
        .collect();

    let mut offline = engine_with_all();
    let a = offline.run(events.iter());

    let mut online = engine_with_all();
    let mut b = Vec::new();
    for e in &events {
        b.extend(online.on_event(e));
    }
    assert_eq!(a, b, "online and offline must produce identical updates");
    assert!(!a.is_empty());
}

#[test]
fn fea_8_book_feature_silent_while_stale() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(BookImbalance::new()));
    let snap = EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        1,
        1,
        100,
        MarketEvent::BookSnapshot {
            bids: smallvec![(100.0, 6.0)],
            asks: smallvec![(101.0, 2.0)],
            seq: 100,
            depth: 2,
            reason: SnapshotReason::Init,
        },
    );
    let u = e.on_event(&snap);
    // imbalance = (6-2)/(6+2) = 0.5
    assert_eq!(value(&u, "imbalance.top"), Some(0.5));

    // A gap delta makes the book stale → feature must go silent (FEA-8).
    let gap = EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        2,
        2,
        105,
        MarketEvent::BookDelta {
            bids: smallvec![(100.0, 9.0)],
            asks: smallvec![],
            first_seq: 105,
            last_seq: 105,
        },
    );
    assert!(value(&e.on_event(&gap), "imbalance.top").is_none());
}

#[test]
fn fea_10_screener_edge_triggers_with_snapshot() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(Cvd::new("bybit")));
    let mut screener = Screener::new(vec![Rule {
        id: "cvd_breakout".into(),
        conds: vec![Cond {
            feature: "cvd.bybit".into(),
            op: Op::Ge,
            threshold: 5.0,
        }],
    }]);

    let mut hits = Vec::new();
    for u in e.on_event(&trade(1, 100.0, 3.0, Side::Buy)) {
        hits.extend(screener.on_update(&u)); // cvd=3, below 5
    }
    assert!(hits.is_empty());
    for u in e.on_event(&trade(2, 100.0, 4.0, Side::Buy)) {
        hits.extend(screener.on_update(&u)); // cvd=7 ≥ 5 → fire
    }
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rule_id, "cvd_breakout");
    assert_eq!(hits[0].snapshot.get("cvd.bybit"), Some(&7.0));

    // Still true next tick → edge-triggered, no re-fire.
    let mut hits2 = Vec::new();
    for u in e.on_event(&trade(3, 100.0, 1.0, Side::Buy)) {
        hits2.extend(screener.on_update(&u)); // cvd=8, still ≥5
    }
    assert!(hits2.is_empty(), "edge-triggered: fires once");
}

// ---- FEA-7: catalog config (features.toml) --------------------------------

#[test]
fn fea_7_features_toml_parses_and_rejects_unknown_keys() {
    use mp_features::FeaturesConfig;
    let toml = r#"
        bar_tf_ns = 60000000000
        [cvd]
        venues = ["bybit", "okx"]
        [whale_print]
        min_notional = 300000.0
        [liq_cluster]
        window_ns = 30000000000
        min_cluster_notional = 4000000.0
    "#;
    let cfg = FeaturesConfig::from_toml(toml).unwrap();
    assert_eq!(cfg.cvd.venues, vec!["bybit", "okx"]);
    assert_eq!(cfg.whale_print.min_notional, 300000.0);

    // A typo'd key is a hard error (deny_unknown_fields), never a silent default.
    let bad = r#"
        [whale_print]
        min_notionl = 300000.0
    "#;
    assert!(FeaturesConfig::from_toml(bad).is_err());
}

#[test]
fn fea_6_params_hash_is_canonical_and_change_sensitive() {
    use mp_features::FeaturesConfig;
    // Formatting / whitespace differences do NOT change the hash (canonical:
    // parse-normalize then hash, not a raw-text hash).
    let a =
        FeaturesConfig::from_toml("bar_tf_ns = 60000000000\n[cvd]\nvenues=[\"bybit\"]").unwrap();
    let b =
        FeaturesConfig::from_toml("bar_tf_ns   =   60000000000\n\n[cvd]\nvenues = [ \"bybit\" ]\n")
            .unwrap();
    assert_eq!(a.params_hash().unwrap(), b.params_hash().unwrap());
    // A real param change DOES change the hash (⇒ forces a new ver=N, FEA-6).
    let c = FeaturesConfig::from_toml("[whale_print]\nmin_notional = 999999.0").unwrap();
    assert_ne!(a.params_hash().unwrap(), c.params_hash().unwrap());
}

// ---- FEA-5: NaN validate/suppress/count/WARN -------------------------------

#[test]
fn fea_5_non_finite_outputs_are_suppressed_and_counted() {
    use mp_features::TickFeature;
    struct NanFeature;
    impl TickFeature for NanFeature {
        fn id(&self) -> String {
            "nan.test".into()
        }
        fn on_event(&mut self, _ev: &mp_core::EventEnvelope) -> Option<f64> {
            Some(f64::NAN)
        }
    }
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(NanFeature));
    let ups = e.on_event(&trade(1, 100.0, 1.0, Side::Buy));
    // The NaN never reaches downstream (fail-closed), and the suppression is
    // COUNTED — visible to the ops layer, not silent (FEA-5/CONV-8).
    assert!(ups.iter().all(|u| u.value.is_finite()));
    assert!(!ups.iter().any(|u| u.feature == "nan.test"));
    assert_eq!(e.nan_suppressed(), 1);
}

// ---- FEA-9: offline-only features are refused on the live path -------------

#[test]
fn fea_9_offline_only_features_are_flagged_for_live_refusal() {
    use mp_features::{Locality, TickFeature};
    struct LeadLag;
    impl TickFeature for LeadLag {
        fn id(&self) -> String {
            "leadlag.bybit.okx".into()
        }
        fn locality(&self) -> Locality {
            Locality::Offline
        }
        fn on_event(&mut self, _ev: &mp_core::EventEnvelope) -> Option<f64> {
            None
        }
    }
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new("bybit"))); // online-capable
    e.register_tick(|| Box::new(LeadLag)); // offline-only
                                           // The live runner MUST call this after registration and refuse to start
                                           // if it is non-empty (FEA-9): leadlag never runs on the live path.
    assert_eq!(
        e.offline_only_features(),
        vec!["leadlag.bybit.okx".to_string()]
    );
    let mut clean = FeatureEngine::new(1_000_000_000);
    clean.register_tick(|| Box::new(Cvd::new("bybit")));
    assert!(clean.offline_only_features().is_empty());
}

// ---- FEA-2: as-of ordering is a prefix property -----------------------------

#[test]
fn fea_2_updates_for_a_prefix_equal_the_prefix_of_updates() {
    // As-of discipline: the updates produced by events[..k] are EXACTLY the
    // first part of the updates produced by the full sequence — no feature can
    // peek at an event that hasn't arrived (FEA-2/PD-3, structural).
    let events: Vec<EventEnvelope> = (0..30)
        .map(|i| {
            trade(
                i * SEC / 2 + 1,
                100.0 + (i % 5) as f64,
                1.0 + (i % 3) as f64,
                if i % 2 == 0 { Side::Buy } else { Side::Sell },
            )
        })
        .collect();
    let mut full_engine = engine_with_all();
    let full = full_engine.run(events.iter());
    for k in [1usize, 7, 15, 29] {
        let mut prefix_engine = engine_with_all();
        let prefix = prefix_engine.run(events[..k].iter());
        assert_eq!(
            prefix.as_slice(),
            &full[..prefix.len()],
            "prefix k={k} must be a prefix of the full run"
        );
    }
}
