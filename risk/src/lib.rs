//! mp-risk — risk & sizing engine (spec 008).
//!
//! Runs *above* strategies and *below* the risk gate: converts risk-unit
//! intents into contracts (vol targeting), caps everything by quarter-Kelly and
//! drawdown governors, and allocates capital across strategies. Sizing is where
//! identical signals become winners or corpses. Pure math over core types — no
//! venue access (PD-4), no wall clock (PD-3).
//!
//! NOTE: the backtester's `SimConfig::default()` (sim crate) uses INFINITE
//! daily-loss budgets (`f64::INFINITY`). That scope is simulator-only — it keeps
//! RG-8/RG-9 inert so a backtest is never stopped by a loss budget (the sim is
//! a measurement, not a live process). It is the SIM engine's concern, is NOT a
//! live/executable default, and is intentionally NOT the gate's own
//! `RiskLimits::default()` here. Live/paper configs must set finite budgets in
//! `risk.toml`.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod allocator;
pub mod config;
pub mod gate;
pub mod governor;
pub mod kelly;
pub mod killswitch;
pub mod portfolio;
pub mod sizing;

pub use allocator::{allocate, shrink_only, AllocParams, StrategyInput};
pub use config::{regime_fit_from_features, ConfigError, RiskConfig};
pub use gate::{
    evaluate, trip_on_breach, GateInput, Mode, RejectReason, RiskLimits, TripRequest, Verdict,
};
pub use governor::dd_governor;
pub use kelly::{dd_budget_from_mc, full_kelly, kelly_cap, KellyParams, KellyStats};
pub use killswitch::{KillSwitches, ResetRefused, Scope};
pub use portfolio::{
    correlation_adjusted_exposure, cumulative_funding_cost, expected_return_net_of_funding,
};
pub use sizing::{size, SizedOrder, SizingInputs, SizingParams, SizingTrace};
