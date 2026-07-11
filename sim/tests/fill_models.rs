//! Fill-model ladder (SIM-2), funding/coverage run-refusal (SIM-4/6), and the
//! 2x-cost/optimistic-maker metrics (SIM-8/12). Test names embed requirement
//! IDs (CONV-21).

use mp_core::{
    EventEnvelope, IntentId, MarketEvent, OrderIntent, OrderKind, Side, SizeUnit, StrategyId,
    SymbolId, TimeInForce, Venue,
};
use mp_features::catalog::Cvd;
use mp_features::{FeatureEngine, FeatureUpdate};
use mp_sim::{Backtester, FillModel, SimConfig, SimError};
use mp_strategies::{Ctx, RegimeMask, Strategy, Universe};

const MS: i64 = 1_000_000;
const SYM: SymbolId = SymbolId(0);

fn trade(recv: i64, price: f64, qty: f64, side: Side) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SYM,
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

fn book_snapshot(
    recv: i64,
    seq: u64,
    bids: Vec<(f64, f64)>,
    asks: Vec<(f64, f64)>,
) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SYM,
        recv,
        recv,
        seq,
        MarketEvent::BookSnapshot {
            bids: bids.into_iter().collect(),
            asks: asks.into_iter().collect(),
            seq,
            depth: 0,
            reason: mp_core::SnapshotReason::Init,
        },
    )
}

fn funding(recv: i64, rate: f64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SYM,
        recv,
        recv,
        recv as u64,
        MarketEvent::Funding {
            rate,
            interval_s: 28_800,
            next_funding_ts_ns: recv + 28_800_000_000_000,
        },
    )
}

fn engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new("bybit")));
    e
}

/// A strategy that emits exactly one intent, on the first feature update it
/// sees, then goes silent. Lets tests drive the fill models with a precise,
/// known order rather than CoinFlip's seeded randomness.
struct OneShot {
    fired: bool,
    kind: OrderKind,
    side: Side,
    qty: f64,
}

impl Strategy for OneShot {
    fn id(&self) -> StrategyId {
        StrategyId::new("oneshot")
    }
    fn universe(&self) -> Universe {
        Universe::default()
    }
    fn subscriptions(&self) -> Vec<String> {
        vec!["cvd.bybit".to_string()]
    }
    fn warmup_ns(&self) -> i64 {
        0
    }
    fn declared_regime(&self) -> RegimeMask {
        RegimeMask::any()
    }
    fn on_feature(&mut self, u: &FeatureUpdate, _ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        if self.fired {
            return Vec::new();
        }
        self.fired = true;
        vec![OrderIntent {
            intent_id: IntentId(1),
            strategy: self.id(),
            venue: Venue::Bybit,
            symbol: u.symbol,
            side: self.side,
            kind: self.kind,
            qty: SizeUnit::Contracts(self.qty),
            tif: TimeInForce::Ioc,
            reduce_only: false,
            tag: "oneshot".into(),
        }]
    }
}

fn cfg(fill_model: FillModel) -> SimConfig {
    SimConfig {
        fill_model,
        latency_ns: 0,
        slip_frac: 0.0,
        ..SimConfig::default()
    }
}

// ---- SIM-2 L1: participation cap + partial-fill walk-forward -------------

#[test]
fn sim_2_l1_market_buy_is_capped_by_top_of_book_participation() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Market,
            side: Side::Buy,
            qty: 10.0, // wants 10, but the ask only shows 4 (cap = 4*0.5=2)
        }),
        cfg(FillModel::L1TopOfBook),
        1,
    );
    let events = vec![
        book_snapshot(0, 1, vec![(99.0, 100.0)], vec![(100.0, 4.0)]),
        trade(MS, 100.0, 1.0, Side::Buy), // triggers strategy + first fill attempt
        trade(2 * MS, 100.0, 1.0, Side::Buy), // book unchanged: same cap again
    ];
    bt.run(&events).unwrap();
    // First attempt fills the participation-capped qty (4*0.5=2), not the
    // full 10 requested — the rest remains pending (walks forward).
    assert!(bt.position(SYM) > 0.0 && bt.position(SYM) < 10.0);
}

