//! Liquidation-fade strategy (spec 004/006, `strategies/liq-fade-v1/
//! hypothesis.md`). Fades a liquidation cascade AFTER exhaustion: when the
//! rolling sell-side (or buy-side) liquidation notional (`liq.vol_*`) has
//! spiked, the liquidation prints are STRETCHED from mid (`liq.dist`), and
//! the rolling sum is DRAINING (flow stopped — exhaustion), we take the side
//! of mean reversion. Exits on reversion (`liq.dist` collapse), on
//! re-acceleration (a new `liq.vol_*` peak — the fade was wrong), or the
//! time stop. v1 is single-venue, market-intent only, no scaling.
//!
//! HONEST DATA GATE (hypothesis.md): the `liq.*` features only fire on a
//! recording with a liquidation stream (bybit, COL-29 spec 024). The current
//! hyperliquid corpus has none, so a real-corpus backtest yields zero trades
//! by construction — that verdict is recorded as "no data," never faked.

use crate::strategy::{Ctx, ParamSpace, RegimeMask, Strategy, Universe};
use mp_core::{IntentId, OrderIntent, OrderKind, Side, SizeUnit, StrategyId, SymbolId, Venue};
use mp_features::FeatureUpdate;
use std::collections::BTreeMap;

/// Per-unit risk the gate's sizer assumes — same convention as carry-v1 and
/// orderflow-v1 (RSK-1: strategies emit risk units; the gate owns contracts).
const PER_RISK_UNIT_PCT: f64 = 0.005;

