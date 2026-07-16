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
}