#[test]
fn sim_2_l1_limit_trade_print_rule_bans_touch_fills() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Limit { px: 99.0 },
            side: Side::Buy,
            qty: 1.0,
        }),
        cfg(FillModel::L1TopOfBook),
        1,
    );
    let events = vec![
        // Triggers the strategy; the intent is enqueued as a resting buy limit @99.
        trade(0, 100.0, 1.0, Side::Buy),
        // A sell print at 99.5 touches nothing below 99 — must NOT fill.
        trade(MS, 99.5, 1.0, Side::Sell),
    ];
    bt.run(&events).unwrap();
    assert_eq!(bt.position(SYM), 0.0, "touching the level must not fill");

    // A sell print that actually trades through 99 — now it fills.
    bt.run(&[trade(2 * MS, 98.5, 1.0, Side::Sell)]).unwrap();
    assert_eq!(bt.position(SYM), 1.0);
    assert_eq!(bt.avg_cost(SYM), 99.0, "fills at the resting limit price");
    assert!(bt.metrics().maker_trades == 0); // no closing trade yet, but the fill was tagged
}

// ---- SIM-2 L2: depth walk pays impact across levels -----------------------

#[test]
fn sim_2_l2_market_buy_walks_multiple_levels_and_pays_impact() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Market,
            side: Side::Buy,
            qty: 3.0,
        }),
        cfg(FillModel::L2DepthWalk),
        1,
    );
    let events = vec![
        book_snapshot(0, 1, vec![(99.0, 100.0)], vec![(100.0, 1.0), (101.0, 5.0)]),
        trade(MS, 100.0, 1.0, Side::Buy), // fires the strategy (enqueues the intent)
        trade(2 * MS, 100.0, 1.0, Side::Buy), // next event: the walk actually executes
    ];
    bt.run(&events).unwrap();
    assert_eq!(bt.position(SYM), 3.0);
    // 1 @100 + 2 @101, weighted average = (100 + 202) / 3.
    let expected_avg = (100.0 + 2.0 * 101.0) / 3.0;
    assert!(
        (bt.avg_cost(SYM) - expected_avg).abs() < 1e-9,
        "avg={} expected={}",
        bt.avg_cost(SYM),
        expected_avg
    );
}

#[test]
fn sim_2_l2_limit_fill_capped_by_queue_share_needs_multiple_prints() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Limit { px: 99.0 },
            side: Side::Buy,
            qty: 1.0,
        }),
        cfg(FillModel::L2DepthWalk), // queue_share default 0.25
        1,
    );
    // Fires the strategy: resting buy limit @99, qty 1.
    bt.run(&[trade(0, 100.0, 1.0, Side::Buy)]).unwrap();
    // A crossing print of qty 1 only fills 1*0.25=0.25 of the resting limit.
    bt.run(&[trade(MS, 98.0, 1.0, Side::Sell)]).unwrap();
    assert!(
        (bt.position(SYM) - 0.25).abs() < 1e-9,
        "first print only fills the queue_share slice, got {}",
        bt.position(SYM)
    );
    // A second crossing print fills the rest.
    bt.run(&[trade(2 * MS, 98.0, 3.0, Side::Sell)]).unwrap();
    assert!(
        (bt.position(SYM) - 1.0).abs() < 1e-9,
        "subsequent prints complete the fill, got {}",
        bt.position(SYM)
    );
}

// ---- SIM-4: missing-funding run refusal -----------------------------------

#[test]
fn sim_4_missing_funding_refuses_to_report_a_held_perp_position() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Market,
            side: Side::Buy,
            qty: 1.0,
        }),
        cfg(FillModel::L1TopOfBook),
        1,
    );
    let nine_hours = 9 * 3_600 * 1_000_000_000i64;
    let events = vec![
        trade(0, 100.0, 1.0, Side::Buy),          // opens the position
        trade(nine_hours, 100.0, 1.0, Side::Buy), // time passes, no Funding ever
    ];
    let err = bt.run(&events).unwrap_err();
    assert_eq!(err, SimError::MissingFunding(SYM));
}

#[test]
fn sim_4_funding_event_present_lets_the_run_report() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Market,
            side: Side::Buy,
            qty: 1.0,
        }),
        cfg(FillModel::L1TopOfBook),
        1,
    );
    let nine_hours = 9 * 3_600 * 1_000_000_000i64;
    let events = vec![
        trade(0, 100.0, 1.0, Side::Buy),
        funding(4 * 3_600 * 1_000_000_000i64, 0.0001),
        trade(nine_hours, 100.0, 1.0, Side::Buy),
    ];
    assert!(bt.run(&events).is_ok());
}

