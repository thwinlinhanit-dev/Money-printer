//! mp-oms — order state machine + reconciler (spec 007).
//!
//! The only part of the system that can lose money by being wrong in a new way,
//! so it is paranoid by design: strategies propose, the gate disposes (spec
//! 007/`mp-risk`), the OMS never forgets an order, and the reconciler trusts the
//! venue over memory.
//!
//! v1 slice: the transport-agnostic state machine + idempotent order store +
//! reconciler, tested with no network. Venue adapters, the WAL, paper/live
//! wiring, and `oms doctor` are the same core, tracked in spec 007 Decisions.
//! Credentials load ONLY in this crate (CONV-17) when the live adapter lands.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod reconcile;
pub mod state;

pub use reconcile::{reconcile, ReconStatus};
pub use state::{IllegalTransition, OmsEvent, Order, OrderState, OrderStore};
