//! mp-sim — backtester & simulation (spec 005).
//!
//! The honest evaluation machine: event-replay simulation that runs the
//! PRODUCTION feature engine, strategy, and sizing crates unmodified (SIM-5) —
//! only the clock and fill model differ from live. Its job is to kill bad ideas
//! cheaply and make live/backtest divergence measurable.
//!
//! v1 slice: replay + the L0/L1/L2 fill-model ladder + average-cost accounting
//! with an exact identity + expectancy metrics (with the SIM-8 2x-cost stress
//! column and SIM-12 optimistic-maker split) + a deterministic decision log +
//! SIM-4/SIM-6 run-refusal on missing funding / low coverage. The
//! walk-forward/Monte-Carlo harnesses and the experiment tracker are the same
//! event core, tracked in spec 005 Decisions.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod account;
pub mod decision_log;
pub mod engine;
pub mod error;
pub mod metrics;

pub use account::Accountant;
pub use decision_log::DecisionLog;
pub use engine::{Backtester, FillModel, SimConfig};
pub use error::SimError;
pub use metrics::Metrics;
