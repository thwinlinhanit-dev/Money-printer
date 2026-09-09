//! Fixture strategies (STR-9). `NullStrategy` trades nothing; `CoinFlipStrategy`
//! trades on a seeded coin flip — deliberately edge-free so the funnel docs can
//! use its (failing) backtest as the worked example of an honest G1 kill.

use crate::strategy::{Ctx, RegimeMask, Strategy, Universe};
use mp_core::{IntentId, OrderIntent, OrderKind, Side, SizeUnit, StrategyId, TimeInForce};
use mp_features::FeatureUpdate;

/// Emits no orders, ever.
#[derive(Debug, Default)]
pub struct NullStrategy;

impl Strategy for NullStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::new("null")
    }
    fn universe(&self) -> Universe {
        Universe::default()
    }
    fn subscriptions(&self) -> Vec<String> {
        Vec::new()
    }
    fn warmup_ns(&self) -> i64 {
        0
    }
    fn declared_regime(&self) -> RegimeMask {
        RegimeMask::any()
    }
    fn on_feature(&mut self, _u: &FeatureUpdate, _ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        Vec::new()
    }
    fn with_params(&self, _params: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
        Box::new(NullStrategy)
    }
}

/// Trades a market order on a seeded coin flip. No edge — its purpose is to be
/// killed by the machine (spec 006 §CoinFlip).
#[derive(Debug, Default)]
pub struct CoinFlipStrategy {
    next_intent: u128,
}

impl CoinFlipStrategy {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Strategy for CoinFlipStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::new("coinflip")
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
    fn on_feature(&mut self, u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        // Deterministic given the seeded ctx rng (CONV-11).
        let flip = ctx.next_u64();
        let side = if flip & 1 == 0 { Side::Buy } else { Side::Sell };
        self.next_intent += 1;
        vec![OrderIntent {
            intent_id: IntentId(self.next_intent),
            strategy: self.id(),
            // Use the venue stamped on the update by FeatureEngine (Major #4 fix:
            // venue comes from EventEnvelope.venue, not a hardcoded default).
            venue: u.venue,
            symbol: u.symbol,
            side,
            kind: OrderKind::Market,
            qty: SizeUnit::RiskUnits(1.0),
            tif: TimeInForce::Ioc,
            reduce_only: false,
            tag: "coinflip".into(),
        }]
    }
    fn with_params(&self, _params: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
        Box::new(CoinFlipStrategy::new())
    }
}

/// Venue-generic noise control (spec 054 REL-32): the same edge-free seeded
/// flip as [`CoinFlipStrategy`], but the subscription is the `cvd.` PREFIX —
/// the sim engine's subscription matcher (`name.starts_with(sub)`) dispatches
/// every venue's CVD (`cvd.bybit`, `cvd.hyperliquid`, …) to it, so it fires
/// on ANY venue's tape where the legacy fixed-`cvd.bybit` control starves.
/// Fires on a seeded-rate SAMPLE of qualifying updates (`fire_rate_per_update`,
/// default 1/100) so a multi-day tape yields a usable sample instead of one
/// order per tick; direction comes from the same seeded draw (CONV-11).
/// Purpose: the R-8 control — a strategy that MUST be rejected at every
/// horizon; a pipeline that promotes it is broken.
#[derive(Debug, Clone)]
pub struct CoinFlipAnyStrategy {
    /// Probability that a dispatched `cvd.*` update produces an order.
    fire_rate_per_update: f64,
    next_intent: u128,
}

impl CoinFlipAnyStrategy {
    pub fn new() -> Self {
        Self {
            fire_rate_per_update: 0.01,
            next_intent: 0,
        }
    }
}

