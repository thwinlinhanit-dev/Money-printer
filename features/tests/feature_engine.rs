//! Acceptance tests for spec 004. Test names embed requirement IDs (CONV-21).

use mp_core::{EventEnvelope, MarketEvent, Side, SnapshotReason, SymbolId, Venue};
use mp_features::catalog::*;
use mp_features::{
    Cond, FeatureEngine, FeatureUpdate, LiqDelta, Op, Rule, Screener, WhaleNet, WhaleNetDelta,
};
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
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit)))
        .register_tick(|| Box::new(WhalePrint::new(100_000.0)))
        .register_tick(|| Box::new(OiDelta::new()))
        .register_tick(|| Box::new(FundingPassthrough::new("bybit")))
        .register_bar(|| Box::new(BarDelta::new("1s")))
        .register_bar(|| Box::new(RealizedVol::new("1s", 2)))
        .register_bar(|| Box::new(DonchianBreakout::new(3)));
    e
}

fn value(engine: &FeatureEngine, ups: &[FeatureUpdate], feat: &str) -> Option<f64> {
    let id = engine.name_to_id(feat)?;
    ups.iter().rev().find(|u| u.feature == id).map(|u| u.value)
}

#[test]
fn fea_1_cvd_accumulates_signed_volume() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit)));
    let u1 = e.on_event(&trade(1, 100.0, 2.0, Side::Buy));
    assert_eq!(value(&e, &u1, "cvd.bybit"), Some(2.0));
    let u2 = e.on_event(&trade(2, 100.0, 0.5, Side::Sell));
    assert_eq!(value(&e, &u2, "cvd.bybit"), Some(1.5));
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

fn whale_pos(recv: i64, addr: &str, size: f64, entry: f64, venue: Venue) -> EventEnvelope {
    EventEnvelope::new(
        venue,
        SymbolId(0),
        recv,
        recv,
        0,
        MarketEvent::WhalePosition {
            address: addr.into(),
            size,
            entry,
            leverage: 10.0,
            liq_price: f64::NAN,
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
    assert_eq!(value(&e, &u, "whale_print.hyperliquid"), Some(-200_000.0));
}

#[test]
fn fea_1_whale_net_aggregates_across_addresses_and_replaces() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(WhaleNet::new(Venue::Hyperliquid)));
    // A: long 10 BTC @ 30k → +300k.
    let u = e.on_event(&whale_pos(1, "0xa", 10.0, 30_000.0, Venue::Hyperliquid));
    assert_eq!(value(&e, &u, "whale.net.hyperliquid"), Some(300_000.0));
    // B: short 5 BTC @ 30k → +300k − 150k = +150k net.
    let u = e.on_event(&whale_pos(2, "0xb", -5.0, 30_000.0, Venue::Hyperliquid));
    assert_eq!(value(&e, &u, "whale.net.hyperliquid"), Some(150_000.0));
    // A's position REPLACES (last poll wins, snapshot semantics): +2 BTC
    // → +60k − 150k = −90k net: whales flipped net short.
    let u = e.on_event(&whale_pos(3, "0xa", 2.0, 30_000.0, Venue::Hyperliquid));
    assert_eq!(value(&e, &u, "whale.net.hyperliquid"), Some(-90_000.0));
}

#[test]
fn fea_1_whale_delta_changes_since_previous_net() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(WhaleNetDelta::new(Venue::Hyperliquid)));
    // First reading: baseline, no delta yet (oi.delta shape).
    assert!(e
        .on_event(&whale_pos(1, "0xa", 10.0, 30_000.0, Venue::Hyperliquid))
        .is_empty());
    // Second: +300k → +60k = −240k.
    let u = e.on_event(&whale_pos(2, "0xa", 2.0, 30_000.0, Venue::Hyperliquid));
    assert_eq!(value(&e, &u, "whale.delta.hyperliquid"), Some(-240_000.0));
    // Unchanged reading → 0.0 (momentum flat, oi.delta semantics).
    let u = e.on_event(&whale_pos(3, "0xa", 2.0, 30_000.0, Venue::Hyperliquid));
    assert_eq!(value(&e, &u, "whale.delta.hyperliquid"), Some(0.0));
}

#[test]
fn fea_1_whale_net_venue_scoped_and_nan_fail_closed() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(WhaleNet::new(Venue::Hyperliquid)));
    // A Bybit position must not touch the HL-scoped feature.
    assert!(e
        .on_event(&whale_pos(1, "0xa", 10.0, 30_000.0, Venue::Bybit))
        .is_empty());
    // NaN entry → position skipped ENTIRELY (fail-closed, CONV-8); a corrupt
    // frame must not move the aggregate.
    assert!(e
        .on_event(&whale_pos(2, "0xbad", 10.0, f64::NAN, Venue::Hyperliquid))
        .is_empty());
    // NaN size likewise.
    assert!(e
        .on_event(&whale_pos(
            3,
            "0xbad",
            f64::NAN,
            30_000.0,
            Venue::Hyperliquid
        ))
        .is_empty());
    // A valid reading still emits the honest aggregate (only 0xa counted).
    let u = e.on_event(&whale_pos(4, "0xa", 1.0, 30_000.0, Venue::Hyperliquid));
    assert_eq!(value(&e, &u, "whale.net.hyperliquid"), Some(30_000.0));
}

