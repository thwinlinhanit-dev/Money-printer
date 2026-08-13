//! Order-flow imbalance strategy (spec 004/006, `strategies/orderflow-v1/
//! hypothesis.md`). Joins a push when the near-book depth gauge
//! (`book.depth.0.5`) and the aggressive tape (`tape.bps_delta`) ALIGN in
//! sign and magnitude — the Cryexc/OpenMarket "fake vs real move" heuristic:
//! a move with flow behind it and no wall in front of it tends to continue
//! over the short horizon. Exits when the imbalance neutralizes, flips
//! against the position, or the time stop fires. v1 is single-venue
//! (hyperliquid), market-intent only, no scaling.

use crate::strategy::{Ctx, ParamSpace, RegimeMask, Strategy, Universe};
use mp_core::{IntentId, OrderIntent, OrderKind, Side, SizeUnit, StrategyId, SymbolId, Venue};
use mp_features::FeatureUpdate;
use std::collections::BTreeMap;

/// Per-unit risk the gate's sizer assumes (mirrors `risk::SizingParams`
/// `per_trade_risk_pct` default 0.005) — same convention as carry-v1
/// (RSK-1: strategies emit risk units; the gate owns contracts).
const PER_RISK_UNIT_PCT: f64 = 0.005;

/// Configuration for the orderflow-v1 strategy.
#[derive(Debug, Clone, Copy)]
pub struct OrderflowConfig {
    /// |gauge| floor for entry (default 0.3 — 30% depth imbalance).
    pub entry_gauge: f64,
    /// |gauge| below which an open position exits (neutralized).
    pub exit_gauge: f64,
    /// |tape.bps_delta| floor for entry confirmation (default 1.0 bps; the
    /// feature itself emits only >= 0.5 bps).
    pub min_tape_bps: f64,
    /// `book.depth_total.0.5` floor — the gauge must not be dust-driven.
    pub min_depth: f64,
    /// A tape print older than this (ns) no longer confirms an entry.
    pub confirm_window_ns: i64,
    /// A gauge reading older than this (ns) is stale for entry purposes.
    pub gauge_stale_ns: i64,
    /// Max hold time in nanoseconds (default 4 hours).
    pub max_hold_ns: i64,
    /// Cancel signal if not filled within this many ns.
    pub signal_timeout_ns: i64,
    /// Annualized vol target for sizing (risk-unit count; the risk gate owns
    /// contracts).
    pub vol_target: f64,
    /// Max fraction of portfolio for this strategy.
    pub max_gross_exposure: f64,
    /// Clamp on the vol_target → risk-units mapping.
    pub max_risk_units: f64,
}

impl Default for OrderflowConfig {
    fn default() -> Self {
        Self {
            entry_gauge: 0.3,
            exit_gauge: 0.05,
            min_tape_bps: 1.0,
            min_depth: 100_000.0,
            confirm_window_ns: 5 * 1_000_000_000,
            gauge_stale_ns: 30 * 1_000_000_000,
            max_hold_ns: 4 * 3_600 * 1_000_000_000,
            signal_timeout_ns: 60 * 1_000_000_000,
            vol_target: 0.15,
            max_gross_exposure: 0.1,
            max_risk_units: 16.0,
        }
    }
}

/// Strategy state machine: IDLE → ENTRY_SIGNALED → ENTERED → EXIT_SIGNALED → IDLE.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OrderflowState {
    Idle,
    EntrySignaled {
        venue: Venue,
        symbol: SymbolId,
        direction: Side,
        entry_ts_ns: i64,
    },
    Entered {
        venue: Venue,
        symbol: SymbolId,
        direction: Side,
        entry_ts_ns: i64,
    },
    ExitSignaled,
}

/// Last observed feature readings for one (venue, symbol).
#[derive(Debug, Default)]
struct SymState {
    gauge: Option<(i64, f64)>,
    depth_total: Option<(i64, f64)>,
    tape: Option<(i64, f64)>,
}

/// The order-flow imbalance strategy.
pub struct OrderflowV1 {
    id: StrategyId,
    config: OrderflowConfig,
    universe: Universe,
    per_sym: BTreeMap<(Venue, SymbolId), SymState>,
    state: OrderflowState,
    next_intent: u128,
}

impl OrderflowV1 {
    pub fn new(id: StrategyId, universe: Universe, config: OrderflowConfig) -> Self {
        Self {
            id,
            config,
            universe,
            per_sym: BTreeMap::new(),
            state: OrderflowState::Idle,
            next_intent: 1,
        }
    }

