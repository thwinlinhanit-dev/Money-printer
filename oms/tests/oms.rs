//! OMS state-machine + reconciler tests (spec 007). Names embed IDs (CONV-21).

use mp_core::SymbolId;
use mp_oms::state::{OmsEvent, OrderState, OrderStore};
use mp_oms::{reconcile, reconcile_orders, OrderReconStatus, ReconStatus};
use proptest::prelude::*;
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

#[test]
fn exe_4_unknown_timeout_escalation() {
    let mut store = OrderStore::new();
    store.submit("mp-s-6").apply(OmsEvent::Submit).unwrap();
    let now = 1_000_000_000i64;
    // No ack: Unknown begins timing.
    store.apply("mp-s-6", OmsEvent::AckTimeout, now).unwrap().unwrap();
    assert!(store.unknown_expired(now, 60_000_000_000).is_empty(), "not yet expired");
    // Under the 60s unexpired.
    assert!(store.unknown_expired(now + 59_000_000_000, 60_000_000_000).is_empty());
    // Past the 60s window → escalation set (caller maps to venue kill switch).
    assert_eq!(
        store.unknown_expired(now + 61_000_000_000, 60_000_000_000),
        vec!["mp-s-6".to_string()]
    );
    // Query resolves it: no longer unknown, no longer escalating.
    store.apply("mp-s-6", OmsEvent::ResolveAcked, now + 61_000_000_000).unwrap().unwrap();
    assert!(store.unknown_expired(now + 61_000_000_000, 60_000_000_000).is_empty());
    assert!(store.unknown_ids().is_empty());
}

#[test]
fn exe_6_reconcile_orders_clean_and_diverged() {
    // Clean: identical views.
    let mut internal = OrderStore::new();
    internal.submit("mp-o-1").apply(OmsEvent::Submit).unwrap();
    internal.submit("mp-o-2").apply(OmsEvent::Submit).unwrap();
    internal.apply("mp-o-1", OmsEvent::Ack, 0).unwrap().unwrap();
    let mut venue = BTreeMap::new();
    venue.insert("mp-o-1".into(), OrderState::Acked);
    venue.insert("mp-o-2".into(), OrderState::Submitted);
    assert_eq!(reconcile_orders(&internal, &venue), OrderReconStatus::Clean);

    // Loss: venue never saw an internal order.
    internal.submit("mp-o-3");
    assert!(matches!(
        reconcile_orders(&internal, &venue),
        OrderReconStatus::MissingVenue(ids) if ids == vec!["mp-o-3".to_string()]
    ));

    // Foreign venue order (complete view: every internal id also present).
    let mut venue = BTreeMap::new();
    venue.insert("mp-o-1".into(), OrderState::Acked);
    venue.insert("mp-o-2".into(), OrderState::Submitted);
    venue.insert("mp-o-3".into(), OrderState::RiskChecked);
    venue.insert("mp-o-x".into(), OrderState::Acked);
    assert!(matches!(
        reconcile_orders(&internal, &venue),
        OrderReconStatus::MissingInternal(ids) if ids == vec!["mp-o-x".to_string()]
    ));

    // Same id, differing state (complete view again).
    let mut venue = BTreeMap::new();
    venue.insert("mp-o-1".into(), OrderState::Submitted); // internal says Acked
    venue.insert("mp-o-2".into(), OrderState::Submitted);
    venue.insert("mp-o-3".into(), OrderState::RiskChecked);
    assert!(matches!(
        reconcile_orders(&internal, &venue),
        OrderReconStatus::StateMismatch(diffs)
            if diffs == vec![("mp-o-1".to_string(), OrderState::Acked, OrderState::Submitted)]
    ));
}

/// Arbitrary legal events (the OMS state machine is transport-agnostic).
fn any_event() -> impl Strategy<Value = OmsEvent> {
    prop_oneof![
        Just(OmsEvent::Submit),
        Just(OmsEvent::Ack),
        Just(OmsEvent::Fill { complete: false }),
        Just(OmsEvent::Fill { complete: true }),
        Just(OmsEvent::Cancel),
        Just(OmsEvent::Reject),
        Just(OmsEvent::AckTimeout),
        Just(OmsEvent::ResolveAcked),
        Just(OmsEvent::ResolveRejected),
        Just(OmsEvent::ResolveNotFound),
    ]
}

/// Integration-test proptest config: no failure persistence files (this test
/// has no lib.rs/main.rs source to anchor them).
fn no_persist() -> proptest::test_runner::Config {
    proptest::test_runner::Config {
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    }
}

proptest! {
    #![proptest_config(no_persist())]
    /// CONV-22: the state machine is TOTAL (every (state, event) is either a
    /// legal Ok(next) or an IllegalTransition — never a panic, never a silent
    /// state change) and TERMINAL states are absorbing (no event may legally
    /// leave Filled/Cancelled/Rejected/Failed). Drives the store-level API so
    /// the `Unknown` escalation bookkeeping is property-tested too.
    #[test]
    fn conv_22_state_machine_total_and_absorbing_after_random_walks(
        store_id in 0u32..8,
        events in prop::collection::vec(any_event(), 0..64),
        now_ns in 0i64..1_000_000_000_000,
    ) {
        let mut store = OrderStore::new();
        let id = format!("mp-p-{store_id}");
        store.submit(&id);
        // Walk: every step must be Ok or IllegalTransition (never panic), and
        // once terminal, every later step must be Illegal.
        let mut terminal = false;
        for (i, ev) in events.into_iter().enumerate() {
            let before = store.get(&id).unwrap().state;
            match store.apply(&id, ev, now_ns + i as i64) {
                Some(Ok(next)) if terminal => {
                    // Terminal states are absorbing (CONV-22).
                    panic!("terminal {before:?} accepted {ev:?}");
                }
                Some(Ok(next)) => terminal = next.is_terminal(),
                Some(Err(_)) => {
                    // Illegal transitions must never mutate.
                    prop_assert_eq!(store.get(&id).unwrap().state, before);
                }
                None => panic!("order vanished"),
            }
        }
        // Freeze consistency: unknown_ids() lists every currently-Unknown order,
        // and each carries an escalation timestamp.
        for id in store.unknown_ids() {
            let o = store.get(&id).unwrap();
            prop_assert_eq!(o.state, OrderState::Unknown);
            prop_assert!(o.unknown_since_ns.is_some());
        }
    }
}

#[test]
fn exe_10_doctor_self_test_passes() {
    assert!(mp_oms::state_machine_self_test().is_ok());
}
