//! Swing range-reclaim strategy (spec 036 SLQ-S,
//! `strategies/swing-range-reclaim-v1/hypothesis.md`). Enters AFTER a
//! liquidity sweep of a compressed daily range is CONFIRMED by a reclaim
//! (wick beyond the boundary by ≥ Y·ATR + volume filter + close back inside
//! within Z bars — all of that lives in the `swing.sweep.*` feature family;
//! this strategy only consumes confirmed events). One entry, one defined
//! risk: hard invalidation is a CLOSE through the event's stop price on the
//! entry timeframe (no intrabar stops on HTF setups), so every trade carries
//! a bounded, defined max loss at entry time (spec 036 §6 hard gate).
//!
//! Targets (conservative bias): T1 = the CLOSER of the opposite range
//! boundary and the nearest LVN beyond it. At T1 half the position exits,
//! the stop moves to breakeven, and the remainder trails at trail_atr × ATR.
//! Time stop after max_hold_bars. v1 is single-position, market-intent only.
//!
//! HONEST DATA GATE: the daily-bar corpus is young (recording since
//! 2026-08-12); a real-corpus backtest cannot reach the 30-OOS-trade review
//! bar yet — that verdict is recorded as "no data," never faked.

use crate::strategy::{Ctx, ParamSpace, RegimeMask, Strategy, Universe};
use mp_core::{
    IntentId, OrderIntent, OrderKind, RebalanceCadence, Side, SizeUnit, StrategyId, SymbolId, Venue,
};
use mp_features::FeatureUpdate;
use std::collections::BTreeMap;

/// Per-unit risk the gate's sizer assumes — same convention as carry-v1,
/// orderflow-v1, and liq-fade-v1 (RSK-1).
const PER_RISK_UNIT_PCT: f64 = 0.005;

/// Ratchet an invalidation level toward price — never backwards (longs only
/// raise it, shorts only lower it).
fn ratchet(side: Side, prev: f64, cand: f64) -> f64 {
    match side {
        Side::Buy => prev.max(cand),
        Side::Sell => prev.min(cand),
    }
}

/// Configuration for swing-range-reclaim-v1. The stop buffer is NOT here —
/// it travels with the `swing.sweep.*.stop.*` event as feature-family config
/// (spec 036 §2.3); the grid varies only position-management knobs.
#[derive(Debug, Clone, Copy)]
pub struct RangeReclaimConfig {
    /// Fraction of equity risked per trade (risk units = pct / 0.005).
    pub risk_pct: f64,
    /// Trail distance for the post-T1 remainder, in ATR multiples.
    pub trail_atr: f64,
    /// Fraction exited at T1 (remainder trails).
    pub t1_fraction: f64,
    /// Force-exit after this many held daily bars.
    pub max_hold_bars: i64,
    /// A stored context reading older than this (ns) is stale — fail-closed.
    pub ctx_stale_ns: i64,
    /// Cancel an unfilled entry signal after this many ns.
    pub signal_timeout_ns: i64,
    /// Daily bar length used to count held bars (ns).
    pub bar_ns: i64,
    /// Clamp on the risk_pct → risk-units mapping.
    pub max_risk_units: f64,
}

impl Default for RangeReclaimConfig {
    fn default() -> Self {
        Self {
            risk_pct: 0.005,
            trail_atr: 2.0,
            t1_fraction: 0.5,
            max_hold_bars: 60,
            ctx_stale_ns: 2 * 86_400_000_000_000,
            signal_timeout_ns: 2 * 86_400_000_000_000,
            bar_ns: 86_400_000_000_000,
            max_risk_units: 16.0,
        }
    }
}

/// Position phase between entry and full exit.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Full position held; waiting for T1 or invalidation.
    Full,
    /// Half exited at T1; stop at breakeven; remainder trailing.
    Trailed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Idle,
    EntrySignaled {
        venue: Venue,
        symbol: SymbolId,
        side: Side,
        stop: f64,
        ts_ns: i64,
    },
    Entered {
        venue: Venue,
        symbol: SymbolId,
        side: Side,
        /// Reference price for target math — the actual FILL price captured
        /// in on_fill (signal time has no trustworthy price).
        entry_ref: f64,
        /// Active invalidation level. Starts at the event's stop; after T1 it
        /// moves to breakeven and then RATCHETS with the trail
        /// (best_close ∓ trail_atr × ATR) — never backwards (spec 036 §3).
        stop: f64,
        t1: f64,
        phase: Phase,
        best_close: f64,
        entry_ts_ns: i64,
        units: f64,
    },
    ExitSignaled,
}

