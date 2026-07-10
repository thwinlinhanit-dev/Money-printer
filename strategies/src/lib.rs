//! mp-strategies — Strategy API + promotion funnel (spec 006).
//!
//! The contract strategy code lives under, and the gauntlet it must survive to
//! touch money. The API is deliberately small; the funnel is deliberately slow.
//! Strategies emit `OrderIntent` only and cannot reach a venue (PD-4) — this
//! crate has no oms/collectors/network dependency, enforced by the guardrail.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod examples;
pub mod funnel;
pub mod strategy;

pub use examples::{CoinFlipStrategy, NullStrategy};
pub use funnel::{Actor, FunnelError, FunnelState, Stage, Transition};
pub use strategy::{Ctx, ParamSpace, RegimeMask, Strategy, TimerId, Universe};
