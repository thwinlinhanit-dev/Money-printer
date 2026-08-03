//! Shared execution vocabulary (spec 006/007): the `OrderIntent` a strategy
//! emits and the `Fill` it hears back. Lives in `core` because strategies, sim,
//! risk, and oms all speak it — but strategies must NOT depend on oms (PD-4), so
//! it cannot live there.

use crate::event::{Side, SymbolId, Venue};
use serde::{Deserialize, Serialize};

/// Stable strategy identifier (a slug like `"carry-v1"`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StrategyId(pub String);

impl StrategyId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// Unique intent id. In sim it is assigned sequentially for determinism; in
/// live it is a ULID (CONV-19). Kept opaque here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IntentId(pub u128);

/// How size is expressed. Strategies default to `RiskUnits`; the sizing engine
/// (spec 008) converts to contracts. Raw contracts are for reduce-only exits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SizeUnit {
    RiskUnits(f64),
    Contracts(f64),
}

/// Order kind.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OrderKind {
    Market,
    Limit { px: f64 },
    Cancel { target: IntentId },
}

/// Time in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    Ioc,
    Gtc,
    PostOnly,
}

/// What a strategy emits. It never reaches a venue directly — the sizing engine
/// and risk gate stand between (PD-4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderIntent {
    pub intent_id: IntentId,
    pub strategy: StrategyId,
    pub venue: Venue,
    pub symbol: SymbolId,
    pub side: Side,
    pub kind: OrderKind,
    pub qty: SizeUnit,
    pub tif: TimeInForce,
    pub reduce_only: bool,
    pub tag: String,
}

/// Intent-shape failure reasons (CONV-8: decision-path inputs must be finite).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntentError {
    #[error("quantity/risk-units is not finite")]
    NonFiniteQty,
    #[error("quantity/risk-units is negative")]
    NegativeQty,
    #[error("limit price is not strictly positive and finite")]
    BadLimitPrice,
    #[error("strategy id is empty")]
    EmptyStrategy,
}

impl OrderIntent {
    /// Structural validation at the strategy→sizing seam (audit C2). Runs before
    /// the intent can reach the gate: a buggy strategy emitting NaN qty, a
    /// negative size, or junk gets an error here, never a fill.
    pub fn validate(&self) -> Result<(), IntentError> {
        match &self.qty {
            SizeUnit::Contracts(c) => {
                if !c.is_finite() {
                    return Err(IntentError::NonFiniteQty);
                }
                if *c < 0.0 {
                    return Err(IntentError::NegativeQty);
                }
            }
            SizeUnit::RiskUnits(r) => {
                if !r.is_finite() {
                    return Err(IntentError::NonFiniteQty);
                }
                if *r < 0.0 {
                    return Err(IntentError::NegativeQty);
                }
            }
        }
        if let OrderKind::Limit { px } = &self.kind {
            if !px.is_finite() || *px <= 0.0 {
                return Err(IntentError::BadLimitPrice);
            }
        }
        if self.strategy.0.is_empty() {
            return Err(IntentError::EmptyStrategy);
        }
        Ok(())
    }
}

/// Whether a fill added or removed liquidity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Liquidity {
    Maker,
    Taker,
}

/// A (partial) fill of an intent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub intent_id: IntentId,
    pub symbol: SymbolId,
    pub side: Side,
    pub price: f64,
    pub qty: f64,
    pub fee: f64,
    pub liquidity: Liquidity,
    pub ts_ns: i64,
}
