//! Funding-rate carry strategy (spec 015, `strategies/carry-v1/hypothesis.md`).
//! Monitors `funding.rate` per (venue, symbol); enters OPPOSITE the funding
//! sign (short when longs pay, long when shorts pay) once the rolling
//! |z-score| of recent funding is extreme; exits on normalization, a hard time
//! stop, live adverse-funding accrual past `max_adverse_funding`, or a funding
//! flip against the position beyond `z_flip` σ. v1 is single-venue and NAKED
//! (spec 015 defers the perp-spot hedge to Phase 1); the `hedge` field marks
//! the intended kind so the nakedness is explicit, never silent (audit C1).

use crate::strategy::{Ctx, ParamSpace, RegimeMask, Strategy, Universe};
use mp_core::{IntentId, OrderIntent, OrderKind, Side, SizeUnit, StrategyId, SymbolId, Venue};
use mp_features::FeatureUpdate;
use std::collections::{BTreeMap, VecDeque};

/// Per-unit risk the gate's sizer assumes (mirrors `risk::SizingParams`
/// `per_trade_risk_pct` default 0.005). Used to translate the strategy's
/// `vol_target` into a risk-unit count — the strategy itself never sizes in
/// contracts (RSK-1: strategies emit risk units; the gate owns contracts).
const PER_RISK_UNIT_PCT: f64 = 0.005;

/// Hedge kind for carry. v1 is `None` (single-venue naked perp); spec 015
/// defers the perp-spot hedge to Phase 1 (cross-venue execution, owner-gated).
/// The field exists so the exposure stance is explicit and auditable, and so
/// enabling the hedge later needs no semantics change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HedgeKind {
    /// Naked perp leg only (v1).
    None,
    /// Opposite-side spot leg on the same venue (Phase 1 — not yet executed).
    PerpSpot,
}

/// Configuration for the carry-v1 strategy.
#[derive(Debug, Clone, Copy)]
pub struct CarryConfig {
    /// |funding.rate| floor over which we may enter (default 0.01% = 0.0001).
    pub entry_threshold: f64,
    /// |funding.rate| below which we exit (default 0.002% = 0.00002).
    pub exit_threshold: f64,
    /// |z-score| at which we enter once the rolling window is warm (hypothesis:
    /// "entry at |funding z-score| ≥ threshold").
    pub z_entry: f64,
    /// |z-score| below which funding counts as normalized (used with the
    /// absolute `exit_threshold` — either triggers the exit).
    pub z_exit: f64,
    /// Funding flipped against us beyond this many σ → stop (hypothesis risk
    /// params: "stop on funding flip beyond 2 standard deviations").
    pub z_flip: f64,
    /// Rolling window of funding ticks for mean/σ (default 24 ≈ 8 days at 8h).
    pub z_window: usize,
    /// Ticks before the z-score is trusted; below this, the raw-rate floor
    /// alone gates entry (cold-start fallback).
    pub z_min_obs: usize,
    /// Annualized vol target for sizing (drives the risk-unit count; the risk
    /// gate converts to contracts using instrument vol).
    pub vol_target: f64,
    /// Max fraction of portfolio for this strategy (exposure cap).
    pub max_gross_exposure: f64,
    /// Max hold time in nanoseconds (default 14 days).
    pub max_hold_ns: i64,
    /// Max adverse funding accumulation before stop (default 2%). Units:
    /// summed adverse per-interval rates — the funding rate IS a fraction of
    /// notional per interval, so this is a %-of-notional proxy; the strategy
    /// has no mark/notional to convert exactly (noted in hypothesis risk
    /// params).
    pub max_adverse_funding: f64,
    /// Cancel signal if not filled within this many ns.
    pub signal_timeout_ns: i64,
    /// Clamp on the vol_target→risk-units mapping.
    pub max_risk_units: f64,
    /// Intended hedge kind (v1: `None`).
    pub hedge: HedgeKind,
}

