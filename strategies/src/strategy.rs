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

    /// Return a new instance with the given walk-forward param overrides.
    /// Strategies without a param grid may ignore the map and return a fresh
    /// default instance (there is no default impl — every strategy must
    /// provide one; `params()` declares which keys the grid may set).
    fn with_params(&self, _params: &BTreeMap<String, f64>) -> Box<dyn Strategy>;

    // ---- swing-horizon metadata (spec 035, SWG-3) --------------------------
    //
    // These are HINTS for sim/risk/sizing, NOT hard rules. Swing strategies
    // override them; the v1 per-tick strategies keep the event-driven default
    // (`Event` — evaluated on every event, the sim's legacy behavior, SWG-7).

    /// `[min_bars, max_bars]` holding-period window. Default `[1, MAX]` —
    /// any horizon. Sizing uses it only to compute expected funding drag; the
    /// risk gate's own stops/DD limits still rule (SWG-6, 000 PD-5).
    fn holding_period_bars(&self) -> mp_core::BarRange {
        mp_core::BarRange::new(1, u32::MAX)
    }

    /// When the strategy may re-evaluate (spec 035 SWG-3). Swing strategies
    /// override with `Daily`/`FourHour` and the sim dispatches them ONLY on
    /// bar close (SWG-7). Legacy per-tick strategies keep the default `Event`
    /// cadence — every event, unchanged.
    fn rebalance_cadence(&self) -> mp_core::RebalanceCadence {
        mp_core::RebalanceCadence::Event
    }
}