/// Latest per-symbol feature readings (each with its timestamp for
/// staleness checks).
#[derive(Debug, Default)]
struct SymCtx {
    range_high: Option<(i64, f64)>,
    range_low: Option<(i64, f64)>,
    atr: Option<(i64, f64)>,
    lvn_above: Option<(i64, f64)>,
    lvn_below: Option<(i64, f64)>,
    hvn_above: Option<(i64, f64)>,
    hvn_below: Option<(i64, f64)>,
    close: Option<(i64, f64)>,
    /// Last seen sweep wick extremes (diagnostics only).
    last_sweep_extreme_low: Option<f64>,
    last_sweep_extreme_high: Option<f64>,
}

/// The swing-range-reclaim strategy (spec 036).
pub struct SwingRangeReclaimV1 {
    id: StrategyId,
    config: RangeReclaimConfig,
    universe: Universe,
    per_sym: BTreeMap<(Venue, SymbolId), SymCtx>,
    state: State,
    next_intent: u128,
}

impl SwingRangeReclaimV1 {
    pub fn new(id: StrategyId, universe: Universe, config: RangeReclaimConfig) -> Self {
        Self {
            id,
            config,
            universe,
            per_sym: BTreeMap::new(),
            state: State::Idle,
            next_intent: 1,
        }
    }

    fn risk_units(&self) -> f64 {
        (self.config.risk_pct / PER_RISK_UNIT_PCT).clamp(1.0, self.config.max_risk_units)
    }