#[test]
fn fea_1_whale_net_evicts_positions_not_refreshed() {
    // The census is top-N + watchlist: a flattened position (no tombstone
    // event from clearinghouseState) or a dropped-off leaderboard address
    // stops refreshing. A 10s stale window must evict it — whale.net is the
    // CURRENT census, never a graveyard of dead positions.
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(WhaleNet::with_stale_after(Venue::Hyperliquid, 10 * SEC)));
    // A at t=1s: +10 BTC @ 30k = +300k.
    let u = e.on_event(&whale_pos(1, "0xa", 10.0, 30_000.0, Venue::Hyperliquid));
    assert_eq!(value(&e, &u, "whale.net.hyperliquid"), Some(300_000.0));
    // B at t=12s: A is stale (last seen 1s < 12s − 10s cutoff) → evicted,
    // only B counted: −5 BTC @ 30k = −150k.
    let u = e.on_event(&whale_pos(
        12 * SEC,
        "0xb",
        -5.0,
        30_000.0,
        Venue::Hyperliquid,
    ));
    assert_eq!(value(&e, &u, "whale.net.hyperliquid"), Some(-150_000.0));
    // A refreshed at t=20s (within B's window too): +2 BTC → +60k − 150k.
    let u = e.on_event(&whale_pos(
        20 * SEC,
        "0xa",
        2.0,
        30_000.0,
        Venue::Hyperliquid,
    ));
    assert_eq!(value(&e, &u, "whale.net.hyperliquid"), Some(-90_000.0));
}

#[test]
fn fea_1_whale_delta_evicts_stale_positions() {
    // The delta feature shares the same census (same eviction helper): a
    // stale position dropped between readings must move the delta.
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| {
        Box::new(WhaleNetDelta::with_stale_after(
            Venue::Hyperliquid,
            10 * SEC,
        ))
    });
    // Baseline: A long 10 BTC @ 30k → +300k, no delta yet.
    assert!(e
        .on_event(&whale_pos(1, "0xa", 10.0, 30_000.0, Venue::Hyperliquid))
        .is_empty());
    // t=12s: A evicted (stale), B short 5 BTC → net −150k; delta −450k.
    let u = e.on_event(&whale_pos(
        12 * SEC,
        "0xb",
        -5.0,
        30_000.0,
        Venue::Hyperliquid,
    ));
    assert_eq!(value(&e, &u, "whale.delta.hyperliquid"), Some(-450_000.0));
}

#[test]
fn fea_1_whale_net_is_order_independent() {
    // Address arrival order must not change the aggregate (BTreeMap keying,
    // CONV-10): 3×10k − 1×20k = +10k either way.
    let mut a = FeatureEngine::new(SEC);
    a.register_tick(|| Box::new(WhaleNet::new(Venue::Hyperliquid)));
    let mut b = FeatureEngine::new(SEC);
    b.register_tick(|| Box::new(WhaleNet::new(Venue::Hyperliquid)));
    let mut ups_a = Vec::new();
    ups_a.extend(a.on_event(&whale_pos(1, "0x1", 3.0, 10_000.0, Venue::Hyperliquid)));
    ups_a.extend(a.on_event(&whale_pos(2, "0x2", -1.0, 20_000.0, Venue::Hyperliquid)));
    let mut ups_b = Vec::new();
    ups_b.extend(b.on_event(&whale_pos(1, "0x2", -1.0, 20_000.0, Venue::Hyperliquid)));
    ups_b.extend(b.on_event(&whale_pos(2, "0x1", 3.0, 10_000.0, Venue::Hyperliquid)));
    assert_eq!(
        value(&a, &ups_a, "whale.net.hyperliquid"),
        value(&b, &ups_b, "whale.net.hyperliquid"),
        "address order must not matter"
    );
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
    assert_eq!(value(&e, &u, "liq.cluster"), Some(-1_200_000.0));
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
    assert_eq!(value(&e, &u, "oi.delta"), Some(50.0));

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
    let ups = e.on_event(&f);
    assert_eq!(value(&e, &ups, "funding.bybit"), Some(0.0001));
}

#[test]
fn fea_1_bar_delta_on_close() {
    let mut e = FeatureEngine::new(SEC);
    e.register_bar(|| Box::new(BarDelta::new("1s")));
    // Two trades in bucket 0, then one in bucket 1 closes bar 0.
    e.on_event(&trade(0, 100.0, 3.0, Side::Buy));
    e.on_event(&trade(SEC / 2, 100.0, 1.0, Side::Sell));
    let u = e.on_event(&trade(SEC + 1, 100.0, 1.0, Side::Buy)); // closes bar 0
    assert_eq!(value(&e, &u, "delta.bar.1s"), Some(2.0)); // 3 buy - 1 sell
}

