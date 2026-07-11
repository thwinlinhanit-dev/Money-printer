//! mp-risk — risk & sizing engine (spec 008).
//!
//! Runs *above* strategies and *below* the risk gate: converts risk-unit
//! intents into contracts (vol targeting), caps everything by quarter-Kelly and
//! drawdown governors, and allocates capital across strategies. Sizing is where
//! identical signals become winners or corpses. Pure math over core types — no
//! venue access (PD-4), no wall clock (PD-3).
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod allocator;
pub mod config;
pub mod gate;
pub mod governor;
pub mod kelly;
pub mod killswitch;
pub mod sizing;

pub use allocator::{allocate, shrink_only, AllocParams, StrategyInput};
pub use config::{regime_fit_from_features, ConfigError, RiskConfig};
pub use gate::{evaluate, GateInput, Mode, RejectReason, RiskLimits, Verdict};
pub use governor::dd_governor;
pub use kelly::{dd_budget_from_mc, full_kelly, kelly_cap, KellyParams, KellyStats};
pub use killswitch::{KillSwitches, ResetRefused, Scope};
pub use sizing::{size, SizedOrder, SizingInputs, SizingParams, SizingTrace};
