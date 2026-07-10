//! mp-sim — backtester & simulation (spec 005).
//!
//! The honest evaluation machine: event-replay simulation that runs the
//! PRODUCTION feature engine, strategy, and sizing crates unmodified (SIM-5) —
//! only the clock and fill model differ from live. Its job is to kill bad ideas
//! cheaply and make live/backtest divergence measurable.
//!
//! v1 slice: replay + taker fill + average-cost accounting with an exact
//! identity + expectancy metrics + a deterministic decision log. The L0/L2 fill
//! models, walk-forward/Monte-Carlo harnesses, and the experiment tracker are
//! the same event core, tracked in spec 005 Decisions.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod account;
pub mod decision_log;
pub mod engine;
pub mod metrics;

pub use account::Accountant;
pub use decision_log::DecisionLog;
pub use engine::{Backtester, SimConfig};
pub use metrics::Metrics;