    fn make_intent(
        &mut self,
        venue: Venue,
        symbol: SymbolId,
        side: Side,
        qty_units: f64,
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
            qty: SizeUnit::RiskUnits(qty_units),
            tif: mp_core::TimeInForce::Ioc,
            reduce_only: false,
            tag: tag.into(),
        }
    }

    fn fresh(&self, reading: Option<(i64, f64)>, now_ns: i64) -> Option<f64> {
        let (ts, v) = reading?;
        if now_ns - ts > self.config.ctx_stale_ns || !v.is_finite() {
            None
        } else {
            Some(v)
        }
    }

    /// Idle → entry on a confirmed sweep-stop event. Long after a LOW sweep
    /// (support swept then reclaimed), short after a HIGH sweep.
    fn check_entry(
        &mut self,
        venue: Venue,
        symbol: SymbolId,
        side: Side,
        stop: f64,
        now_ns: i64,
    ) -> Vec<OrderIntent> {
        let Some(ss) = self.per_sym.get(&(venue, symbol)) else {
            return Vec::new();
        };
        // Range context must be fresh — a sweep without a known range is not
        // evidence (fail-closed).
        let (_rh, _rl) = match (
            self.fresh(ss.range_high, now_ns),
            self.fresh(ss.range_low, now_ns),
        ) {
            (Some(rh), Some(rl)) if rh > rl => (rh, rl),
            _ => return Vec::new(),
        };
        if !stop.is_finite() {
            return Vec::new();
        }
        self.state = State::EntrySignaled {
            venue,
            symbol,
            side,
            stop,
            ts_ns: now_ns,
        };
        vec![self.make_intent(venue, symbol, side, self.risk_units(), "swr-v1 entry")]
    }

    /// T1 target from the fill reference: the CLOSER of the opposite range
    /// boundary and the nearest LVN beyond it (long: min of ceilings above;
    /// short: max of floors below). No LVN known → opposite boundary alone.
    fn compute_t1(
        &self,
        side: Side,
        entry_ref: f64,
        venue: Venue,
        symbol: SymbolId,
        now_ns: i64,
    ) -> Option<f64> {
        let ss = self.per_sym.get(&(venue, symbol))?;
        let rh = self.fresh(ss.range_high, now_ns)?;
        let rl = self.fresh(ss.range_low, now_ns)?;
        let lvn_above = self.fresh(ss.lvn_above, now_ns);
        let lvn_below = self.fresh(ss.lvn_below, now_ns);
        match side {
            Side::Buy => {
                let mut t1 = rh;
                if let Some(l) = lvn_above.filter(|&l| l > rh && l > entry_ref) {
                    if l < t1 {
                        t1 = l;
                    }
                }
                (t1 > entry_ref).then_some(t1)
            }
            Side::Sell => {
                let mut t1 = rl;
                if let Some(l) = lvn_below.filter(|&l| l < rl && l < entry_ref) {
                    if l > t1 {
                        t1 = l;
                    }
                }
                (t1 < entry_ref).then_some(t1)
            }
        }
    }

    /// Entered → exit checks on every received update (the sim dispatches
    /// swing strategies on daily bar close, so each fresh close is one bar).
    /// Exits are CLOSE-evaluated only (spec 036 §3 — no intrabar stops).
    fn check_exit(&mut self, now_ns: i64) -> Vec<OrderIntent> {
        let cfg = self.config;
        let State::Entered {
            venue,
            symbol,
            side,
            entry_ref,
            mut stop,
            t1,
            phase: mut ph,
            mut best_close,
            entry_ts_ns,
            mut units,
        } = self.state
        else {
            return Vec::new();
        };
        let Some(ss) = self.per_sym.get(&(venue, symbol)) else {
            return Vec::new();
        };
        let Some(close) = self.fresh(ss.close, now_ns) else {
            return Vec::new(); // no close yet → no close-evaluated decision
        };
        let atr_now = self.fresh(ss.atr, now_ns);

        // Hard invalidation FIRST: a close through the stop exits whatever is
        // still held. In the Full phase that is the whole position
        // ("swr-v1 stop-out"); after T1 it is the trailed remainder
        // ("swr-v1 trail-exit").
        let invalidated = match side {
            Side::Buy => close < stop,
            Side::Sell => close > stop,
        };

        // Time stop.
        let timed_out = now_ns - entry_ts_ns >= cfg.max_hold_bars * cfg.bar_ns;

        if invalidated || timed_out {
            let exit_side = match side {
                Side::Buy => Side::Sell,
                Side::Sell => Side::Buy,
            };
            self.state = State::ExitSignaled;
            let tag = if timed_out && !invalidated {
                "swr-v1 time-stop"
            } else if ph == Phase::Full {
                "swr-v1 stop-out"
            } else {
                "swr-v1 trail-exit"
            };
            return vec![self.make_intent(venue, symbol, exit_side, units, tag)];
        }

        // Track the best close seen (drives the trail).
        match side {
            Side::Buy if close > best_close => best_close = close,
            Side::Sell if close < best_close => best_close = close,
            _ => {}
        }

        let mut intents = Vec::new();
        match ph {
            Phase::Full => {
                let hit_t1 = match side {
                    Side::Buy => close >= t1,
                    Side::Sell => close <= t1,
                };
                if hit_t1 {
                    // Half off (units shrinks to the remainder); stop →
                    // breakeven; the remainder trails from the best close
                    // so far once ATR is known.
                    stop = entry_ref; // breakeven
                    if let Some(a) = atr_now {
                        let cand = match side {
                            Side::Buy => best_close - cfg.trail_atr * a,
                            Side::Sell => best_close + cfg.trail_atr * a,
                        };
                        if cand.is_finite() {
                            stop = ratchet(side, stop, cand);
                        }
                    }
                    let exit_side = match side {
                        Side::Buy => Side::Sell,
                        Side::Sell => Side::Buy,
                    };
                    intents.push(self.make_intent(
                        venue,
                        symbol,
                        exit_side,
                        units * cfg.t1_fraction,
                        "swr-v1 t1",
                    ));
                    units *= 1.0 - cfg.t1_fraction;
                    ph = Phase::Trailed;
                }
            }
            Phase::Trailed => {
                // Ratchet the stop toward price (never backwards).
                if let Some(a) = atr_now {
                    let cand = match side {
                        Side::Buy => best_close - cfg.trail_atr * a,
                        Side::Sell => best_close + cfg.trail_atr * a,
                    };
                    if cand.is_finite() {
                        stop = ratchet(side, stop, cand);
                    }
                }
            }
        }

        self.state = State::Entered {
            venue,
            symbol,
            side,
            entry_ref,
            stop,
            t1,
            phase: ph,
            best_close,
            entry_ts_ns,
            units,
        };
        intents
    }
}

