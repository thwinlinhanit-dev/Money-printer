//! Swing-horizon strategy metadata (spec 035). Lives in core so strategies,
//! sim, and risk all speak ONE vocabulary without a dependency from `sim`/`risk`
//! down to the strategies crate (CONV-3: strategies already depends on core;
//! sim/risk depend on core, not strategies). Same pattern as `exec`/`OrderIntent`.
#![forbid(unsafe_op_in_unsafe_fn)]

use serde::{Deserialize, Serialize};

/// When a swing strategy is allowed to re-evaluate (spec 035, SWG-3/SWG-7).
/// Swing strategies evaluate ONLY on bar close — daily or 4h — never on the
/// per-tick cadence v1 strategies use. `Event` is the legacy cadence: the v1
/// per-tick strategies keep the default and the sim dispatches them on every
/// event, unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RebalanceCadence {
    /// Legacy v1 per-tick evaluation: the sim dispatches the strategy on every
    /// event (feature update / timer / fill). Default for strategies that do
    /// not opt into bar-close semantics (SWG-7).
    Event,
    /// Bar-close re-evaluation on daily bars.
    Daily,
    /// Bar-close re-evaluation on 4h bars.
    FourHour,
}

impl RebalanceCadence {
    /// Whether this cadence requires bar-close dispatch (SWG-7): `Daily` and
    /// `FourHour` strategies may only act when the engine crosses a bar
    /// boundary. `Event` strategies are never gated.
    pub fn is_bar_close(self) -> bool {
        !matches!(self, Self::Event)
    }
}

/// Holding period as a closed `[min_bars, max_bars]` bar range (spec 035,
/// SWG-3 reads this as `Range<Bars>`). A hint for sim/risk/sizing, NOT an
/// enforced hard rule — sizing must still run its own stop/DD gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarRange {
    pub min_bars: u32,
    pub max_bars: u32,
}

impl BarRange {
    /// A closed range. Callers must ensure `min <= max`; if `min > max` the
    /// range is empty (`contains` never true).
    pub fn new(min_bars: u32, max_bars: u32) -> Self {
        Self { min_bars, max_bars }
    }
    pub fn contains(&self, bars: u32) -> bool {
        bars >= self.min_bars && bars <= self.max_bars
    }
    pub fn min(&self) -> u32 {
        self.min_bars
    }
    pub fn max(&self) -> u32 {
        self.max_bars
    }
}
