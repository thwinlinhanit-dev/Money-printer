//! mp-collectors — venue WS collectors → normalized events (spec 002).
//!
//! This crate is the **transport-agnostic** core: normalization, book sync,
//! reconnect/backoff policy, rate budgets, and staleness detection, all driven
//! by the [`transport::Transport`] trait and tested via a mock. The live
//! WebSocket transport (tokio + tokio-tungstenite) is intentionally **not**
//! here yet — adding a network dependency is must-ask-first (CLAUDE.md). When
//! approved, it implements `Transport` and nothing else in this crate changes.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backoff;
pub mod bybit;
pub mod collector;
pub mod normalize;
pub mod rate;
pub mod rng;
pub mod staleness;
pub mod transport;

pub use backoff::Backoff;
pub use bybit::BybitNormalizer;
pub use collector::{Collector, CollectorConfig, DriveOutcome};
pub use normalize::{HealthCounters, NormError, Normalizer};
pub use rate::RateBudget;
pub use staleness::Staleness;
pub use transport::{MockTransport, Transport, TransportEvent};