#[test]
fn fea_1_footprint_delta_buckets_by_notional_and_rolls_on_bar() {
    let mut e = FeatureEngine::new(SEC);
    // whale = notional >= 100k (1000 BTC at $100 is exactly at the boundary).
    e.register_tick(|| Box::new(FootprintDelta::new(SEC, "whale", 100_000.0, f64::MAX)))
        .register_tick(|| Box::new(FootprintDelta::new(SEC, "small", 0.0, 100_000.0)));
    // 100 * 1500 = 150k ≥ 100k → whale bucket, buy. Bar 0 opens, nothing yet.
    assert!(e.on_event(&trade(0, 100.0, 1500.0, Side::Buy)).is_empty());
    // 100 * 500 = 50k → small bucket, sell (in same bar).
    assert!(e
        .on_event(&trade(SEC / 2, 100.0, 500.0, Side::Sell))
        .is_empty());
    // Trade in bar 1 closes bar 0: whale delta = +1500 (buy), small = -500.
    let u = e.on_event(&trade(SEC + 1, 101.0, 1.0, Side::Buy));
    assert_eq!(value(&e, &u, "footprint.delta.1s.whale"), Some(1500.0));
    assert_eq!(value(&e, &u, "footprint.delta.1s.small"), Some(-500.0));
    // The aggregate equals delta.bar.1s semantics: 1500 - 500 = +1000.
    let mut g = FeatureEngine::new(SEC);
    g.register_bar(|| Box::new(BarDelta::new("1s")));
    let mut all = Vec::new();
    for i in 0..3 {
        let ts = if i == 2 { SEC + 1 } else { i * SEC / 2 };
        let ev = if i == 0 {
            trade(ts, 100.0, 1500.0, Side::Buy)
        } else if i == 1 {
            trade(ts, 100.0, 500.0, Side::Sell)
        } else {
            trade(ts, 101.0, 1.0, Side::Buy)
        };
        all.extend(g.on_event(&ev));
    }
    assert_eq!(value(&g, &all, "delta.bar.1s"), Some(1000.0));
}

#[test]
fn fea_1_footprint_imbalance_ratio_and_silence_without_flow() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(FootprintImbalance::new(SEC, "whale", 100_000.0, f64::MAX)));
    // Two buy prints then one sell print in bar 0 (all whale-sized).
    assert!(e.on_event(&trade(0, 100.0, 2000.0, Side::Buy)).is_empty());
    assert!(e
        .on_event(&trade(SEC / 3, 100.0, 1000.0, Side::Buy))
        .is_empty());
    assert!(e
        .on_event(&trade(SEC / 2, 100.0, 1000.0, Side::Sell))
        .is_empty());
    // Bar closes: buy 3000, sell 1000 → (3000-1000)/(3000+1000) = 0.5.
    let u = e.on_event(&trade(SEC + 1, 100.0, 1.0, Side::Buy));
    assert_eq!(value(&e, &u, "footprint.imb.1s.whale"), Some(0.5));
    // A bar with no whale flow is silent (no 0/0 emission, FEA-5 style).
    let u2 = e.on_event(&trade(2 * SEC + 1, 100.0, 1.0, Side::Buy)); // 1 * 100 < 100k
    assert_eq!(u2.len(), 0, "bucket-empty bar must not emit imbalance");
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
    assert!(value(&e, &all, "vol.rv.1s.2").is_some(), "rv should warm");
    assert!(
        value(&e, &all, "breakout.3").is_some(),
        "breakout should warm"
    );
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
    assert!(value(&e, &ups, "breakout.3").is_none());
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
    assert_eq!(value(&e, &u, "imbalance.top"), Some(0.5));

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
    let ups = e.on_event(&gap);
    assert!(value(&e, &ups, "imbalance.top").is_none());
}

// ---- book.depth.* / tape.* (Cryexc/OpenMarket additions, spec 004) ----------

fn book(recv: i64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> EventEnvelope {
    let mut b: smallvec::SmallVec<[_; 8]> = smallvec::SmallVec::new();
    for &(p, q) in bids {
        b.push((p, q));
    }
    let mut a: smallvec::SmallVec<[_; 8]> = smallvec::SmallVec::new();
    for &(p, q) in asks {
        a.push((p, q));
    }
    EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        recv,
        recv,
        100,
        MarketEvent::BookSnapshot {
            bids: b,
            asks: a,
            seq: 100,
            depth: 4,
            reason: SnapshotReason::Init,
        },
    )
}

#[test]
fn om_1_book_depth_bands_gauge_and_total() {
    // mid = (100 + 101)/2 = 100.5.
    // 1% band: bids >= 99.495 → 100×6 only; asks <= 101.505 → 101×2 only.
    //   gauge = (600-202)/(802) ≈ 0.4963, total = 802.
    // 10% band: bids >= 90.45 → 100×6 + 95×4 = 980; asks <= 110.55 → 101×2 + 106×3 = 520.
    //   gauge = (980-520)/(1500) ≈ 0.3067, total = 1500.
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(BookDepth::new(0.01, BookDepthKind::Gauge)))
        .register_tick(|| Box::new(BookDepth::new(0.01, BookDepthKind::Total)))
        .register_tick(|| Box::new(BookDepth::new(0.1, BookDepthKind::Gauge)))
        .register_tick(|| Box::new(BookDepth::new(0.1, BookDepthKind::Total)));
    let u = e.on_event(&book(
        1,
        &[(90.0, 10.0), (95.0, 4.0), (100.0, 6.0)],
        &[(101.0, 2.0), (106.0, 3.0), (111.0, 5.0)],
    ));
    assert!((value(&e, &u, "book.depth.1").unwrap() - 0.496259).abs() < 1e-4);
    assert!((value(&e, &u, "book.depth_total.1").unwrap() - 802.0).abs() < 1e-9);
    assert!((value(&e, &u, "book.depth.10").unwrap() - 0.306667).abs() < 1e-4);
    assert!((value(&e, &u, "book.depth_total.10").unwrap() - 1500.0).abs() < 1e-9);
}