/// Configuration for the liq-fade-v1 strategy.
#[derive(Debug, Clone, Copy)]
pub struct LiqFadeConfig {
    /// Rolling notional (USD) a cascade must reach before fading is on the
    /// table (default $1M of one-sided liquidation flow in the window).
    pub entry_vol: f64,
    /// `liq.dist` (bps) the liquidation prints must be stretched from mid —
    /// the knife must have already fallen (default 30 bps).
    pub entry_dist_bps: f64,
    /// Current rolling notional must have drained to at most this fraction of
    /// the recent peak before we fade (default 0.8 — flow has stopped, not
    /// paused). 1.0 would fade mid-cascade (catches knives).
    pub exhaust_frac: f64,
    /// `liq.dist` (bps) below which the reversion is considered complete.
    pub exit_dist_bps: f64,
    /// A `liq.vol_*` reading older than this (ns) is stale for entry purposes.
    pub vol_stale_ns: i64,
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

impl Default for LiqFadeConfig {
    fn default() -> Self {
        Self {
            entry_vol: 1_000_000.0,
            entry_dist_bps: 30.0,
            exhaust_frac: 0.8,
            exit_dist_bps: 5.0,
            vol_stale_ns: 30 * 1_000_000_000,
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
enum LiqFadeState {
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
    /// (ts, rolling notional) of the cascade side we track (vol_sell for a
    /// long fade, vol_buy for a short fade). Both are stored; the entry check
    /// picks the side by which cascade is active.
    vol_buy: Option<(i64, f64)>,
    vol_sell: Option<(i64, f64)>,
    dist: Option<(i64, f64)>,
    /// Rolling peak of the currently-active cascade side (exhaustion anchor).
    peak_vol_sell: Option<(i64, f64)>,
    peak_vol_buy: Option<(i64, f64)>,
}

/// The liquidation-fade strategy.
pub struct LiqFadeV1 {
    id: StrategyId,
    config: LiqFadeConfig,
    universe: Universe,
    per_sym: BTreeMap<(Venue, SymbolId), SymState>,
    state: LiqFadeState,
    next_intent: u128,
}

impl LiqFadeV1 {
    pub fn new(id: StrategyId, universe: Universe, config: LiqFadeConfig) -> Self {
        Self {
            id,
            config,
            universe,
            per_sym: BTreeMap::new(),
            state: LiqFadeState::Idle,
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

    /// Idle → entry. Fade the ACTIVE cascade (whichever side is big) after
    /// exhaustion + stretch. Sell cascade → long fade; buy cascade → short.
    fn check_entry(&mut self, venue: Venue, symbol: SymbolId, now_ns: i64) -> Vec<OrderIntent> {
        let Some(ss) = self.per_sym.get(&(venue, symbol)) else {
            return Vec::new();
        };
        // Sell cascade (longs dumped) → we buy the reversion.
        if let Some(candidate) = self.fade_candidate(
            ss.vol_sell,
            ss.peak_vol_sell,
            ss.dist,
            Side::Sell,
            now_ns,
        ) {
            self.state = LiqFadeState::EntrySignaled {
                venue,
                symbol,
                direction: candidate,
                entry_ts_ns: now_ns,
            };
            return vec![self.make_intent(venue, symbol, candidate, "liq-fade-v1 entry")];
        }
        // Buy cascade (shorts squeezed) → we sell the reversion.
        if let Some(candidate) = self.fade_candidate(
            ss.vol_buy,
            ss.peak_vol_buy,
            ss.dist,
            Side::Buy,
            now_ns,
        ) {
            self.state = LiqFadeState::EntrySignaled {
                venue,
                symbol,
                direction: candidate,
                entry_ts_ns: now_ns,
            };
            return vec![self.make_intent(venue, symbol, candidate, "liq-fade-v1 entry")];
        }
        Vec::new()
    }

    /// The three-entry-condition check for ONE cascade side. Returns the fade
    /// direction (opposite the cascade) when: the cascade notional is big
    /// enough, fresh enough, the prints are stretched from mid, and the
    /// rolling sum has drained to ≤ exhaust_frac of its peak (flow stopped).
    fn fade_candidate(
        &self,
        vol: Option<(i64, f64)>,
        peak: Option<(i64, f64)>,
        dist: Option<(i64, f64)>,
        cascade_side: Side,
        now_ns: i64,
    ) -> Option<Side> {
        let cfg = self.config;
        let (vts, v) = vol?;
        let (_, p) = peak?;
        let (dts, d) = dist?;
        if now_ns - vts > cfg.vol_stale_ns || now_ns - dts > cfg.vol_stale_ns {
            return None; // stale readings are not evidence
        }
        if v < cfg.entry_vol || d < cfg.entry_dist_bps {
            return None; // not a cascade, or not stretched yet
        }
        if v > p * cfg.exhaust_frac {
            return None; // still near peak: mid-cascade, catching a knife
        }
        Some(match cascade_side {
            Side::Sell => Side::Buy,
            Side::Buy => Side::Sell,
        })
    }

    /// Entered → exit: reversion complete (dist collapsed), cascade
    /// re-accelerated (new peak — the fade was wrong), or time stop.
    fn check_exit(&mut self, venue: Venue, symbol: SymbolId, now_ns: i64) -> Vec<OrderIntent> {
        let cfg = self.config;
        let Some(ss) = self.per_sym.get(&(venue, symbol)) else {
            return Vec::new();
        };
        let LiqFadeState::Entered {
            direction,
            entry_ts_ns,
            ..
        } = self.state
        else {
            return Vec::new();
        };
        // The cascade side we faded is the one opposite our position.
        let cascade_side = match direction {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let (vol, peak) = match cascade_side {
            Side::Sell => (ss.vol_sell, ss.peak_vol_sell),
            Side::Buy => (ss.vol_buy, ss.peak_vol_buy),
        };
        let reverted = ss.dist.is_some_and(|(_, d)| d < cfg.exit_dist_bps);
        let reaccelerated = match (vol, peak) {
            (Some((_, v)), Some((_, p))) => v > p, // new peak = wrong fade
            _ => false,
        };
        let time = now_ns - entry_ts_ns > cfg.max_hold_ns;
        if reverted || reaccelerated || time {
            let exit_side = match direction {
                Side::Buy => Side::Sell,
                Side::Sell => Side::Buy,
            };
            self.state = LiqFadeState::ExitSignaled;
            return vec![self.make_intent(venue, symbol, exit_side, "liq-fade-v1 exit")];
        }
        Vec::new()
    }
}

impl Strategy for LiqFadeV1 {
    fn id(&self) -> StrategyId {
        self.id.clone()
    }
    fn universe(&self) -> Universe {
        self.universe.clone()
    }
    fn subscriptions(&self) -> Vec<String> {
        vec![
            "liq.vol_buy".into(),
            "liq.vol_sell".into(),
            "liq.dist".into(),
        ]
    }
    fn warmup_ns(&self) -> i64 {
        60_000_000_000
    }
    fn declared_regime(&self) -> RegimeMask {
        RegimeMask::any()
    }

    fn on_feature(&mut self, u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        // Defense in depth (audit C1 pattern): never read a non-liq feature's
        // value as a cascade/dist reading, even if a dispatcher misroutes it.
        if !(u.name.starts_with("liq.vol_buy")
            || u.name.starts_with("liq.vol_sell")
            || u.name == "liq.dist")
        {
            return Vec::new();
        }
        if u.venue != self.universe.venues.first().copied().unwrap_or(Venue::Hyperliquid)
            || !self.universe.symbols.contains(&u.symbol)
        {
            return Vec::new();
        }
        // Scoped write of the reading + capture of the PRE-update peaks: the
        // state machine must decide against the historical high, not one that
        // already absorbed this reading (otherwise a new high is its own peak
        // and "re-acceleration" is undetectable — lf_5 regression). Peaks
        // advance AFTER the decision, in a fresh borrow below.
        let (peak_sell_before, peak_buy_before, is_sell, is_buy) = {
            let ss = self.per_sym.entry((u.venue, u.symbol)).or_default();
            let peaks = (
                ss.peak_vol_sell.map(|(_, p)| p).unwrap_or(0.0),
                ss.peak_vol_buy.map(|(_, p)| p).unwrap_or(0.0),
            );
            match u.name.as_str() {
                "liq.vol_sell" => ss.vol_sell = Some((u.ts_ns, u.value)),
                "liq.vol_buy" => ss.vol_buy = Some((u.ts_ns, u.value)),
                "liq.dist" => ss.dist = Some((u.ts_ns, u.value)),
                _ => return Vec::new(),
            }
            (peaks.0, peaks.1, u.name == "liq.vol_sell", u.name == "liq.vol_buy")
        };
        let now = ctx.now_ns();
        let intents = match self.state {
            LiqFadeState::Idle => self.check_entry(u.venue, u.symbol, now),
            LiqFadeState::EntrySignaled { entry_ts_ns, .. } => {
                if now - entry_ts_ns > self.config.signal_timeout_ns {
                    self.state = LiqFadeState::Idle;
                }
                Vec::new()
            }
            LiqFadeState::Entered { .. } => self.check_exit(u.venue, u.symbol, now),
            LiqFadeState::ExitSignaled => {
                self.state = LiqFadeState::Idle;
                Vec::new()
            }
        };
        // Fresh borrow: advance the peaks with the reading just absorbed.
        if is_sell && u.value > peak_sell_before {
            self.per_sym
                .entry((u.venue, u.symbol))
                .or_default()
                .peak_vol_sell = Some((u.ts_ns, u.value));
        }
        if is_buy && u.value > peak_buy_before {
            self.per_sym
                .entry((u.venue, u.symbol))
                .or_default()
                .peak_vol_buy = Some((u.ts_ns, u.value));
        }
        intents
    }

    fn on_fill(&mut self, fill: &mp_core::Fill, _ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        if let LiqFadeState::EntrySignaled {
            venue,
            symbol,
            direction,
            entry_ts_ns,
        } = self.state
        {
            self.state = LiqFadeState::Entered {
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
        p.grid.insert("entry_vol".into(), vec![500_000.0, 1_000_000.0, 2_000_000.0]);
        p.grid.insert("entry_dist_bps".into(), vec![15.0, 30.0, 60.0]);
        p.grid.insert("exhaust_frac".into(), vec![0.6, 0.8, 0.9]);
        p
    }

    fn with_params(&self, params: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
        let mut cfg = self.config;
        if let Some(&v) = params.get("entry_vol") {
            cfg.entry_vol = v;
        }
        if let Some(&v) = params.get("entry_dist_bps") {
            cfg.entry_dist_bps = v;
        }
        if let Some(&v) = params.get("exhaust_frac") {
            cfg.exhaust_frac = v;
        }
        Box::new(LiqFadeV1::new(self.id.clone(), self.universe.clone(), cfg))
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

    fn strat() -> LiqFadeV1 {
        LiqFadeV1::new(
            StrategyId::new("liq-fade-v1"),
            Universe {
                venues: vec![Venue::Hyperliquid],
                symbols: vec![SymbolId(1)],
            },
            LiqFadeConfig::default(),
        )
    }

    fn ctx(now: i64) -> TestCtx {
        TestCtx { now, count: 0 }
    }

    /// Drive a sell cascade to a peak, then let the rolling sum drain, then
    /// assert the fade fires long.
    fn sell_cascade_to_exhaustion(s: &mut LiqFadeV1, c: &mut TestCtx) -> Vec<OrderIntent> {
        // Cascade builds to $1.5M (peak), then drains to $1.1M (<= 0.8 x peak).
        assert!(s.on_feature(&up("liq.vol_sell", 1_500_000.0, 100), c).is_empty());
        assert!(s.on_feature(&up("liq.dist", 40.0, 110), c).is_empty());
        // Exhaustion: drained below 0.8 x 1.5M = 1.2M.
        s.on_feature(&up("liq.vol_sell", 1_100_000.0, 200), c)
    }

    #[test]
    fn lf_1_enters_long_after_sell_cascade_exhaustion_and_stretch() {
        let mut s = strat();
        let mut c = ctx(1_000);
        let intents = sell_cascade_to_exhaustion(&mut s, &mut c);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].side, Side::Buy, "fade the sell cascade");
        assert_eq!(intents[0].tag, "liq-fade-v1 entry");
    }

    #[test]
    fn lf_2_enters_short_after_buy_cascade_exhaustion() {
        let mut s = strat();
        let mut c = ctx(1_000);
        assert!(s.on_feature(&up("liq.vol_buy", 1_500_000.0, 100), &mut c).is_empty());
        assert!(s.on_feature(&up("liq.dist", 40.0, 110), &mut c).is_empty());
        let intents = s.on_feature(&up("liq.vol_buy", 1_100_000.0, 200), &mut c);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].side, Side::Sell, "fade the buy cascade");
    }

    #[test]
    fn lf_3_no_entry_below_vol_floor_or_stretch_floor() {
        let mut s = strat();
        let mut c = ctx(1_000);
        // Big cascade but NOT stretched from mid (dist 10 < 30) → no fade.
        assert!(s.on_feature(&up("liq.vol_sell", 1_500_000.0, 100), &mut c).is_empty());
        assert!(s.on_feature(&up("liq.dist", 10.0, 110), &mut c).is_empty());
        assert!(s.on_feature(&up("liq.vol_sell", 1_100_000.0, 200), &mut c).is_empty());
        // Small cascade but stretched → still no fade (vol below floor).
        let mut s2 = strat();
        let mut c2 = ctx(1_000);
        assert!(s2.on_feature(&up("liq.vol_sell", 900_000.0, 100), &mut c2).is_empty());
        assert!(s2.on_feature(&up("liq.dist", 40.0, 110), &mut c2).is_empty());
        assert!(s2.on_feature(&up("liq.vol_sell", 700_000.0, 200), &mut c2).is_empty());
    }

    #[test]
    fn lf_4_no_entry_mid_cascade_or_stale() {
        // Mid-cascade: rolling sum still near the peak (1.4M > 0.8 x 1.5M) —
        // the fade would catch a knife.
        let mut s = strat();
        let mut c = ctx(1_000);
        assert!(s.on_feature(&up("liq.vol_sell", 1_500_000.0, 100), &mut c).is_empty());
        assert!(s.on_feature(&up("liq.dist", 40.0, 110), &mut c).is_empty());
        assert!(s.on_feature(&up("liq.vol_sell", 1_400_000.0, 200), &mut c).is_empty());
        // Stale: the exhausted reading is 31s old (> vol_stale_ns 30s).
        let mut s2 = strat();
        let mut c2 = ctx(40_000_000_000);
        assert!(s2.on_feature(&up("liq.vol_sell", 1_500_000.0, 100), &mut c2).is_empty());
        assert!(s2.on_feature(&up("liq.dist", 40.0, 110), &mut c2).is_empty());
        assert!(s2.on_feature(&up("liq.vol_sell", 1_100_000.0, 31_000_000_000), &mut c2).is_empty());
    }

    #[test]
    fn lf_5_exits_on_reversion_or_reacceleration() {
        // Enter long via the sell cascade.
        let mut s = strat();
        let mut c = ctx(1_000);
        let intents = sell_cascade_to_exhaustion(&mut s, &mut c);
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
                ts_ns: 1_000,
            },
            &mut c,
        );
        // Reversion complete: dist collapses below exit_dist_bps (5).
        let exit = s.on_feature(&up("liq.dist", 2.0, 2_000), &mut c);
        assert_eq!(exit.len(), 1);
        assert_eq!(exit[0].tag, "liq-fade-v1 exit");
        assert_eq!(exit[0].side, Side::Sell);

        // Re-acceleration: new peak above the entry peak (1.5M) — fade was wrong.
        let mut s2 = strat();
        let mut c2 = ctx(1_000);
        let intents2 = sell_cascade_to_exhaustion(&mut s2, &mut c2);
        assert_eq!(intents2.len(), 1);
        s2.on_fill(
            &mp_core::Fill {
                intent_id: IntentId(1),
                symbol: SymbolId(1),
                side: Side::Buy,
                price: 100.0,
                qty: 1.0,
                fee: 0.0,
                liquidity: mp_core::Liquidity::Taker,
                ts_ns: 1_000,
            },
            &mut c2,
        );
        let cut = s2.on_feature(&up("liq.vol_sell", 1_600_000.0, 2_000), &mut c2);
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].side, Side::Sell);
    }

    #[test]
    fn lf_6_ignores_non_liq_features() {
        // Audit C1 pattern: an extreme value on an unrelated feature must
        // never be read as cascade/dist and must not enter.
        let mut s = strat();
        let mut c = ctx(1_000);
        let none = s.on_feature(&up("funding.rate", 0.5, 1_000), &mut c);
        assert!(none.is_empty());
        assert_eq!(s.state, LiqFadeState::Idle);
        let none2 = s.on_feature(&up("book.depth.0.5", 0.9, 1_000), &mut c);
        assert!(none2.is_empty());
        assert_eq!(s.state, LiqFadeState::Idle);
    }
}
