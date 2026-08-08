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

pub use reconcile::{
    reconcile, reconcile_balances, reconcile_orders, OrderReconStatus, ReconStatus,
};
pub use state::{IllegalTransition, OmsEvent, Order, OrderState, OrderStore};

/// EXE-10 offline sanity used by `oms doctor`: drive the full legal state
/// graph and assert it still holds. A regression in the legal-transition
/// graph is a crash in the field, so this is a runtime health check, not just
/// a test.
pub fn state_machine_self_test() -> Result<(), String> {
    use OmsEvent::*;
    use OrderState::*;

    // Full happy path.
    let mut store = OrderStore::new();
    store.submit("self-test");
    let expect = [
        (Submit, Submitted),
        (Ack, Acked),
        (Fill { complete: false }, PartFilled),
        (Fill { complete: true }, Filled),
    ];
    for (ev, want) in expect {
        match store.apply("self-test", ev, 0) {
            Some(Ok(got)) if got == want => {}
            other => {
                return Err(format!(
                    "legal path diverged at {ev:?}: {other:?} (want {want:?})"
                ))
            }
        }
    }

    // Unknown resolution path.
    let mut store = OrderStore::new();
    store.submit("self-test");
    store.apply("self-test", Submit, 0);
    match store.apply("self-test", AckTimeout, 0) {
        Some(Ok(Unknown)) => {}
        other => return Err(format!("AckTimeout should yield Unknown: {other:?}")),
    }
    match store.apply("self-test", ResolveNotFound, 0) {
        Some(Ok(Failed)) => {}
        other => return Err(format!("ResolveNotFound should yield Failed: {other:?}")),
    }

    // Illegal transitions must error, never silently change state.
    let mut store = OrderStore::new();
    store.submit("self-test");
    if store
        .apply("self-test", Ack, 0)
        .is_some_and(|r| r.is_ok())
    {
        return Err("ack-before-submit must be illegal".into());
    }
    Ok(())
}