#[test]
fn om_2_book_depth_silent_while_stale() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(BookDepth::new(0.01, BookDepthKind::Gauge)));
    let u = e.on_event(&book(1, &[(100.0, 6.0)], &[(101.0, 2.0)]));
    assert!(value(&e, &u, "book.depth.1").is_some());
    // Gap delta → stale → silent (FEA-8, same BookMirror contract).
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
    let ups = e.on_event(&gap);
    assert!(value(&e, &ups, "book.depth.1").is_none());
}

#[test]
fn om_3_tape_bps_delta_hides_sub_floor_noise() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(TapeBpsDelta::new(0.5)));
    let mut ups = e.on_event(&trade(1, 100.0, 1.0, Side::Buy));
    assert!(
        value(&e, &ups, "tape.bps_delta").is_none(),
        "first trade: no prior"
    );
    ups = e.on_event(&trade(2, 100.0001, 1.0, Side::Buy));
    assert!(
        value(&e, &ups, "tape.bps_delta").is_none(),
        "0.01 bps < 0.5 floor"
    );
    ups = e.on_event(&trade(3, 100.01, 1.0, Side::Buy));
    let d = value(&e, &ups, "tape.bps_delta").unwrap();
    // (100.01-100.0001)/100.0001 × 10000 ≈ 0.99 bps
    assert!((d - 0.99).abs() < 0.02, "got {d}");
}

#[test]
fn om_4_tape_tps_counts_per_bar() {
    let mut e = FeatureEngine::new(SEC);
    e.register_bar(|| Box::new(TapeTps::new("1s", 1.0)));
    let mut all = Vec::new();
    for _ in 0..3 {
        all.extend(e.on_event(&trade(SEC, 100.0, 1.0, Side::Buy)));
    }
    assert!(
        value(&e, &all, "tape.tps.1s").is_none(),
        "bar not closed yet"
    );
    // First trade of the next second closes the 1s bar holding 3 trades.
    all.extend(e.on_event(&trade(2 * SEC, 100.0, 1.0, Side::Buy)));
    assert_eq!(value(&e, &all, "tape.tps.1s"), Some(3.0));
    // End-of-stream finish closes the final partial bar (5 more trades).
    for _ in 0..5 {
        all.extend(e.on_event(&trade(2 * SEC, 100.0, 1.0, Side::Buy)));
    }
    let fin = e.finish(3 * SEC);
    assert_eq!(value(&e, &fin, "tape.tps.1s"), Some(6.0));
}

// ---- liq.* liquidation flow (COL-29 real liq source, spec 004) -------------

fn liq(recv: i64, price: f64, qty: f64, side: Side) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        recv,
        recv,
        0,
        MarketEvent::Liquidation { price, qty, side },
    )
}

#[test]
fn liq_1_vol_by_side_accumulates_and_window_expires() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(LiqVol::new(300 * SEC, Side::Buy)))
        .register_tick(|| Box::new(LiqVol::new(300 * SEC, Side::Sell)));
    // Accumulate updates so the persistent per-side readings stay visible
    // (a feature only re-emits when its own side is touched).
    let mut all = Vec::new();
    all.extend(e.on_event(&liq(1, 100.0, 2.0, Side::Buy)));
    assert_eq!(value(&e, &all, "liq.vol_buy"), Some(200.0));
    assert!(
        value(&e, &all, "liq.vol_sell").is_none(),
        "sell side untouched"
    );
    all.extend(e.on_event(&liq(2, 101.0, 3.0, Side::Sell)));
    assert_eq!(value(&e, &all, "liq.vol_sell"), Some(303.0));
    // Buy sum keeps its own window (not netted against the sell side).
    all.extend(e.on_event(&liq(200 * SEC, 100.0, 0.5, Side::Buy)));
    assert_eq!(value(&e, &all, "liq.vol_buy"), Some(250.0));
    assert_eq!(value(&e, &all, "liq.vol_sell"), Some(303.0));
    // 305s later the t=1 buy has left the 300s window; the t=200s buy stays.
    all.extend(e.on_event(&liq(305 * SEC, 100.0, 1.0, Side::Buy)));
    assert_eq!(value(&e, &all, "liq.vol_buy"), Some(50.0 + 100.0));
}