    fn risk_units(&self) -> f64 {
        (self.config.vol_target / PER_RISK_UNIT_PCT).clamp(1.0, self.config.max_risk_units)
    }

    fn make_intent(
        &mut self,
        venue: Venue,
        symbol: SymbolId,
        side: Side,
        tag: &str,
    ) -> OrderIntent {
        let iid = self.next_intent;
        self.next_intent += 1;
        OrderIntent {
            intent_id: IntentId(iid),
            strategy: self.id.clone(),
            venue,
            symbol,
            side,
            kind: OrderKind::Market,
            qty: SizeUnit::RiskUnits(self.risk_units()),
            tif: mp_core::TimeInForce::Ioc,
            reduce_only: false,
            tag: tag.into(),
        }
    }

    /// Idle → entry: gauge and tape recent, aligned in sign, strong enough,
    /// and the band thick enough that the gauge is not dust-driven.
    fn check_entry(&mut self, venue: Venue, symbol: SymbolId, now_ns: i64) -> Vec<OrderIntent> {
        let cfg = self.config;
        let Some(ss) = self.per_sym.get(&(venue, symbol)) else {
            return Vec::new();
        };
        let Some((gts, gauge)) = ss.gauge else {
            return Vec::new();
        };
        let Some((tts, tape)) = ss.tape else {
            return Vec::new();
        };
        let Some((_, depth)) = ss.depth_total else {
            return Vec::new();
        };
        if now_ns - gts > cfg.gauge_stale_ns || now_ns - tts > cfg.confirm_window_ns {
            return Vec::new();
        }
        if depth < cfg.min_depth || gauge.abs() < cfg.entry_gauge || tape.abs() < cfg.min_tape_bps {
            return Vec::new();
        }
        if (gauge > 0.0) != (tape > 0.0) {
            return Vec::new();
        }
        let direction = if gauge > 0.0 { Side::Buy } else { Side::Sell };
        self.state = OrderflowState::EntrySignaled {
            venue,
            symbol,
            direction,
            entry_ts_ns: now_ns,
        };
        vec![self.make_intent(venue, symbol, direction, "orderflow-v1 entry")]
    }

    /// Entered → exit: imbalance neutralized, flipped against us, or time stop.
    fn check_exit(&mut self, venue: Venue, symbol: SymbolId, now_ns: i64) -> Vec<OrderIntent> {
        let cfg = self.config;
        let Some(ss) = self.per_sym.get(&(venue, symbol)) else {
            return Vec::new();
        };
        let OrderflowState::Entered {
            direction,
            entry_ts_ns,
            ..
        } = self.state
        else {
            return Vec::new();
        };
        let gauge = ss.gauge.map(|(_, v)| v);
        let neutral = gauge.is_some_and(|g| g.abs() < cfg.exit_gauge);
        // Flipped: gauge sign no longer matches the position direction.
        let flip = gauge.is_some_and(|g| (g > 0.0) != (direction == Side::Buy));
        let time = now_ns - entry_ts_ns > cfg.max_hold_ns;
        if neutral || flip || time {
            let exit_side = match direction {
                Side::Buy => Side::Sell,
                Side::Sell => Side::Buy,
            };
            self.state = OrderflowState::ExitSignaled;
            return vec![self.make_intent(venue, symbol, exit_side, "orderflow-v1 exit")];
        }
        Vec::new()
    }
}

impl Strategy for OrderflowV1 {
    fn id(&self) -> StrategyId {
        self.id.clone()
    }
    fn universe(&self) -> Universe {
        self.universe.clone()
    }
    fn subscriptions(&self) -> Vec<String> {
        vec![
            "book.depth.".into(),
            "book.depth_total.".into(),
            "tape.bps_delta".into(),
        ]
    }
    fn warmup_ns(&self) -> i64 {
        60_000_000_000
    }
    fn declared_regime(&self) -> RegimeMask {
        RegimeMask::any()
    }