impl Default for CoinFlipAnyStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for CoinFlipAnyStrategy {
    fn id(&self) -> StrategyId {
        StrategyId::new("coinflip-any")
    }
    fn universe(&self) -> Universe {
        Universe::default()
    }
    fn subscriptions(&self) -> Vec<String> {
        // PREFIX match (sim `subscribed`): every venue's CVD.
        vec!["cvd.".to_string()]
    }
    fn warmup_ns(&self) -> i64 {
        0
    }
    fn declared_regime(&self) -> RegimeMask {
        RegimeMask::any()
    }
    fn on_feature(&mut self, u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        // One seeded draw per qualifying update: gate AND side (CONV-11 —
        // deterministic given the seeded ctx rng).
        let draw = ctx.next_u64();
        // High 53 bits as a fraction of [0,1) — f64-exact.
        let unit = (draw >> 11) as f64 / (1u64 << 53) as f64;
        if unit >= self.fire_rate_per_update {
            return Vec::new();
        }
        let side = if draw & 1 == 0 { Side::Buy } else { Side::Sell };
        self.next_intent += 1;
        vec![OrderIntent {
            intent_id: IntentId(self.next_intent),
            strategy: self.id(),
            venue: u.venue,
            symbol: u.symbol,
            side,
            kind: OrderKind::Market,
            qty: SizeUnit::RiskUnits(1.0),
            tif: TimeInForce::Ioc,
            reduce_only: false,
            tag: "coinflip-any".into(),
        }]
    }
    fn with_params(&self, _params: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
        Box::new(CoinFlipAnyStrategy::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::Venue;
    use std::collections::BTreeMap;

    /// Minimal seeded Ctx (LCG — deterministic, CONV-11 style).
    struct SeedCtx(u64);
    impl Ctx for SeedCtx {
        fn now_ns(&self) -> i64 {
            0
        }
        fn position(&self, _symbol: mp_core::SymbolId) -> f64 {
            0.0
        }
        fn equity_allocated(&self) -> f64 {
            100_000.0
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0
        }
        fn set_timer(&mut self, _after_ns: i64) -> crate::strategy::TimerId {
            crate::strategy::TimerId(1)
        }
        fn log(&mut self, _msg: &str) {}
    }

    fn upd(name: &str, venue: Venue) -> FeatureUpdate {
        FeatureUpdate {
            symbol: mp_core::SymbolId(1),
            feature: mp_core::SymbolId(9),
            name: name.into(),
            venue,
            value: 1.0,
            ts_ns: 0,
            ver: 1,
        }
    }

    // REL-32: the control fires on ANY venue's cvd.* (the prefix seam), and
    // the same seed produces the same orders (CONV-11).
    #[test]
    fn rel_32_fires_on_any_venue_and_is_seed_deterministic() {
        assert_eq!(CoinFlipAnyStrategy::new().subscriptions(), vec!["cvd.".to_string()]);
        let run = |seed: u64| {
            let mut s = CoinFlipAnyStrategy::new();
            let mut ctx = SeedCtx(seed);
            let mut fired = Vec::new();
            for i in 0..2000 {
                let v = if i % 2 == 0 { Venue::Hyperliquid } else { Venue::Bybit };
                fired.extend(s.on_feature(&upd("cvd.x", v), &mut ctx));
            }
            fired
        };
        let a = run(7);
        let b = run(7);
        assert_eq!(a.len(), b.len(), "same seed ⇒ identical fire count");
        assert_eq!(
            a.iter().map(|o| o.intent_id.0).collect::<Vec<_>>(),
            b.iter().map(|o| o.intent_id.0).collect::<Vec<_>>()
        );
        assert!(!a.is_empty(), "control must fire on the tape");
        assert!(
            a.iter().any(|o| o.venue == Venue::Hyperliquid) && a.iter().any(|o| o.venue == Venue::Bybit),
            "venue-generic: fires on BOTH venues"
        );
        // Legacy fixed-venue control would starve here — sanity only.
        assert!(CoinFlipStrategy::new().subscriptions() == vec!["cvd.bybit".to_string()]);
    }

    // REL-32: the sampled rate bounds the fire count (default 1/100 of
    // qualifying updates, ±sampling noise) — no per-tick order flood.
    #[test]
    fn rel_32_sampled_rate_bounds_fires() {
        let mut s = CoinFlipAnyStrategy::new();
        let mut ctx = SeedCtx(42);
        let mut fired = 0u64;
        for i in 0..10_000 {
            if !s.on_feature(&upd("cvd.hyperliquid", Venue::Hyperliquid), &mut ctx).is_empty() {
                fired += 1;
            }
        }
        assert!(
            fired > 50 && fired < 200,
            "rate 1/100 over 10k updates must sample ~100 fires (got {fired})"
        );
        // Non-cvd updates are never even gated (subscription-scoped), but the
        // strategy still ignores them defensively.
        let mut s2 = CoinFlipAnyStrategy::new();
        let mut ctx2 = SeedCtx(1);
        assert!(s2.on_feature(&upd("funding.rate", Venue::Bybit), &mut ctx2).is_empty());
        let _ = BTreeMap::<String, f64>::new();
    }
}