#[test]
fn liq_2_rate_is_events_per_second_in_window() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(LiqRate::new(100 * SEC)));
    let mut ups = Vec::new();
    for t in [10, 20, 30, 40] {
        ups.extend(e.on_event(&liq(t * SEC, 100.0, 1.0, Side::Sell)));
    }
    // 4 events in a 100s window = 0.04 /s.
    assert!((value(&e, &ups, "liq.rate").unwrap() - 0.04).abs() < 1e-9);
    // At t=150 all four have left the window: 1 / 100s = 0.01 /s.
    ups.extend(e.on_event(&liq(150 * SEC, 100.0, 1.0, Side::Buy)));
    assert!((value(&e, &ups, "liq.rate").unwrap() - 0.01).abs() < 1e-9);
}

#[test]
fn liq_3_dist_measures_price_gap_from_mid_in_bps() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(LiqDist::default()));
    // mid = 100.5 (book helper: 100/101 touch).
    e.on_event(&book(1, &[(100.0, 6.0)], &[(101.0, 2.0)]));
    let u = e.on_event(&liq(2, 99.0, 1.0, Side::Sell));
    let d = value(&e, &u, "liq.dist").unwrap();
    assert!((d - 149.25).abs() < 0.01, "|99-100.5|/100.5 x10000 = {d}");
    // At mid the distance is zero.
    let u2 = e.on_event(&liq(3, 100.5, 1.0, Side::Buy));
    assert_eq!(value(&e, &u2, "liq.dist"), Some(0.0));
}

#[test]
fn liq_4_dist_silent_while_book_stale() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(LiqDist::default()));
    e.on_event(&book(1, &[(100.0, 6.0)], &[(101.0, 2.0)]));
    let u = e.on_event(&liq(2, 99.0, 1.0, Side::Sell));
    assert!(value(&e, &u, "liq.dist").is_some());
    // Gap delta -> stale -> silent (FEA-8, same BookMirror contract).
    let gap = EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        3,
        3,
        105,
        MarketEvent::BookDelta {
            bids: smallvec![(100.0, 9.0)],
            asks: smallvec![],
            first_seq: 105,
            last_seq: 105,
        },
    );
    let ups = e.on_event(&gap);
    assert!(value(&e, &ups, "liq.dist").is_none());
}

fn liq_at(recv: i64, price: f64, qty: f64, side: Side, venue: Venue) -> EventEnvelope {
    EventEnvelope::new(
        venue,
        SymbolId(0),
        recv,
        recv,
        0,
        MarketEvent::Liquidation { price, qty, side },
    )
}

#[test]
fn liq_delta_1_divergence_when_venues_disagree() {
    let mut e = FeatureEngine::new(SEC);
    // Global registration: the divergence needs ONE instance seeing both venues.
    e.register_global_tick(|| Box::new(LiqDelta::new(300 * SEC, Venue::Bybit, Venue::Okx)));
    // Bybit squeezes shorts (buy liqs), Okx dumps longs (sell liqs).
    let mut all = Vec::new();
    all.extend(e.on_event(&liq_at(1, 100.0, 5.0, Side::Buy, Venue::Bybit))); // +500
    all.extend(e.on_event(&liq_at(2, 100.0, 3.0, Side::Sell, Venue::Okx))); //  -300
                                                                            // (500 - 0) - (0 - 300) = 800
    assert_eq!(value(&e, &all, "liq.delta.bybit_okx"), Some(800.0));
    // A sell liq on bybit partially cancels venue a's pressure.
    all.extend(e.on_event(&liq_at(3, 100.0, 1.0, Side::Sell, Venue::Bybit)));
    // (500 - 100) - (0 - 300) = 700
    assert_eq!(value(&e, &all, "liq.delta.bybit_okx"), Some(700.0));
}

#[test]
fn liq_delta_2_sync_venues_near_zero() {
    let mut e = FeatureEngine::new(SEC);
    e.register_global_tick(|| Box::new(LiqDelta::new(300 * SEC, Venue::Bybit, Venue::Okx)));
    // Both venues dump the SAME amount: no divergence (the venues agree).
    let mut all = Vec::new();
    all.extend(e.on_event(&liq_at(1, 100.0, 5.0, Side::Sell, Venue::Bybit)));
    all.extend(e.on_event(&liq_at(2, 100.0, 5.0, Side::Sell, Venue::Okx)));
    assert_eq!(value(&e, &all, "liq.delta.bybit_okx"), Some(0.0));
    // One venue louder: the divergence IS the imbalance between them.
    all.extend(e.on_event(&liq_at(3, 100.0, 2.0, Side::Sell, Venue::Bybit)));
    // (-700) - (-500) = -200
    assert_eq!(value(&e, &all, "liq.delta.bybit_okx"), Some(-200.0));
}