    fn on_feature(&mut self, u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        // Defense in depth (audit C1 pattern): never read a non-orderflow
        // feature's value as a gauge/tape reading, even if a dispatcher
        // misroutes it. NOTE: `book.depth_total.*` does NOT start with
        // `book.depth.` (underscore vs dot) — both prefixes must be admitted.
        if !(u.name.starts_with("book.depth.")
            || u.name.starts_with("book.depth_total.")
            || u.name.starts_with("tape.bps_delta"))
        {
            return Vec::new();
        }
        if u.venue != self.universe.venues.first().copied().unwrap_or(Venue::Hyperliquid)
            || !self.universe.symbols.contains(&u.symbol)
        {
            return Vec::new();
        }
        let ss = self.per_sym.entry((u.venue, u.symbol)).or_default();
        if u.name.starts_with("book.depth.") && !u.name.starts_with("book.depth_total.") {
            ss.gauge = Some((u.ts_ns, u.value));
        } else if u.name.starts_with("book.depth_total.") {
            ss.depth_total = Some((u.ts_ns, u.value));
        } else if u.name == "tape.bps_delta" {
            ss.tape = Some((u.ts_ns, u.value));
        }
        let now = ctx.now_ns();
        match self.state {
            OrderflowState::Idle => self.check_entry(u.venue, u.symbol, now),
            OrderflowState::EntrySignaled { entry_ts_ns, .. } => {
                if now - entry_ts_ns > self.config.signal_timeout_ns {
                    self.state = OrderflowState::Idle;
                }
                Vec::new()
            }
            OrderflowState::Entered { .. } => self.check_exit(u.venue, u.symbol, now),
            OrderflowState::ExitSignaled => {
                self.state = OrderflowState::Idle;
                Vec::new()
            }
        }
    }

    fn on_fill(&mut self, fill: &mp_core::Fill, _ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        if let OrderflowState::EntrySignaled {
            venue,
            symbol,
            direction,
            entry_ts_ns,
        } = self.state
        {
            self.state = OrderflowState::Entered {
                venue,
                symbol,
                direction,
                entry_ts_ns,
            };
        }
        let _ = fill;
        Vec::new()
    }

    fn params(&self) -> ParamSpace {
        let mut p = ParamSpace::default();
        p.grid.insert("entry_gauge".into(), vec![0.2, 0.3, 0.4]);
        p.grid.insert("min_tape_bps".into(), vec![0.5, 1.0, 2.0]);
        p.grid.insert("min_depth".into(), vec![50_000.0, 100_000.0, 250_000.0]);
        p
    }