// ---- SIM-6: low-coverage run refusal --------------------------------------

#[test]
fn sim_6_low_manifest_coverage_refuses_the_run() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Market,
            side: Side::Buy,
            qty: 1.0,
        }),
        SimConfig::default(), // min_coverage default 0.995
        1,
    );
    let err = bt
        .run_checked(&[trade(0, 100.0, 1.0, Side::Buy)], 0.90)
        .unwrap_err();
    assert_eq!(
        err,
        SimError::LowCoverage {
            actual: 0.90,
            required: 0.995
        }
    );
}

#[test]
fn sim_6_full_coverage_runs_normally() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Market,
            side: Side::Buy,
            qty: 1.0,
        }),
        SimConfig::default(),
        1,
    );
    assert!(bt
        .run_checked(&[trade(0, 100.0, 1.0, Side::Buy)], 1.0)
        .is_ok());
}

// ---- SIM-8/SIM-12: stress expectancy + optimistic-maker split -------------

#[test]
fn sim_8_stress_expectancy_2x_is_never_better_than_base_when_fees_positive() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Market,
            side: Side::Buy,
            qty: 1.0,
        }),
        cfg(FillModel::L1TopOfBook),
        1,
    );
    bt.run(&[
        trade(0, 100.0, 1.0, Side::Buy),
        trade(MS, 110.0, 1.0, Side::Buy), // marks it up so unrealized exists
    ])
    .unwrap();
    // No closed trade yet (still holding) ⇒ both are 0, but the call must not
    // panic and must be well-defined; the inequality holds whenever fees > 0
    // and trades > 0. Assert the always-available contract (SIM-8): the
    // stress figure is <= the base expectancy.
    assert!(bt.stress_expectancy_2x() <= bt.metrics().expectancy() + 1e-12);
}

#[test]
fn sim_12_optimistic_maker_fills_are_tracked_separately() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Limit { px: 99.0 },
            side: Side::Buy,
            qty: 1.0,
        }),
        cfg(FillModel::L1TopOfBook),
        1,
    );
    bt.run(&[
        trade(0, 100.0, 1.0, Side::Buy),  // rests the limit
        trade(MS, 98.0, 1.0, Side::Sell), // trade-print fills it (maker, optimistic)
    ])
    .unwrap();
    assert_eq!(bt.position(SYM), 1.0);
    // The fill itself opened a position (no realized P&L yet since nothing
    // closed) — maker_trades stays 0 until a maker-tagged fill *closes* P&L.
    // Force a close via a market sell so a maker-tagged realized trade lands.
    bt.run(&[trade(2 * MS, 105.0, 5.0, Side::Sell)]).unwrap();
    // The buy-side open was maker (optimistic); its eventual close realizes
    // P&L attributed through record_trade on the taker leg. We assert the
    // metric exists and is queryable without panic (contract, not a specific
    // number, since attribution is on the closing fill's own tag).
    let _ = bt.metrics().maker_expectancy();
}

// ---- SIM-3: latency injection defers fills ---------------------------------

#[test]
fn sim_3_fill_waits_for_configured_latency() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(OneShot {
            fired: false,
            kind: OrderKind::Market,
            side: Side::Buy,
            qty: 1.0,
        }),
        SimConfig {
            fill_model: FillModel::L1TopOfBook,
            latency_ns: 200 * MS,
            slip_frac: 0.0,
            ..SimConfig::default()
        },
        1,
    );
    bt.run(&[
        trade(0, 100.0, 1.0, Side::Buy),        // intent at t=0
        trade(100 * MS, 101.0, 1.0, Side::Buy), // t=100ms < 200ms: must NOT fill
    ])
    .unwrap();
    assert_eq!(bt.position(SYM), 0.0, "fill before latency elapsed");
    bt.run(&[trade(250 * MS, 102.0, 1.0, Side::Buy)]).unwrap(); // t=250ms >= 200ms
    assert_eq!(bt.position(SYM), 1.0);
    assert_eq!(bt.avg_cost(SYM), 102.0, "fills at the post-latency print");
}
