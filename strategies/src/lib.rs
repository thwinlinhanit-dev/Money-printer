//! mp-strategies — Strategy API + promotion funnel (spec 006).
//!
//! The contract strategy code lives under, and the gauntlet it must survive to
//! touch money. The API is deliberately small; the funnel is deliberately slow.
//! Strategies emit `OrderIntent` only and cannot reach a venue (PD-4) — this
//! crate has no oms/collectors/network dependency, enforced by the guardrail.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod carry_v1;
pub mod examples;
pub mod funnel;
pub mod liq_fade_v1;
pub mod orderflow_v1;
pub mod strategy;
pub mod swing_range_reclaim_v1;

pub use carry_v1::{CarryConfig, CarryV1};
pub use examples::{CoinFlipStrategy, NullStrategy};
pub use funnel::{
    Actor, Autopsy, EvidenceRef, FunnelError, FunnelState, Stage, Transition, EVIDENCE_MAX_AGE_NS,
};
pub use liq_fade_v1::{LiqFadeConfig, LiqFadeV1};
pub use orderflow_v1::{OrderflowConfig, OrderflowV1};
pub use strategy::{Ctx, ParamSpace, RegimeMask, Strategy, TimerId, Universe};
pub use swing_range_reclaim_v1::{RangeReclaimConfig, SwingRangeReclaimV1};

/// Strategies frozen by spec 035 SWG-8 — retained, NOT removed, but no swing
/// strategy/backtest/acceptance criterion may depend on them until a separate
/// deprecation decision. Keep this list in sync with spec 035 §Freeze list.
pub const FROZEN_STRATEGIES: &[&str] = &["liq-fade-v1"];