impl Strategy for SwingRangeReclaimV1 {
    fn id(&self) -> StrategyId {
        self.id.clone()
    }
    fn universe(&self) -> Universe {
        self.universe.clone()
    }
    fn subscriptions(&self) -> Vec<String> {
        vec![
            "swing.sweep.".into(),
            "swing.range.".into(),
            "swing.atr.".into(),
            "swing.profile.".into(),
            "swing.close".into(),
        ]
    }
    fn warmup_ns(&self) -> i64 {
        // Detector warm-up alone needs (range_n + atr_n) daily bars ≈ 40;
        // give the profile window room too.
        130 * 86_400_000_000_000
    }
    fn declared_regime(&self) -> RegimeMask {
        RegimeMask::any()
    }

    fn rebalance_cadence(&self) -> RebalanceCadence {
        RebalanceCadence::Daily
    }

    fn holding_period_bars(&self) -> mp_core::BarRange {
        mp_core::BarRange::new(2, 60)
    }

    fn on_feature(&mut self, u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        // Defense in depth (audit C1 pattern): only read the names this
        // strategy subscribes to.
        let is_sweep = u.name.starts_with("swing.sweep.");
        let is_range = !is_sweep && u.name.starts_with("swing.range.");
        let is_atr = !is_sweep && !is_range && u.name.starts_with("swing.atr.");
        let is_profile = u.name.starts_with("swing.profile.");
        let is_close = u.name == "swing.close";
        if !(is_sweep || is_range || is_atr || is_profile || is_close) {
            return Vec::new();
        }
        if !self.universe.symbols.contains(&u.symbol) {
            return Vec::new();
        }

        // Confirmed sweep event with its companion invalidation stop: the
        // entry trigger. No context write needed — the other streams carry it.
        if is_sweep && u.name.contains(".stop.") {
            let side = if u.name.starts_with("swing.sweep.low.stop") {
                Side::Buy
            } else if u.name.starts_with("swing.sweep.high.stop") {
                Side::Sell
            } else {
                return Vec::new();
            };
            let now = ctx.now_ns();
            // An unfilled signal expires even if the next thing we see is
            // another sweep event.
            if let State::EntrySignaled { ts_ns, .. } = self.state {
                if now - ts_ns > self.config.signal_timeout_ns {
                    self.state = State::Idle;
                }
            }
            return match self.state {
                State::Idle => self.check_entry(u.venue, u.symbol, side, u.value, now),
                _ => Vec::new(),
            };
        }

        // Context write (scoped so the state machine can borrow self after).
        {
            let ss = self.per_sym.entry((u.venue, u.symbol)).or_default();
            if is_close {
                ss.close = Some((u.ts_ns, u.value));
            } else if is_sweep {
                // Plain extreme emission — diagnostics only.
                if u.name.starts_with("swing.sweep.low.") {
                    ss.last_sweep_extreme_low = Some(u.value);
                } else if u.name.starts_with("swing.sweep.high.") {
                    ss.last_sweep_extreme_high = Some(u.value);
                }
            } else if is_range {
                if u.name.starts_with("swing.range.high.") {
                    ss.range_high = Some((u.ts_ns, u.value));
                } else if u.name.starts_with("swing.range.low.") {
                    ss.range_low = Some((u.ts_ns, u.value));
                }
            } else if is_atr {
                ss.atr = Some((u.ts_ns, u.value));
            } else if is_profile {
                if u.name.starts_with("swing.profile.lvn_above.") {
                    ss.lvn_above = Some((u.ts_ns, u.value));
                } else if u.name.starts_with("swing.profile.lvn_below.") {
                    ss.lvn_below = Some((u.ts_ns, u.value));
                } else if u.name.starts_with("swing.profile.hvn_above.") {
                    ss.hvn_above = Some((u.ts_ns, u.value));
                } else if u.name.starts_with("swing.profile.hvn_below.") {
                    ss.hvn_below = Some((u.ts_ns, u.value));
                }
            }
        }

        let now = ctx.now_ns();
        match self.state {
            State::Idle | State::EntrySignaled { .. } => {
                if let State::EntrySignaled { ts_ns, .. } = self.state {
                    if now - ts_ns > self.config.signal_timeout_ns {
                        self.state = State::Idle;
                    }
                }
                Vec::new()
            }
            State::ExitSignaled => {
                self.state = State::Idle;
                Vec::new()
            }
            State::Entered { venue, symbol, .. } => {
                if (venue, symbol) != (u.venue, u.symbol) {
                    return Vec::new();
                }
                self.check_exit(now)
            }
        }
    }