#[test]
fn liq_delta_3_window_expiry_decays_divergence() {
    let mut e = FeatureEngine::new(SEC);
    e.register_global_tick(|| Box::new(LiqDelta::new(300 * SEC, Venue::Bybit, Venue::Okx)));
    let mut all = Vec::new();
    all.extend(e.on_event(&liq_at(1, 100.0, 5.0, Side::Buy, Venue::Bybit))); // +500
    all.extend(e.on_event(&liq_at(2, 100.0, 3.0, Side::Sell, Venue::Okx))); // -300
    assert_eq!(value(&e, &all, "liq.delta.bybit_okx"), Some(800.0));
    // 305s later both have left the 300s window: divergence is zero again.
    all.extend(e.on_event(&liq_at(305 * SEC, 100.0, 1.0, Side::Buy, Venue::Bybit)));
    // (100) - (0) = 100
    assert_eq!(value(&e, &all, "liq.delta.bybit_okx"), Some(100.0));
}

#[test]
fn liq_delta_4_ignores_other_events_and_other_venues() {
    let mut e = FeatureEngine::new(SEC);
    e.register_global_tick(|| Box::new(LiqDelta::new(300 * SEC, Venue::Bybit, Venue::Okx)));
    let mut all = Vec::new();
    // Trades and a third venue's liqs must not move the divergence.
    all.extend(e.on_event(&trade(1, 100.0, 9.0, Side::Buy)));
    all.extend(e.on_event(&liq_at(2, 100.0, 4.0, Side::Buy, Venue::Hyperliquid)));
    assert!(value(&e, &all, "liq.delta.bybit_okx").is_none());
    let u = e.on_event(&liq_at(3, 100.0, 2.0, Side::Sell, Venue::Bybit));
    assert_eq!(value(&e, &u, "liq.delta.bybit_okx"), Some(-200.0));
}

#[test]
fn fea_10_screener_edge_triggers_with_snapshot() {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit)));
    let mut screener = Screener::new(vec![Rule {
        id: "cvd_breakout".into(),
        conds: vec![Cond {
            feature: "cvd.bybit".into(),
            op: Op::Ge,
            threshold: 5.0,
        }],
    }]);
    // Evaluate every tick for this test (minimum clamped to 100ms).
    screener.set_eval_interval_ns(1);
    // Pre-populate SymbolId → name mapping from the engine
    screener.set_name_map(e.name_map().clone());

    let mut hits = Vec::new();
    for u in e.on_event(&trade(1, 100.0, 3.0, Side::Buy)) {
        hits.extend(screener.on_update(&u)); // cvd=3, below 5
    }
    assert!(hits.is_empty());
    for u in e.on_event(&trade(200_000_000, 100.0, 4.0, Side::Buy)) {
        hits.extend(screener.on_update(&u)); // cvd=7 ≥ 5 → fire
    }
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rule_id, "cvd_breakout");
    assert_eq!(hits[0].snapshot.get("cvd.bybit"), Some(&7.0));

    // Still true next tick → edge-triggered, no re-fire.
    let mut hits2 = Vec::new();
    for u in e.on_event(&trade(400_000_000, 100.0, 1.0, Side::Buy)) {
        hits2.extend(screener.on_update(&u)); // cvd=8, still ≥5
    }
    assert!(hits2.is_empty(), "edge-triggered: fires once");
}

// ---- FEA-7: catalog config (features.toml) --------------------------------

#[test]
fn fea_7_features_toml_parses_and_rejects_unknown_keys() {
    use mp_features::{FeaturesConfig, LiqDeltaParams};
    let toml = r#"
        bar_tf_ns = 60000000000
        [cvd]
        venues = ["bybit", "okx"]
        [whale_print]
        min_notional = 300000.0
        [whale_net]
        venues = ["hyperliquid", "bybit"]
        stale_after_ns = 300000000000
        [liq_cluster]
        window_ns = 30000000000
        min_cluster_notional = 4000000.0
    "#;
    let cfg = FeaturesConfig::from_toml(toml).unwrap();
    assert_eq!(cfg.cvd.venues, vec!["bybit", "okx"]);

    // The PRODUCTION config must keep parsing as the catalog grows (FEA-7
    // deny_unknown_fields would otherwise reject a section the struct doesn't
    // know — this test catches a drift between features.toml and the parser).
    let prod = std::fs::read_to_string("features.toml")
        .expect("features/features.toml should exist relative to the crate dir");
    let prod_cfg = FeaturesConfig::from_toml(&prod)
        .unwrap_or_else(|e| panic!("production features.toml must parse: {e}"));
    assert!(!prod_cfg.book_depth.bands.is_empty());
    assert!(prod_cfg.tape.min_bps_delta > 0.0);
    assert_eq!(prod_cfg.liq_flow.window_ns, 300_000_000_000);
    assert_eq!(cfg.whale_print.min_notional, 300000.0);
    assert_eq!(cfg.whale_net.venues, vec!["hyperliquid", "bybit"]);
    assert_eq!(cfg.whale_net.stale_after_ns, 300_000_000_000);

    // Defaults when the section is absent (hyperliquid only, 10 min stale).
    let bare = FeaturesConfig::from_toml("[whale_print]\nmin_notional = 1.0").unwrap();
    assert_eq!(bare.whale_net.venues, vec!["hyperliquid"]);
    assert_eq!(bare.whale_net.stale_after_ns, 600_000_000_000);

    // A typo'd key is a hard error (deny_unknown_fields), never a silent default.
    let bad = r#"
        [whale_print]
        min_notionl = 300000.0
    "#;
    assert!(FeaturesConfig::from_toml(bad).is_err());

    // Cross-venue pair validation (CONV-8 fail-closed): an unknown venue slug
    // or an identical pair is an engine_from_config error, never a silent
    // skip or a nonsense self-divergence.
    use mp_features::engine_from_config;
    let bad_slug = FeaturesConfig {
        liq_delta: LiqDeltaParams {
            pairs: vec![["bybit".into(), "not-a-venue".into()]],
            ..LiqDeltaParams::default()
        },
        ..FeaturesConfig::default()
    };
    assert!(engine_from_config(&bad_slug).is_err());
    let same_pair = FeaturesConfig {
        liq_delta: LiqDeltaParams {
            pairs: vec![["bybit".into(), "bybit".into()]],
            ..LiqDeltaParams::default()
        },
        ..FeaturesConfig::default()
    };
    assert!(engine_from_config(&same_pair).is_err());
    // The production pair (bybit x binance) registers cleanly.
    let prod_cfg_engine = engine_from_config(&prod_cfg).unwrap();
    assert!(
        prod_cfg_engine
            .name_to_id("liq.delta.bybit_binance")
            .is_some(),
        "production [liq_delta] pair must register"
    );
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
    assert!(!ups.iter().any(|u| e.resolve_name(u.feature) == "nan.test"));
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
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit))); // online-capable
    e.register_tick(|| Box::new(LeadLag)); // offline-only
                                           // The live runner MUST call this after registration and refuse to start
                                           // if it is non-empty (FEA-9): leadlag never runs on the live path.
    assert_eq!(
        e.offline_only_features(),
        vec!["leadlag.bybit.okx".to_string()]
    );
    let mut clean = FeatureEngine::new(1_000_000_000);
    clean.register_tick(|| Box::new(Cvd::new(Venue::Bybit)));
    assert!(clean.offline_only_features().is_empty());
}

