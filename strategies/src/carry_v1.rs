//! Funding-rate carry strategy (spec 015). Monitors funding rate per
//! (venue, symbol); enters on extreme funding (long negative rate, short
//! positive rate), exits when funding normalizes. Single-venue v1.

use crate::strategy::{Ctx, ParamSpace, RegimeMask, Strategy, Universe};
use mp_core::{IntentId, OrderIntent, OrderKind, Side, SizeUnit, StrategyId, SymbolId, Venue};
use mp_features::FeatureUpdate;
use std::collections::BTreeMap;

/// Configuration for the carry-v1 strategy.
#[derive(Debug, Clone, Copy)]
pub struct CarryConfig {
    /// |funding.rate| over which we enter (default 0.01% = 0.0001).
    pub entry_threshold: f64,
    /// |funding.rate| below which we exit (default 0.002% = 0.00002).
    pub exit_threshold: f64,
    /// Annualized vol target for sizing.
    pub vol_target: f64,
    /// Max fraction of portfolio for this strategy.
    pub max_gross_exposure: f64,
    /// Max hold time in nanoseconds (default 14 days).
    pub max_hold_ns: i64,
    /// Max adverse funding accumulation before stop (default 2%).
    pub max_adverse_funding: f64,
    /// Cancel signal if not filled within this many ns.
    pub signal_timeout_ns: i64,
}

impl Default for CarryConfig {
    fn default() -> Self {
        Self {
            entry_threshold: 0.0001,
            exit_threshold: 0.00002,
            vol_target: 0.15,        // 15% annualized
            max_gross_exposure: 0.1, // 10% of portfolio
            max_hold_ns: 14 * 86_400 * 1_000_000_000, // 14 days
            max_adverse_funding: 0.02,
            signal_timeout_ns: 60 * 1_000_000_000, // 60 seconds
        }
    }
}

/// Strategy state machine: IDLE → ENTRY_SIGNALED → ENTERED → EXIT_SIGNALED → IDLE.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CarryState {
    Idle,
    EntrySignaled { direction: Side, entry_value: f64, entry_ts_ns: i64 },
    Entered { direction: Side, entry_value: f64, entry_ts_ns: i64, cumulative_funding: f64 },
    ExitSignaled,
}

/// Tracks the funding rate for one venue/symbol pair.
#[allow(dead_code)]
struct FundingState {
    last_rate: f64,
    last_ts_ns: i64,
}

/// The carry strategy.
pub struct CarryV1 {
    id: StrategyId,
    config: CarryConfig,
    universe: Universe,
    funding: BTreeMap<(Venue, SymbolId), FundingState>,
    state: CarryState,
    next_intent: u128,
}

impl CarryV1 {
    pub fn new(id: StrategyId, universe: Universe, config: CarryConfig) -> Self {
        Self { id, config, universe, funding: BTreeMap::new(), state: CarryState::Idle, next_intent: 1 }
    }

    fn make_intent(&mut self, side: Side, tag: &str) -> OrderIntent {
        let sym = self.universe.symbols.first().copied().unwrap_or(SymbolId(0));
        let venue = self.universe.venues.first().copied().unwrap_or(Venue::Hyperliquid);
        let iid = self.next_intent;
        self.next_intent += 1;
        OrderIntent {
            intent_id: IntentId(iid),
            strategy: self.id.clone(),
            venue,
            symbol: sym,
            side,
            kind: OrderKind::Market,
            qty: SizeUnit::RiskUnits(self.config.max_gross_exposure), // vol-targeted via risk gate
            tif: mp_core::TimeInForce::Ioc,
            reduce_only: false,
            tag: tag.into(),
        }
    }

    fn check_entry(&mut self, rate: f64, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        if rate.abs() < self.config.entry_threshold {
            return Vec::new();
        }
        let direction = if rate > 0.0 { Side::Sell } else { Side::Buy };
        self.state = CarryState::EntrySignaled {
            direction,
            entry_value: rate,
            entry_ts_ns: ctx.now_ns(),
        };
        vec![self.make_intent(direction, "carry-v1 entry")]
    }