impl Default for CarryConfig {
    fn default() -> Self {
        Self {
            entry_threshold: 0.0001,
            exit_threshold: 0.00002,
            z_entry: 2.0,
            z_exit: 0.5,
            z_flip: 2.0,
            z_window: 24,
            z_min_obs: 8,
            vol_target: 0.15,        // 15% annualized
            max_gross_exposure: 0.1, // 10% of portfolio
            max_hold_ns: 14 * 86_400 * 1_000_000_000, // 14 days
            max_adverse_funding: 0.02,
            signal_timeout_ns: 60 * 1_000_000_000, // 60 seconds
            max_risk_units: 16.0,
            hedge: HedgeKind::None,
        }
    }
}

/// Strategy state machine: IDLE → ENTRY_SIGNALED → ENTERED → EXIT_SIGNALED → IDLE.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CarryState {
    Idle,
    EntrySignaled {
        venue: Venue,
        symbol: SymbolId,
        direction: Side,
        entry_value: f64,
        entry_ts_ns: i64,
    },
    Entered {
        venue: Venue,
        symbol: SymbolId,
        direction: Side,
        entry_value: f64,
        entry_ts_ns: i64,
        /// Accumulated ADVERSE funding (we-pay rate sum) while held — live,
        /// updated per funding update so the stop is real (audit C1: this was
        /// frozen at 0, dead).
        cumulative_funding: f64,
    },
    ExitSignaled,
}

/// Rolling funding history for one venue/symbol pair.
struct FundingState {
    rates: VecDeque<f64>,
}

impl FundingState {
    fn new() -> Self {
        Self { rates: VecDeque::new() }
    }

    fn push(&mut self, rate: f64, window: usize) {
        self.rates.push_back(rate);
        while self.rates.len() > window {
            self.rates.pop_front();
        }
    }

    /// (mean, sample std) over the window; None when degenerate (n < 2, zero or
    /// non-finite variance) — z is then undefined and raw-rate fallback rules.
    fn stats(&self) -> Option<(f64, f64)> {
        let n = self.rates.len();
        if n < 2 {
            return None;
        }
        let mean = self.rates.iter().sum::<f64>() / n as f64;
        let var = self.rates.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
        let std = var.sqrt();
        if !std.is_finite() || std <= f64::EPSILON {
            None
        } else {
            Some((mean, std))
        }
    }