// ---- microstructure (microprice / spread.bp / spread.regime) ---------------

fn book_hl(recv: i64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> EventEnvelope {
    let mut b: smallvec::SmallVec<[_; 8]> = smallvec::SmallVec::new();
    for &(p, q) in bids {
        b.push((p, q));
    }
    let mut a: smallvec::SmallVec<[_; 8]> = smallvec::SmallVec::new();
    for &(p, q) in asks {
        a.push((p, q));
    }
    EventEnvelope::new(
        Venue::Hyperliquid,
        SymbolId(0),
        recv,
        recv,
        100,
        MarketEvent::BookSnapshot {
            bids: b,
            asks: a,
            seq: 100,
            depth: 4,
            reason: SnapshotReason::Init,
        },
    )
}

fn micro_engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(SEC);
    e.register_tick(|| Box::new(Microprice::for_venue(Venue::Hyperliquid)))
        .register_tick(|| Box::new(SpreadBp::for_venue(Venue::Hyperliquid)))
        .register_tick(|| Box::new(SpreadRegime::for_venue(Venue::Hyperliquid, 2.0)));
    e
}

#[test]
fn mic_1_microprice_is_qty_weighted_mid() {
    // bid 100 × 10, ask 101 × 20: microprice = (10·101 + 20·100)/30 ≈ 100.3333.
    let mut e = micro_engine();
    let u = e.on_event(&book_hl(1, &[(100.0, 10.0)], &[(101.0, 20.0)]));
    let mp = value(&e, &u, "microprice.hyperliquid").unwrap();
    assert!((mp - 100.333333333).abs() < 1e-6);
    // Venue mismatch: a Bybit book must not feed the HL-scoped feature.
    let u = e.on_event(&book(1, &[(100.0, 10.0)], &[(101.0, 20.0)]));
    assert!(value(&e, &u, "microprice.hyperliquid").is_none());
}

#[test]
fn mic_2_spread_bp_and_regime_threshold() {
    // bid 100, ask 101 → spread = 1/100.5 × 10_000 ≈ 99.5 bps ≥ 2 → regime 1.
    let mut e = micro_engine();
    let u = e.on_event(&book_hl(1, &[(100.0, 6.0)], &[(101.0, 2.0)]));
    let bp = value(&e, &u, "spread.bp.hyperliquid").unwrap();
    assert!((bp - 99.50248756).abs() < 1e-4);
    assert_eq!(value(&e, &u, "spread.regime.hyperliquid"), Some(1.0));
    // Tight book: 100 / 100.01 → 0.99995 bps < 2 → regime 0.
    let u = e.on_event(&book_hl(2, &[(100.0, 6.0)], &[(100.01, 2.0)]));
    let bp = value(&e, &u, "spread.bp.hyperliquid").unwrap();
    assert!((bp - 0.9999500).abs() < 1e-6);
    assert_eq!(value(&e, &u, "spread.regime.hyperliquid"), Some(0.0));
}