    fn check_exit(&mut self, rate: f64, now_ns: i64, _ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        match self.state {
            CarryState::Entered { direction, entry_ts_ns, cumulative_funding, .. } => {
                if rate.abs() < self.config.exit_threshold
                    || now_ns - entry_ts_ns > self.config.max_hold_ns
                    || cumulative_funding.abs() > self.config.max_adverse_funding
                {
                    self.state = CarryState::ExitSignaled;
                    return vec![self.make_intent(direction, "carry-v1 exit")];
                }
                Vec::new()
            }
            CarryState::EntrySignaled { entry_ts_ns, .. } => {
                if now_ns - entry_ts_ns > self.config.signal_timeout_ns {
                    self.state = CarryState::Idle;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

impl Strategy for CarryV1 {
    fn id(&self) -> StrategyId { self.id.clone() }
    fn universe(&self) -> Universe { self.universe.clone() }
    fn subscriptions(&self) -> Vec<String> { vec!["funding.*".into()] }
    fn warmup_ns(&self) -> i64 { 60_000_000_000 }
    fn declared_regime(&self) -> RegimeMask { RegimeMask::of(&["chop", "range-bound"]) }

    fn on_feature(&mut self, u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        let venue = self.universe.venues.first().copied().unwrap_or(Venue::Hyperliquid);
        if u.venue != venue || !self.universe.symbols.contains(&u.symbol) {
            return Vec::new();
        }
        self.funding.entry((u.venue, u.symbol))
            .or_insert(FundingState { last_rate: 0.0, last_ts_ns: 0 });
        match self.state {
            CarryState::Idle => self.check_entry(u.value, ctx),
            CarryState::EntrySignaled { .. } | CarryState::Entered { .. } => {
                self.check_exit(u.value, u.ts_ns, ctx)
            }
            CarryState::ExitSignaled => {
                self.state = CarryState::Idle;
                Vec::new()
            }
        }
    }

    fn on_fill(&mut self, fill: &mp_core::Fill, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        let _ = (fill, ctx);
        // On fill, transition EntrySignaled → Entered
        if let CarryState::EntrySignaled { direction, entry_value, entry_ts_ns } = self.state {
            self.state = CarryState::Entered {
                direction,
                entry_value,
                entry_ts_ns,
                cumulative_funding: 0.0,
            };
        }
        Vec::new()
    }

    fn params(&self) -> ParamSpace {
        let mut p = ParamSpace::default();
        p.grid.insert("entry_threshold".into(), vec![0.00005, 0.0001, 0.0002]);
        p.grid.insert("exit_threshold".into(), vec![0.00001, 0.00002, 0.00005]);
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::{Ctx, TimerId};

    struct TestCtx { now: i64, equity: f64, count: u64 }
    impl Ctx for TestCtx {
        fn now_ns(&self) -> i64 { self.now }
        fn position(&self, _: SymbolId) -> f64 { 0.0 }
        fn equity_allocated(&self) -> f64 { self.equity }
        fn next_u64(&mut self) -> u64 { self.count += 1; self.count }
        fn set_timer(&mut self, _: i64) -> TimerId { TimerId(0) }
        fn log(&mut self, _: &str) {}
    }

    fn make_update(value: f64, ts_ns: i64) -> FeatureUpdate {
        FeatureUpdate { feature: SymbolId(1), venue: Venue::Hyperliquid, symbol: SymbolId(1), ts_ns, value, ver: 1 }
    }

    #[test]
    fn str_9_carry_emits_intent_on_funding_extreme() {
        let mut s = CarryV1::new(
            StrategyId::new("carry-v1"),
            Universe { venues: vec![Venue::Hyperliquid], symbols: vec![SymbolId(1)] },
            CarryConfig { entry_threshold: 0.0001, ..Default::default() },
        );
        let mut ctx = TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 };
        let intents = s.on_feature(&make_update(0.0002, 1_000_000_000), &mut ctx);
        assert!(!intents.is_empty(), "should emit entry intent");
        assert_eq!(intents[0].side, Side::Sell);
        assert_eq!(intents[0].tag, "carry-v1 entry");
    }

    #[test]
    fn str_10_carry_exits_on_normalization() {
        let mut s = CarryV1::new(
            StrategyId::new("carry-v1"),
            Universe { venues: vec![Venue::Hyperliquid], symbols: vec![SymbolId(1)] },
            CarryConfig { entry_threshold: 0.0001, exit_threshold: 0.00002, ..Default::default() },
        );
        s.on_feature(&make_update(0.0002, 1_000_000_000), &mut TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 });
        s.state = CarryState::Entered { direction: Side::Sell, entry_value: 0.0002, entry_ts_ns: 1_000_000_000, cumulative_funding: 0.0 };
        let mut ctx = TestCtx { now: 2_000_000_000, equity: 1_000_000.0, count: 0 };
        let exit = s.on_feature(&make_update(0.00001, 2_000_000_000), &mut ctx);
        assert!(!exit.is_empty(), "should exit on normalization");
        assert_eq!(exit[0].tag, "carry-v1 exit");
    }

    #[test]
    fn str_11_carry_enters_on_negative_funding() {
        let mut s = CarryV1::new(
            StrategyId::new("carry-v1"),
            Universe { venues: vec![Venue::Hyperliquid], symbols: vec![SymbolId(1)] },
            CarryConfig { entry_threshold: 0.0001, ..Default::default() },
        );
        let mut ctx = TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 };
        let intents = s.on_feature(&make_update(-0.0002, 1_000_000_000), &mut ctx);
        assert!(!intents.is_empty(), "should emit entry intent for negative funding");
        assert_eq!(intents[0].side, Side::Buy); // negative funding → long
    }
}
