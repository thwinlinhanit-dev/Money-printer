//! Acceptance tests for spec 035 SWG-7 (bar-close execution, amending spec
//! 007). Test names embed requirement IDs (CONV-21): swg_7_*.
//!
//! The sim must dispatch a `Daily`/`FourHour`-cadence strategy ONLY on bar
//! close (feature updates AND its own timers — deferred to the boundary, never
//! dropped), while legacy `Event`-cadence strategies keep per-event dispatch.

use mp_core::{
    EventEnvelope, IntentId, MarketEvent, OrderIntent, OrderKind, RebalanceCadence, Side, SizeUnit,
    StrategyId, SymbolId, TimeInForce, Venue,
};
use mp_features::catalog::Cvd;
use mp_features::FeatureEngine;
use mp_features::FeatureUpdate;
use mp_sim::{Backtester, SimConfig};
use mp_strategies::strategy::{Ctx, TimerId};
use mp_strategies::{RegimeMask, Strategy, Universe};

const HR: i64 = 3_600_000_000_000;

fn trade(recv: i64, price: f64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        recv,
        recv,
        recv as u64,
        MarketEvent::Trade {
            price,
            qty: 1.0,
            side: Side::Buy,
            trade_id: recv as u64,
        },
    )
}

fn funding(recv: i64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        recv,
        recv,
        recv as u64,
        MarketEvent::Funding {
            rate: 0.0001,
            interval_s: 8 * 3600,
            next_funding_ts_ns: recv,
        },
    )
}

/// 9 hourly trades starting at 0h: with bar_tf = 4h the bar boundaries are the
/// events at 0h (first event), 4h, and 8h — 3 boundaries out of 9 events. A
/// funding tick lands just after the 8h trade so the SIM-4 hold-coverage guard
/// is satisfied (positions are held across the 8h funding boundary). Funding
/// events produce no Cvd updates, so dispatch counts are unaffected.
fn hourly_feed(start_h: i64, n: i64) -> Vec<EventEnvelope> {
    let mut evs: Vec<EventEnvelope> = (0..n)
        .map(|i| trade((start_h + i) * HR, 100.0 + (i % 5) as f64))
        .collect();
    evs.push(funding((start_h + n) * HR + 1_000_000_000));
    evs
}

fn engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit)));
    e
}

/// A probe strategy that emits a standard market order on every dispatch.
/// `arm_timer` adds a +1h timer per feature dispatch, fired in `on_timer`
/// (also a market order). Cadence is configurable — the whole point of SWG-7
/// is that the ENGINE gates by cadence, not the strategy.
struct SwingProbe {
    id: StrategyId,
    cadence: RebalanceCadence,
    arm_timer: bool,
    next_intent: u128,
}

impl SwingProbe {
    fn new(cadence: RebalanceCadence, arm_timer: bool) -> Self {
        Self {
            id: StrategyId::new("swing-probe"),
            cadence,
            arm_timer,
            next_intent: 0,
        }
    }
    fn order(&mut self) -> OrderIntent {
        self.next_intent += 1;
        OrderIntent {
            intent_id: IntentId(self.next_intent),
            strategy: self.id.clone(),
            venue: Venue::Bybit,
            symbol: SymbolId(0),
            side: Side::Buy,
            kind: OrderKind::Market,
            qty: SizeUnit::Contracts(0.01),
            tif: TimeInForce::Ioc,
            reduce_only: false,
            tag: "swg7".into(),
        }
    }
}

impl Strategy for SwingProbe {
    fn id(&self) -> StrategyId {
        self.id.clone()
    }
    fn universe(&self) -> Universe {
        Universe {
            venues: vec![Venue::Bybit],
            symbols: vec![SymbolId(0)],
        }
    }
    fn subscriptions(&self) -> Vec<String> {
        vec!["cvd.bybit".into()]
    }
    fn warmup_ns(&self) -> i64 {
        0
    }
    fn declared_regime(&self) -> RegimeMask {
        RegimeMask::any()
    }
    fn on_feature(&mut self, _u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        if self.arm_timer {
            ctx.set_timer(HR);
        }
        vec![self.order()]
    }
    fn on_timer(&mut self, _t: TimerId, _ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        vec![self.order()]
    }
    fn with_params(&self, _p: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
        Box::new(SwingProbe::new(self.cadence, self.arm_timer))
    }
    fn rebalance_cadence(&self) -> RebalanceCadence {
        self.cadence
    }
}

fn run_with(cadence: RebalanceCadence, arm_timer: bool) -> Backtester {
    let mut bt = Backtester::new(
        engine(),
        Box::new(SwingProbe::new(cadence, arm_timer)),
        SimConfig {
            latency_ns: 0,
            bar_tf_ns: 4 * HR, // 4h bars — the swing bar set (spec 035 §3)
            ..SimConfig::default()
        },
        42,
    );
    bt.run(&hourly_feed(0, 9)).unwrap();
    bt
}

#[test]
fn swg_7_swing_strategy_dispatched_only_on_bar_close() {
    // 9 hourly events, 4h bars ⇒ exactly 3 bar boundaries (0h, 4h, 8h). The
    // swing probe must act ONLY there — 3 intents, not 9 — for BOTH bar-close
    // cadences, and its standard market orders still fill.
    for cadence in [RebalanceCadence::Daily, RebalanceCadence::FourHour] {
        let bt = run_with(cadence, false);
        assert_eq!(
            bt.decision_log().intent_count(),
            3,
            "{cadence:?} must evaluate only on bar close (3 boundaries in 9 events)"
        );
        assert!(
            bt.decision_log().fill_count() > 0,
            "standard market orders must still execute through the fill machinery"
        );
    }
}

#[test]
fn swg_7_event_cadence_strategy_keeps_per_event_dispatch() {
    // The v1 per-tick behavior is untouched: `Event` cadence ⇒ one dispatch per
    // feature update (9), never gated to bar boundaries.
    let bt = run_with(RebalanceCadence::Event, false);
    assert_eq!(bt.decision_log().intent_count(), 9);
}

#[test]
fn swg_7_swing_timer_deferred_to_bar_close_not_dropped() {
    // Timer armed at every feature dispatch, +1h. Swing (FourHour):
    // - feature intents at the 3 boundaries (0h, 4h, 8h) = 3
    // - timer set at 0h fires at 1h — MID-bar → deferred to the 4h boundary,
    //   where it fires: 1
    // - timer set at 4h fires at 5h — mid-bar → deferred to the 8h boundary,
    //   where it fires: 1
    // - timer set at 8h fires at 9h — past the run end, moot.
    // Total 5 — every timer survives to a bar close; none are silently dropped.
    let bt = run_with(RebalanceCadence::FourHour, true);
    assert_eq!(bt.decision_log().intent_count(), 5);

    // Event cadence on the same feed: 9 feature intents + 9 hourly timer
    // intents (a timer armed at every dispatch, 0h..8h, firing 1h..9h) = 18 —
    // timers are NOT deferred for the legacy cadence.
    let bt = run_with(RebalanceCadence::Event, true);
    assert_eq!(bt.decision_log().intent_count(), 18);
}