#[test]
fn mic_3_silent_on_stale_or_one_sided_book() {
    // One-sided snapshot (no asks) → nothing to compute (FEA-8).
    let mut e = micro_engine();
    assert!(e.on_event(&book_hl(1, &[(100.0, 6.0)], &[])).is_empty());
    // A gap delta stales the book → all three go silent.
    let gap = EventEnvelope::new(
        Venue::Hyperliquid,
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
    assert!(e.on_event(&gap).is_empty());
}

#[test]
fn mic_4_config_registers_microstructure_family_and_rejects_unknown_slug() {
    use mp_features::FeaturesConfig;
    let cfg = FeaturesConfig::from_toml(
        "[microstructure]\nvenues = [\"hyperliquid\"]\nwide_spread_bps = 1.5",
    )
    .unwrap();
    assert_eq!(cfg.microstructure.wide_spread_bps, 1.5);
    let mut e = mp_features::engine_from_config(&cfg).unwrap();
    let u = e.on_event(&book_hl(1, &[(100.0, 10.0)], &[(100.02, 10.0)]));
    // spread = 0.02/100.01 × 10_000 ≈ 2.0 bps ≥ 1.5 → regime 1.
    let mp = value(&e, &u, "microprice.hyperliquid").unwrap();
    assert!((mp - 100.01).abs() < 1e-6);
    assert_eq!(value(&e, &u, "spread.regime.hyperliquid"), Some(1.0));
    // Fail-closed (CONV-8): a bogus venue slug is an error, not a skip.
    let bad = FeaturesConfig {
        microstructure: mp_features::MicrostructureParams {
            venues: vec!["bogus".into()],
            wide_spread_bps: 2.0,
        },
        ..FeaturesConfig::default()
    };
    assert!(mp_features::engine_from_config(&bad).is_err());
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

// ---- spec 035 SWG-2: swing bar features register via engine_from_config -----

#[test]
fn swg_2_engine_from_config_registers_swing_bar_features() {
    use mp_features::FeaturesConfig;
    let cfg = FeaturesConfig::from_toml(
        r#"
        bar_tf_ns = 60000000000
        [swing]
        realized_vol_window = 5
        sqrt_bars_per_year = 725.2
        trend_lookback = 3
        value_area_window = 5
        value_area_bucket = 10.0
        rolling_vwap_bars = 3
        "#,
    )
    .unwrap();
    let mut e = mp_features::engine_from_config(&cfg).unwrap();
    // Every swing feature id must be registered and interned (SWG-2: bar-only
    // regime/structure available to strategies).
    for id in [
        "swing.realized_vol.5",
        "swing.trend_strength.3",
        "swing.value_area.poc.5",
        "swing.value_area.high.5",
        "swing.value_area.low.5",
        "swing.rolling_vwap.3",
    ] {
        assert!(e.name_to_id(id).is_some(), "{id} must register");
    }
    // Feed 6 bars (one trade per 60s bucket) so 5 bars close; each swing
    // feature emits once warm.
    let mut all = Vec::new();
    for i in 0..6 {
        all.extend(e.on_event(&trade(i * SEC * 60 + 1, 100.0 + i as f64, 1.0, Side::Buy)));
    }
    assert!(value(&e, &all, "swing.realized_vol.5").is_some());
    assert!(value(&e, &all, "swing.trend_strength.3").is_some());
    assert!(value(&e, &all, "swing.value_area.poc.5").is_some());
    assert!(value(&e, &all, "swing.value_area.high.5").is_some());
    assert!(value(&e, &all, "swing.value_area.low.5").is_some());
    assert!(value(&e, &all, "swing.rolling_vwap.3").is_some());
    // Params change → new feature-store ver (FEA-6): different config hash.
    let cfg2 = FeaturesConfig::from_toml("[swing]\nrealized_vol_window = 20").unwrap();
    assert_ne!(cfg.params_hash().unwrap(), cfg2.params_hash().unwrap());
}

// ---- spec 036 SLQ: liquidity-structure family registers via engine config --

#[test]
fn slq_engine_from_config_registers_sweep_and_profile_family() {
    use mp_features::FeaturesConfig;
    let cfg = FeaturesConfig::from_toml(
        r#"
        bar_tf_ns = 60000000000
        [swing]
        sweep_range_n = 5
        sweep_atr_n = 5
        profile_window = 6
        "#,
    )
    .unwrap();
    let e = mp_features::engine_from_config(&cfg).unwrap();
    // The whole SLQ family must register: ATR, close passthrough, both range
    // boundaries, the four sweep event streams, and the four nearest-level
    // streams (FEA-4 one-code-path: live and materialized share these ids).
    for id in [
        "swing.atr.5",
        "swing.close",
        "swing.range.high.5",
        "swing.range.low.5",
        "swing.sweep.low.5",
        "swing.sweep.high.5",
        "swing.sweep.low.stop.5",
        "swing.sweep.high.stop.5",
        "swing.profile.hvn_above.6",
        "swing.profile.hvn_below.6",
        "swing.profile.lvn_above.6",
        "swing.profile.lvn_below.6",
    ] {
        assert!(e.name_to_id(id).is_some(), "{id} must register");
    }
    // Unknown [swing] keys still fail closed (FEA-7).
    assert!(FeaturesConfig::from_toml("[swing]\nsweep_range_k = 5").is_err());
}
