//! The Strategy contract (STR-1). Deliberately small. Strategies are pure
//! functions of (features, fills, timers, params, ctx) → intents. `Ctx` exposes
//! NO I/O, NO wall clock, NO venue handles — by construction, so PD-3/PD-4 hold
//! at compile time (this crate cannot even name a venue adapter).

use mp_core::{OrderIntent, SymbolId, Venue};
use mp_features::FeatureUpdate;
use std::collections::BTreeMap;

/// Opaque timer handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerId(pub u64);

/// The only capabilities a strategy has. No I/O, no wall clock (PD-3).
pub trait Ctx {
    /// Injected time (event time in sim, OS time in live — never read directly).
    fn now_ns(&self) -> i64;
    /// Current net position in contracts for a symbol (owned by this strategy).
    fn position(&self, symbol: SymbolId) -> f64;
    /// Equity currently allocated to this strategy.
    fn equity_allocated(&self) -> f64;
    /// Next value from the seeded PRNG (CONV-11) — the only randomness allowed.
    fn next_u64(&mut self) -> u64;
    /// Request a timer callback after `after_ns`.
    fn set_timer(&mut self, after_ns: i64) -> TimerId;
    /// Structured log line (no direct I/O from the strategy).
    fn log(&mut self, msg: &str);
}

/// Symbols/venues a strategy trades.
#[derive(Debug, Clone, Default)]
pub struct Universe {
    pub venues: Vec<Venue>,
    pub symbols: Vec<SymbolId>,
}

/// Which regimes a strategy expects to profit in (RSK-7 reads this).
#[derive(Debug, Clone, Default)]
pub struct RegimeMask {
    /// Regime labels (e.g. "trend", "high_vol"); empty ⇒ any.
    pub allowed: Vec<String>,
}

impl RegimeMask {
    pub fn any() -> Self {
        Self { allowed: vec![] }
    }
    pub fn of(labels: &[&str]) -> Self {
        Self {
            allowed: labels.iter().map(|s| s.to_string()).collect(),
        }
    }
    pub fn matches(&self, label: &str) -> bool {
        self.allowed.is_empty() || self.allowed.iter().any(|l| l == label)
    }
}

/// Walk-forward parameter grid (SIM-9 consumes this).
#[derive(Debug, Clone, Default)]
pub struct ParamSpace {
    pub grid: BTreeMap<String, Vec<f64>>,
}

/// A trading strategy. See spec 006.
pub trait Strategy {
    fn id(&self) -> mp_core::StrategyId;
    fn universe(&self) -> Universe;
    fn subscriptions(&self) -> Vec<String>;
    fn warmup_ns(&self) -> i64;
    fn declared_regime(&self) -> RegimeMask;

    /// React to a feature update. The primary decision method.
    fn on_feature(&mut self, u: &FeatureUpdate, ctx: &mut dyn Ctx) -> Vec<OrderIntent>;

    fn on_fill(&mut self, fill: &mp_core::Fill, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        let _ = (fill, ctx);
        Vec::new()
    }

    fn on_timer(&mut self, timer: TimerId, ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        let _ = (timer, ctx);
        Vec::new()
    }

    fn params(&self) -> ParamSpace {
        ParamSpace::default()
    }
}