    fn with_params(&self, params: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
        let mut cfg = self.config;
        if let Some(&v) = params.get("entry_gauge") {
            cfg.entry_gauge = v;
        }
        if let Some(&v) = params.get("min_tape_bps") {
            cfg.min_tape_bps = v;
        }
        if let Some(&v) = params.get("min_depth") {
            cfg.min_depth = v;
        }
        Box::new(OrderflowV1::new(self.id.clone(), self.universe.clone(), cfg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::{Ctx, TimerId};

    struct TestCtx {
        now: i64,
        count: u64,
    }
    impl Ctx for TestCtx {
        fn now_ns(&self) -> i64 {
            self.now
        }
        fn position(&self, _: SymbolId) -> f64 {
            0.0
        }
        fn equity_allocated(&self) -> f64 {
            1_000_000.0
        }
        fn next_u64(&mut self) -> u64 {
            self.count += 1;
            self.count
        }
        fn set_timer(&mut self, _: i64) -> TimerId {
            TimerId(0)
        }
        fn log(&mut self, _: &str) {}
    }

    fn up(name: &str, value: f64, ts: i64) -> FeatureUpdate {
        FeatureUpdate {
            feature: SymbolId(1),
            name: name.into(),
            venue: Venue::Hyperliquid,
            symbol: SymbolId(1),
            ts_ns: ts,
            value,
            ver: 1,
        }
    }

    fn strat() -> OrderflowV1 {
        OrderflowV1::new(
            StrategyId::new("orderflow-v1"),
            Universe {
                venues: vec![Venue::Hyperliquid],
                symbols: vec![SymbolId(1)],
            },
            OrderflowConfig::default(),
        )
    }

    fn ctx(now: i64) -> TestCtx {
        TestCtx { now, count: 0 }
    }

    #[test]
    fn of_1_enters_long_on_aligned_gauge_and_tape() {
        let mut s = strat();
        let mut c = ctx(1_000_000_000);
        // Bid-dominant depth (gauge +0.5) + aggressive buying (tape +2 bps).
        assert!(s.on_feature(&up("book.depth.0.5", 0.5, 900_000_000), &mut c).is_empty());
        assert!(s
            .on_feature(&up("book.depth_total.0.5", 500_000.0, 950_000_000), &mut c)
            .is_empty());
        let intents = s.on_feature(&up("tape.bps_delta", 2.0, 1_000_000_000), &mut c);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].side, Side::Buy);
        assert_eq!(intents[0].tag, "orderflow-v1 entry");
    }

    #[test]
    fn of_2_enters_short_on_mirrored_imbalance() {
        let mut s = strat();
        let mut c = ctx(1_000_000_000);
        assert!(s.on_feature(&up("book.depth.0.5", -0.5, 900_000_000), &mut c).is_empty());
        assert!(s
            .on_feature(&up("book.depth_total.0.5", 500_000.0, 950_000_000), &mut c)
            .is_empty());
        let intents = s.on_feature(&up("tape.bps_delta", -2.0, 1_000_000_000), &mut c);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].side, Side::Sell);
    }

    #[test]
    fn of_3_no_entry_on_gauge_alone_or_misaligned_tape() {
        let mut s = strat();
        let mut c = ctx(1_000_000_000);
        s.on_feature(&up("book.depth.0.5", 0.5, 900_000_000), &mut c);
        s.on_feature(&up("book.depth_total.0.5", 500_000.0, 950_000_000), &mut c);
        // No tape yet.
        assert!(s.on_feature(&up("book.depth.0.5", 0.6, 1_000_000_000), &mut c).is_empty());
        // Misaligned tape: bids dominate but tape is selling.
        let none = s.on_feature(&up("tape.bps_delta", -2.0, 1_000_000_000), &mut c);
        assert!(none.is_empty(), "misaligned tape must not enter");
    }

    #[test]
    fn of_4_tape_stale_blocks_entry() {
        let mut s = strat();
        let mut c = ctx(1_000_000_000);
        s.on_feature(&up("book.depth.0.5", 0.5, 900_000_000), &mut c);
        s.on_feature(&up("book.depth_total.0.5", 500_000.0, 950_000_000), &mut c);
        // Tape observed 6s ago > confirm_window_ns (5s) → stale, no entry.
        s.on_feature(&up("tape.bps_delta", 2.0, 994_000_000), &mut c);
        assert!(s.on_feature(&up("book.depth.0.5", 0.5, 1_000_000_000), &mut c).is_empty());
    }

    #[test]
    fn of_5_exits_on_neutralized_gauge() {
        let mut s = strat();
        let mut c = ctx(1_000_000_000);
        s.on_feature(&up("book.depth.0.5", 0.5, 900_000_000), &mut c);
        s.on_feature(&up("book.depth_total.0.5", 500_000.0, 950_000_000), &mut c);
        let intents = s.on_feature(&up("tape.bps_delta", 2.0, 1_000_000_000), &mut c);
        assert_eq!(intents.len(), 1);
        s.on_fill(
            &mp_core::Fill {
                intent_id: IntentId(1),
                symbol: SymbolId(1),
                side: Side::Buy,
                price: 100.0,
                qty: 1.0,
                fee: 0.0,
                liquidity: mp_core::Liquidity::Taker,
                ts_ns: 1_000_000_000,
            },
            &mut c,
        );
        // Gauge neutralizes below exit_gauge (0.05) → exit.
        let exit = s.on_feature(&up("book.depth.0.5", 0.01, 2_000_000_000), &mut c);
        assert_eq!(exit.len(), 1);
        assert_eq!(exit[0].tag, "orderflow-v1 exit");
        assert_eq!(exit[0].side, Side::Sell);
    }

    #[test]
    fn of_6_exits_on_gauge_flip() {
        let mut s = strat();
        let mut c = ctx(1_000_000_000);
        s.on_feature(&up("book.depth.0.5", 0.5, 900_000_000), &mut c);
        s.on_feature(&up("book.depth_total.0.5", 500_000.0, 950_000_000), &mut c);
        s.on_feature(&up("tape.bps_delta", 2.0, 1_000_000_000), &mut c);
        s.on_fill(
            &mp_core::Fill {
                intent_id: IntentId(1),
                symbol: SymbolId(1),
                side: Side::Buy,
                price: 100.0,
                qty: 1.0,
                fee: 0.0,
                liquidity: mp_core::Liquidity::Taker,
                ts_ns: 1_000_000_000,
            },
            &mut c,
        );
        // Gauge flips negative against the long → exit.
        let exit = s.on_feature(&up("book.depth.0.5", -0.4, 2_000_000_000), &mut c);
        assert_eq!(exit.len(), 1);
        assert_eq!(exit[0].side, Side::Sell);
    }

    #[test]
    fn of_7_ignores_non_orderflow_features() {
        // Audit C1 pattern: an extreme value on an unrelated feature (funding,
        // cvd) must never be read as gauge/tape and must not enter.
        let mut s = strat();
        let mut c = ctx(1_000_000_000);
        let none = s.on_feature(&up("funding.rate", 0.5, 1_000_000_000), &mut c);
        assert!(none.is_empty());
        assert_eq!(s.state, OrderflowState::Idle);
    }
}
