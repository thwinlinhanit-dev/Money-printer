//! OMS state-machine + reconciler tests (spec 007). Names embed IDs (CONV-21).

use mp_core::SymbolId;
use mp_oms::state::{OmsEvent, OrderState, OrderStore};
use mp_oms::{reconcile, ReconStatus};
use std::collections::BTreeMap;

#[test]
fn exe_2_state_machine_legal_path() {
    let mut store = OrderStore::new();
    let o = store.submit("mp-s-1");
    assert_eq!(o.state, OrderState::RiskChecked);
    assert_eq!(o.apply(OmsEvent::Submit).unwrap(), OrderState::Submitted);
    assert_eq!(o.apply(OmsEvent::Ack).unwrap(), OrderState::Acked);
    assert_eq!(
        o.apply(OmsEvent::Fill { complete: false }).unwrap(),
        OrderState::PartFilled
    );
    assert_eq!(
        o.apply(OmsEvent::Fill { complete: true }).unwrap(),
        OrderState::Filled
    );
    assert!(o.state.is_terminal());
}

#[test]
fn exe_2_illegal_transitions_error() {
    let mut store = OrderStore::new();
    let o = store.submit("mp-s-2");
    // Can't ack before submit.
    assert!(o.apply(OmsEvent::Ack).is_err());
    // Fill before ack is illegal.
    o.apply(OmsEvent::Submit).unwrap();
    assert!(o.apply(OmsEvent::Fill { complete: true }).is_err());
}

#[test]
fn exe_3_submit_is_idempotent() {
    let mut store = OrderStore::new();
    store.submit("mp-s-3").apply(OmsEvent::Submit).unwrap();
    // Resubmit same client id after a "crash" — no second order, state kept.
    let again = store.submit("mp-s-3");
    assert_eq!(again.state, OrderState::Submitted);
    assert_eq!(store.len(), 1);
}

#[test]
fn exe_4_unknown_resolves_by_query() {
    let mut store = OrderStore::new();
    let o = store.submit("mp-s-4");
    o.apply(OmsEvent::Submit).unwrap();
    // Connection died before ack.
    assert_eq!(o.apply(OmsEvent::AckTimeout).unwrap(), OrderState::Unknown);
    assert_eq!(store.unknown_ids(), vec!["mp-s-4".to_string()]);

    // Query the venue: order was actually acked.
    let o = store.get_mut("mp-s-4").unwrap();
    assert_eq!(o.apply(OmsEvent::ResolveAcked).unwrap(), OrderState::Acked);
    assert!(store.unknown_ids().is_empty());

    // A different unknown that the venue never saw → Failed (terminal).
    let o2 = store.submit("mp-s-5");
    o2.apply(OmsEvent::Submit).unwrap();
    o2.apply(OmsEvent::AckTimeout).unwrap();
    assert_eq!(
        o2.apply(OmsEvent::ResolveNotFound).unwrap(),
        OrderState::Failed
    );
}

#[test]
fn exe_6_reconciler_clean_and_diverged() {
    let mut internal = BTreeMap::new();
    internal.insert(SymbolId(0), 1.5);
    let mut venue = BTreeMap::new();
    venue.insert(SymbolId(0), 1.5);
    assert_eq!(reconcile(&internal, &venue, 1e-9), ReconStatus::Clean);

    // A foreign position the internal state doesn't know about ⇒ diverged.
    venue.insert(SymbolId(1), 3.0);
    match reconcile(&internal, &venue, 1e-9) {
        ReconStatus::Diverged(diffs) => {
            assert_eq!(diffs, vec![(SymbolId(1), 0.0, 3.0)]);
        }
        ReconStatus::Clean => panic!("expected divergence"),
    }
}