    fn z(&self, rate: f64) -> Option<f64> {
        self.stats().map(|(m, s)| (rate - m) / s)
    }
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
        Self {
            id,
            config,
            universe,
            funding: BTreeMap::new(),
            state: CarryState::Idle,
            next_intent: 1,
        }
    }

    /// vol_target → risk units (RSK-1: the strategy emits units; the gate's
    /// vol-sizer owns contracts). `vol_target` buys `vol_target / per-unit`
    /// risk-unit slots, clamped so a carry slot can never overshoot the gate's
    /// exposure checks (audit C1: `vol_target` was dead config).
    fn risk_units(&self) -> f64 {
        (self.config.vol_target / PER_RISK_UNIT_PCT).clamp(1.0, self.config.max_risk_units)
    }

    fn make_intent(&mut self, venue: Venue, symbol: SymbolId, side: Side, tag: &str) -> OrderIntent {
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

    /// Entry: funding extreme OPPOSITE our future position, on the z-score
    /// (hypothesis: "entry at |funding z-score| ≥ threshold"). No window
    /// baseline yet (cold) → NO entry: without history, "extreme" is
    /// undefined, and the raw-rate fallback is exactly the phantom-entry bug
    /// the audit called out. Warm window: |rate| must clear `entry_threshold`
    /// AND |z| must clear `z_entry` — a stable rate is not an extreme.
    fn check_entry(
        &mut self,
        venue: Venue,
        symbol: SymbolId,
        rate: f64,
        z: Option<f64>,
        warm: bool,
        ctx: &mut dyn Ctx,
    ) -> Vec<OrderIntent> {
        if !warm {
            return Vec::new();
        }
        if rate.abs() < self.config.entry_threshold {
            return Vec::new();
        }
        if z.map_or(true, |z| z.abs() < self.config.z_entry) {
            return Vec::new();
        }
        let direction = if rate > 0.0 { Side::Sell } else { Side::Buy };
        self.state = CarryState::EntrySignaled {
            venue,
            symbol,
            direction,
            entry_value: rate,
            entry_ts_ns: ctx.now_ns(),
        };
        vec![self.make_intent(venue, symbol, direction, "carry-v1 entry")]
    }

    fn check_exit(
        &mut self,
        venue: Venue,
        symbol: SymbolId,
        rate: f64,
        z: Option<f64>,
        warm: bool,
        now_ns: i64,
    ) -> Vec<OrderIntent> {
        match self.state {
            CarryState::Entered {
                direction,
                entry_ts_ns,
                cumulative_funding,
                ..
            } => {
                // Live adverse accrual per funding update (audit C1): a long
                // pays when rate > 0, a short pays when rate < 0.
                let adverse_this = match direction {
                    Side::Buy => rate.max(0.0),
                    Side::Sell => (-rate).max(0.0),
                };
                let cum = cumulative_funding + adverse_this;
                // Flip stop: funding turned AGAINST us by more than z_flip σ
                // (hypothesis risk params). For a short, that is rate dipping
                // z_flip σ below the window mean (−z); for a long, +z.
                let adverse_z = match direction {
                    Side::Buy => z,
                    Side::Sell => z.map(|z| -z),
                };
                let flip = warm && adverse_z.is_some_and(|z| z > self.config.z_flip);
                let exit = rate.abs() < self.config.exit_threshold
                    || now_ns - entry_ts_ns > self.config.max_hold_ns
                    || cum > self.config.max_adverse_funding
                    || flip;
                self.state = CarryState::Entered {
                    venue,
                    symbol,
                    direction,
                    entry_value: self.entry_value_of(),
                    entry_ts_ns,
                    cumulative_funding: cum,
                };
                if exit {
                    let exit_side = match direction {
                        Side::Buy => Side::Sell,
                        Side::Sell => Side::Buy,
                    };
                    self.state = CarryState::ExitSignaled;
                    return vec![self.make_intent(venue, symbol, exit_side, "carry-v1 exit")];
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

    fn entry_value_of(&self) -> f64 {
        match self.state {
            CarryState::Entered { entry_value, .. } => entry_value,
            CarryState::EntrySignaled { entry_value, .. } => entry_value,
            _ => 0.0,
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
        // Audit C1: this strategy trades funding — never interpret any other
        // feature's value as a funding rate, even if a dispatcher misroutes it.
        // Defense in depth: the sim also filters by `subscriptions()`.
        if !u.name.starts_with("funding.") {
            return Vec::new();
        }
        let venue = self.universe.venues.first().copied().unwrap_or(Venue::Hyperliquid);
        if u.venue != venue || !self.universe.symbols.contains(&u.symbol) {
            return Vec::new();
        }
        let (z, warm) = {
            let st = self.funding.entry((u.venue, u.symbol)).or_insert_with(FundingState::new);
            // z is the new observation's extreme-ness against the PRIOR
            // window — push after computing, or the point is its own baseline.
            let z = st.z(u.value);
            st.push(u.value, self.config.z_window);
            (z, st.rates.len() >= self.config.z_min_obs)
        };
        match self.state {
            CarryState::Idle => self.check_entry(u.venue, u.symbol, u.value, z, warm, ctx),
            CarryState::EntrySignaled { .. } | CarryState::Entered { .. } => {
                self.check_exit(u.venue, u.symbol, u.value, z, warm, u.ts_ns)
            }
            CarryState::ExitSignaled => {
                self.state = CarryState::Idle;
                Vec::new()
            }
        }
    }

    fn on_fill(&mut self, fill: &mp_core::Fill, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        let _ = (fill, ctx);
        // On fill, transition EntrySignaled → Entered (accrual starts at 0).
        if let CarryState::EntrySignaled {
            venue,
            symbol,
            direction,
            entry_value,
            entry_ts_ns,
        } = self.state
        {
            self.state = CarryState::Entered {
                venue,
                symbol,
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
        p.grid.insert("z_entry".into(), vec![1.5, 2.0, 2.5]);
        p
    }

    fn with_params(&self, params: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
        let mut cfg = self.config;
        if let Some(&v) = params.get("entry_threshold") { cfg.entry_threshold = v; }
        if let Some(&v) = params.get("exit_threshold") { cfg.exit_threshold = v; }
        if let Some(&v) = params.get("z_entry") { cfg.z_entry = v; }
        Box::new(CarryV1::new(self.id.clone(), self.universe.clone(), cfg))
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
        FeatureUpdate {
            feature: SymbolId(1),
            name: "funding.rate".into(),
            venue: Venue::Hyperliquid,
            symbol: SymbolId(1),
            ts_ns,
            value,
            ver: 1,
        }
    }

    fn make_any_update(value: f64, name: &str) -> FeatureUpdate {
        FeatureUpdate {
            feature: SymbolId(1),
            name: name.into(),
            venue: Venue::Hyperliquid,
            symbol: SymbolId(1),
            ts_ns: 1,
            value,
            ver: 1,
        }
    }

    fn strat() -> CarryV1 {
        CarryV1::new(
            StrategyId::new("carry-v1"),
            Universe { venues: vec![Venue::Hyperliquid], symbols: vec![SymbolId(1)] },
            CarryConfig::default(),
        )
    }

    #[test]
    fn str_9_carry_emits_intent_on_funding_extreme() {
        let mut s = strat();
        let mut ctx = TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 };
        // Warm the window with sub-threshold ticks before the extreme.
        for (i, r) in [0.00001, 0.00002, 0.00001, 0.00002, 0.00001, 0.00002, 0.00001, 0.00002].iter().enumerate() {
            assert!(s.on_feature(&make_update(*r, 1_000_000_000 + i as i64), &mut ctx).is_empty());
        }
        let intents = s.on_feature(&make_update(0.0002, 2_000_000_000), &mut ctx);
        assert!(!intents.is_empty(), "should emit entry intent");
        assert_eq!(intents[0].side, Side::Sell);
        assert_eq!(intents[0].tag, "carry-v1 entry");
    }

    #[test]
    fn str_10_carry_exits_on_normalization() {
        let mut s = strat();
        s.on_feature(&make_update(0.0002, 1_000_000_000), &mut TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 });
        s.state = CarryState::Entered {
            venue: Venue::Hyperliquid,
            symbol: SymbolId(1),
            direction: Side::Sell,
            entry_value: 0.0002,
            entry_ts_ns: 1_000_000_000,
            cumulative_funding: 0.0,
        };
        let mut ctx = TestCtx { now: 2_000_000_000, equity: 1_000_000.0, count: 0 };
        let exit = s.on_feature(&make_update(0.00001, 2_000_000_000), &mut ctx);
        assert!(!exit.is_empty(), "should exit on normalization");
        assert_eq!(exit[0].tag, "carry-v1 exit");
    }

    #[test]
    fn str_11_carry_enters_on_negative_funding() {
        let mut s = strat();
        let mut ctx = TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 };
        for (i, r) in [-0.00001, -0.00002, -0.00001, -0.00002, -0.00001, -0.00002, -0.00001, -0.00002].iter().enumerate() {
            assert!(s.on_feature(&make_update(*r, 1_000_000_000 + i as i64), &mut ctx).is_empty());
        }
        let intents = s.on_feature(&make_update(-0.0002, 2_000_000_000), &mut ctx);
        assert!(!intents.is_empty(), "should emit entry intent for negative funding");
        assert_eq!(intents[0].side, Side::Buy); // negative funding → long
    }

    #[test]
    fn str_12_carry_ignores_non_funding_features() {
        // Audit C1: a CVD (or any non-funding) update with an extreme value
        // must never be read as a funding rate.
        let mut s = strat();
        let mut ctx = TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 };
        let intents = s.on_feature(&make_any_update(0.0002, "cvd.net_volume"), &mut ctx);
        assert!(intents.is_empty(), "non-funding feature must not trigger carry");
        assert_eq!(s.state, CarryState::Idle);
    }

    #[test]
    fn str_13_carry_enters_on_z_score_extreme_after_warmup() {
        // Warm window (8 ticks, mean ≈ 0.00025, σ ≈ 5.3e-5): a 0.0005 rate is
        // |z| ≈ 4.7 ≥ z_entry (2.0) → entry. A 0.0003 rate (|z| ≈ 0.9) → none.
        let mut s = strat();
        let mut ctx = TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 };
        let warm = [0.0002, 0.0003, 0.0002, 0.0003, 0.0002, 0.0003, 0.0002, 0.0003];
        for (i, r) in warm.iter().enumerate() {
            assert!(
                s.on_feature(&make_update(*r, 1_000_000_000 + i as i64 * 1000), &mut ctx).is_empty(),
                "warmup ticks must not enter"
            );
        }
        // A moderate rate within the window distribution: no entry.
        let none = s.on_feature(&make_update(0.0003, 2_000_000_000), &mut ctx);
        assert!(none.is_empty(), "sub-threshold z must not enter");
        // An extreme rate: z-score entry fires.
        let intents = s.on_feature(&make_update(0.0005, 3_000_000_000), &mut ctx);
        assert!(!intents.is_empty(), "|z| ≥ z_entry must enter");
        assert_eq!(intents[0].side, Side::Sell);
    }

    #[test]
    fn str_14_carry_adverse_funding_accrues_and_stops() {
        // Audit C1: the funding stop must be live. A short (Sell) pays when
        // rate < 0; two -0.012 updates exceed max_adverse_funding 0.02 → exit.
        let cfg = CarryConfig { max_adverse_funding: 0.02, z_flip: 1e9, ..Default::default() };
        let mut s = CarryV1::new(
            StrategyId::new("carry-v1"),
            Universe { venues: vec![Venue::Hyperliquid], symbols: vec![SymbolId(1)] },
            cfg,
        );
        let mut ctx = TestCtx { now: 1_000_000_000, equity: 1_000_000.0, count: 0 };
        for (i, r) in [0.00001, 0.00002, 0.00001, 0.00002, 0.00001, 0.00002, 0.00001, 0.00002].iter().enumerate() {
            s.on_feature(&make_update(*r, 1_000_000_000 + i as i64), &mut ctx);
        }
        s.on_feature(&make_update(0.0002, 2_000_000_000), &mut ctx); // entry signal
        s.on_fill(&mp_core::Fill {
            intent_id: IntentId(1),
            symbol: SymbolId(1),
            side: Side::Sell,
            price: 100.0,
            qty: 1.0,
            fee: 0.0,
            liquidity: mp_core::Liquidity::Taker,
            ts_ns: 2_000_000_000,
        }, &mut ctx);
        assert_eq!(
            s.on_feature(&make_update(-0.012, 3_000_000_000), &mut ctx),
            Vec::new(),
            "below the 0.02 adverse cap"
        );
        let exit = s.on_feature(&make_update(-0.012, 4_000_000_000), &mut ctx);
        assert!(!exit.is_empty(), "adverse funding accumulation must stop the trade");
        assert_eq!(exit[0].tag, "carry-v1 exit");
    }

    #[test]
    fn str_15_carry_uses_vol_target_in_sizing() {
        let s = strat();
        let units = s.risk_units();
        assert!(units >= 1.0, "risk units must be ≥ 1, got {units}");
        assert!((units - (0.15 / PER_RISK_UNIT_PCT).min(16.0)).abs() < 1e-9);
        let big = CarryV1::new(
            StrategyId::new("carry-v1"),
            Universe { venues: vec![Venue::Hyperliquid], symbols: vec![SymbolId(1)] },
            CarryConfig { vol_target: 0.30, max_risk_units: 64.0, ..Default::default() },
        );
        assert!(big.risk_units() > units, "higher vol_target must request more units");
    }
}