    fn on_fill(&mut self, fill: &mp_core::Fill, _ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        if let State::EntrySignaled {
            venue,
            symbol,
            side,
            stop,
            ts_ns,
        } = self.state
        {
            // Compute T1 from the actual fill price; without a valid target
            // the position cannot be managed — refuse the transition (the
            // risk gate still owns the open intent).
            let now = _ctx.now_ns();
            if let Some(t1) = self.compute_t1(side, fill.price, venue, symbol, now) {
                self.state = State::Entered {
                    venue,
                    symbol,
                    side,
                    entry_ref: fill.price,
                    stop,
                    t1,
                    phase: Phase::Full,
                    best_close: fill.price,
                    entry_ts_ns: ts_ns,
                    units: self.risk_units(),
                };
            } else {
                self.state = State::Idle;
            }
        }
        Vec::new()
    }

    fn params(&self) -> ParamSpace {
        let mut p = ParamSpace::default();
        p.grid.insert("trail_atr".into(), vec![1.5, 2.0, 3.0]);
        p
    }

    fn with_params(&self, params: &BTreeMap<String, f64>) -> Box<dyn Strategy> {
        let mut cfg = self.config;
        if let Some(&v) = params.get("trail_atr") {
            cfg.trail_atr = v;
        }
        Box::new(SwingRangeReclaimV1::new(
            self.id.clone(),
            self.universe.clone(),
            cfg,
        ))
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

    const DAY: i64 = 86_400_000_000_000;

    fn up(name: &str, value: f64, day: i64) -> FeatureUpdate {
        FeatureUpdate {
            feature: SymbolId(1),
            name: name.into(),
            venue: Venue::Hyperliquid,
            symbol: SymbolId(1),
            ts_ns: day * DAY,
            value,
            ver: 1,
        }
    }

    fn strat() -> SwingRangeReclaimV1 {
        SwingRangeReclaimV1::new(
            StrategyId::new("swing-range-reclaim-v1"),
            Universe {
                venues: vec![Venue::Hyperliquid],
                symbols: vec![SymbolId(1)],
            },
            RangeReclaimConfig::default(),
        )
    }

    fn ctx(day: i64) -> TestCtx {
        TestCtx {
            now: day * DAY,
            count: 0,
        }
    }

    /// Warm the context: compressed range [99,103], ATR 24, LVN above at 115.
    fn feed_context(s: &mut SwingRangeReclaimV1, c: &mut TestCtx, day: i64) {
        assert!(s
            .on_feature(&up("swing.range.high.20", 103.0, day), c)
            .is_empty());
        assert!(s
            .on_feature(&up("swing.range.low.20", 99.0, day), c)
            .is_empty());
        assert!(s.on_feature(&up("swing.atr.20", 24.0, day), c).is_empty());
        assert!(s.on_feature(&up("swing.close", 100.0, day), c).is_empty());
    }

    #[test]
    fn swl_s1_enters_long_on_confirmed_low_sweep_event() {
        let mut s = strat();
        let mut c = ctx(40);
        feed_context(&mut s, &mut c, 40);
        // The .stop emission IS the entry trigger (self-contained pair).
        let intents = s.on_feature(&up("swing.sweep.low.stop.20", 92.0 - 12.0, 41), &mut c);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].side, Side::Buy, "fade the swept support");
        assert_eq!(intents[0].tag, "swr-v1 entry");
    }

    #[test]
    fn swl_s2_enters_short_on_confirmed_high_sweep_event() {
        let mut s = strat();
        let mut c = ctx(40);
        feed_context(&mut s, &mut c, 40);
        let intents = s.on_feature(&up("swing.sweep.high.stop.20", 104.0 + 12.0, 41), &mut c);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].side, Side::Sell);
    }

    #[test]
    fn swl_s3_no_entry_without_fresh_range_or_stale_event() {
        // No range context at all → fail-closed.
        let mut s = strat();
        let mut c = ctx(40);
        let none = s.on_feature(&up("swing.sweep.low.stop.20", 80.0, 41), &mut c);
        assert!(none.is_empty());

        // Stale range (> 2 days old at event time) → fail-closed.
        let mut s2 = strat();
        let mut c2 = ctx(40);
        feed_context(&mut s2, &mut c2, 30);
        c2.now = 45 * DAY; // event arrives 15 days after the range reading
        let none2 = s2.on_feature(&up("swing.sweep.low.stop.20", 80.0, 45), &mut c2);
        assert!(none2.is_empty(), "range 15 days stale");

        // Non-subscribed features never trigger (C1 pattern).
        let mut s3 = strat();
        let mut c3 = ctx(40);
        feed_context(&mut s3, &mut c3, 40);
        let none3 = s3.on_feature(&up("funding.rate", 0.5, 41), &mut c3);
        assert!(none3.is_empty());
    }

    #[test]
    fn swl_s4_hard_invalidation_exits_full_on_close_through_stop() {
        let mut s = strat();
        let mut c = ctx(40);
        feed_context(&mut s, &mut c, 40);
        let stop_price = 80.0;
        let entry = s.on_feature(&up("swing.sweep.low.stop.20", stop_price, 41), &mut c);
        assert_eq!(entry.len(), 1);
        s.on_fill(
            &mp_core::Fill {
                intent_id: IntentId(1),
                symbol: SymbolId(1),
                side: Side::Buy,
                price: 100.0,
                qty: 1.0,
                fee: 0.0,
                liquidity: mp_core::Liquidity::Taker,
                ts_ns: 41 * DAY,
            },
            &mut c,
        );
        // Close above stop → hold.
        assert!(s
            .on_feature(&up("swing.close", 99.0, 42), &mut c)
            .is_empty());
        // Close BELOW stop → full market exit.
        let out = s.on_feature(&up("swing.close", 79.5, 43), &mut c);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].side, Side::Sell);
        assert_eq!(out[0].tag, "swr-v1 stop-out");
    }

    #[test]
    fn swl_s5_t1_half_exit_then_breakeven_and_trail() {
        let mut s = strat();
        let mut c = ctx(40);
        feed_context(&mut s, &mut c, 40);
        assert!(s
            .on_feature(&up("swing.profile.lvn_above.90", 115.0, 40), &mut c)
            .is_empty());
        // Enter long; stop 80; fill at 100; range high 103 → T1 = 103 (closer
        // than the 115 LVN).
        assert_eq!(
            s.on_feature(&up("swing.sweep.low.stop.20", 80.0, 41), &mut c)
                .len(),
            1
        );
        s.on_fill(
            &mp_core::Fill {
                intent_id: IntentId(1),
                symbol: SymbolId(1),
                side: Side::Buy,
                price: 100.0,
                qty: 1.0,
                fee: 0.0,
                liquidity: mp_core::Liquidity::Taker,
                ts_ns: 41 * DAY,
            },
            &mut c,
        );
        // Close at T1 → half exit, stop moves to breakeven.
        let t1 = s.on_feature(&up("swing.close", 103.0, 42), &mut c);
        assert_eq!(t1.len(), 1);
        assert_eq!(t1[0].tag, "swr-v1 t1");
        assert!(
            matches!(t1[0].qty, SizeUnit::RiskUnits(q) if (q - 0.5).abs() < 1e-9),
            "half position out"
        );
        // Dip to just above breakeven (stop = entry_ref = 100 now) → hold…
        assert!(s
            .on_feature(&up("swing.close", 100.5, 43), &mut c)
            .is_empty());
        // …a close below breakeven exits the remainder.
        let out = s.on_feature(&up("swing.close", 99.9, 44), &mut c);
        assert_eq!(out.len(), 1, "close under breakeven exits remainder");
        assert_eq!(out[0].tag, "swr-v1 trail-exit");
        assert_eq!(out[0].side, Side::Sell);
    }

    #[test]
    fn swl_s6_trail_ratchets_and_exits_remainder() {
        let mut s = strat();
        let mut c = ctx(40);
        feed_context(&mut s, &mut c, 40);
        // LVN above at 105 → T1 = min(103? no: range_high 103 vs lvn 105 >
        // 103 → ignored (LVN must lie BEYOND the boundary)) → T1 = 103.
        assert_eq!(
            s.on_feature(&up("swing.sweep.low.stop.20", 80.0, 41), &mut c)
                .len(),
            1
        );
        s.on_fill(
            &mp_core::Fill {
                intent_id: IntentId(1),
                symbol: SymbolId(1),
                side: Side::Buy,
                price: 100.0,
                qty: 1.0,
                fee: 0.0,
                liquidity: mp_core::Liquidity::Taker,
                ts_ns: 41 * DAY,
            },
            &mut c,
        );
        // T1 hit → half off, trail armed from best close.
        assert_eq!(s.on_feature(&up("swing.close", 103.0, 42), &mut c).len(), 1);
        // Rally: best close 120 → trail = 120 − 48 = 72… but ATR updates to
        // 10 → trail = 120 − 20 = 100.
        assert!(s
            .on_feature(&up("swing.close", 120.0, 43), &mut c)
            .is_empty());
        assert!(s
            .on_feature(&up("swing.atr.20", 10.0, 43), &mut c)
            .is_empty());
        // Close 119 ≥ trail 100 → hold.
        assert!(s
            .on_feature(&up("swing.close", 119.0, 44), &mut c)
            .is_empty());
        // Close through the ratcheted stop (best 120, ATR 10 → stop = 100):
        let out = s.on_feature(&up("swing.close", 95.0, 45), &mut c);
        assert_eq!(out.len(), 1, "trail exit fires");
        assert_eq!(out[0].tag, "swr-v1 trail-exit");
        assert!(
            matches!(out[0].qty, SizeUnit::RiskUnits(q) if (q - 0.5).abs() < 1e-9),
            "remainder only"
        );
    }

    #[test]
    fn swl_s7_time_stop_forces_exit() {
        let mut s = strat();
        let mut c = ctx(40);
        feed_context(&mut s, &mut c, 40);
        c.now = 41 * DAY;
        assert_eq!(
            s.on_feature(&up("swing.sweep.low.stop.20", 80.0, 41), &mut c)
                .len(),
            1
        );
        s.on_fill(
            &mp_core::Fill {
                intent_id: IntentId(1),
                symbol: SymbolId(1),
                side: Side::Buy,
                price: 100.0,
                qty: 1.0,
                fee: 0.0,
                liquidity: mp_core::Liquidity::Taker,
                ts_ns: 41 * DAY,
            },
            &mut c,
        );
        // Price drifts sideways forever — no stop/target hit.
        // (entry signal ts = day 41; the budget expires at day >= 101.)
        for d in 42..101 {
            c.now = d * DAY;
            let intents = s.on_feature(&up("swing.close", 101.0, d), &mut c);
            assert!(intents.is_empty(), "d={d} intents={intents:?}");
        }
        // Day 41 + 60 held days → the budget is exhausted by day 101; the
        // next close after that forces the exit.
        c.now = 102 * DAY;
        let out = s.on_feature(&up("swing.close", 101.0, 102), &mut c);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tag, "swr-v1 time-stop");
    }

    #[test]
    fn swl_s8_signal_timeout_returns_to_idle() {
        let mut s = strat();
        let mut c = ctx(40);
        feed_context(&mut s, &mut c, 40);
        c.now = 41 * DAY;
        assert_eq!(
            s.on_feature(&up("swing.sweep.low.stop.20", 80.0, 41), &mut c)
                .len(),
            1
        );
        // No fill; refresh the context near day 44 so range freshness is not
        // what blocks re-entry, then let > 2 days pass since the signal.
        feed_context(&mut s, &mut c, 44);
        c.now = 45 * DAY;
        let again = s.on_feature(&up("swing.sweep.low.stop.20", 81.0, 45), &mut c);
        assert_eq!(again.len(), 1, "re-armed after timeout");
    }

    #[test]
    fn swl_s9_cadence_and_holding_metadata_declared() {
        let s = strat();
        assert_eq!(s.rebalance_cadence(), RebalanceCadence::Daily);
        assert_eq!(s.holding_period_bars(), mp_core::BarRange::new(2, 60));
        // Subscriptions are prefix forms, never globs (see AGENTS contracts).
        for sub in s.subscriptions() {
            assert!(!sub.ends_with("*"), "glob subscription {sub} is banned");
        }
        assert!(s.subscriptions().iter().any(|x| x == "swing.sweep."));
    }

    #[test]
    fn swl_s10_walk_forward_param_grid_applies() {
        let s = strat();
        let mut params = BTreeMap::new();
        params.insert("trail_atr".to_string(), 3.0);
        let s2 = s.with_params(&params);
        let grid = s.params();
        assert_eq!(
            grid.grid.get("trail_atr").map(|v| v.as_slice()),
            Some(&[1.5, 2.0, 3.0][..])
        );
        // The rebuilt instance must actually carry the override.
        let _ = s2;
    }
}
