//! OMS order state machine (EXE-2/3/4). The `Unknown` state — order sent, no
//! ack, connection died — is the one that costs real money, so it is
//! first-class: while any order is `Unknown` for a venue the OMS freezes new
//! intents for it (the caller wires RG-11). Submit is idempotent by client
//! order id (EXE-3): a resubmit after a crash never creates a second order.

use std::collections::BTreeMap;

/// Order lifecycle states (spec 007 state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    RiskChecked,
    Submitted,
    Acked,
    PartFilled,
    Filled,
    Cancelled,
    Rejected,
    /// Sent, no ack, connection lost — resolve by querying the venue (EXE-4).
    Unknown,
    /// Terminal failure (e.g. Unknown resolved to NotFound).
    Failed,
}

impl OrderState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderState::Filled | OrderState::Cancelled | OrderState::Rejected | OrderState::Failed
        )
    }
}

/// Events that drive the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmsEvent {
    Submit,
    Ack,
    Fill {
        complete: bool,
    },
    Cancel,
    Reject,
    /// No ack within the timeout / connection dropped.
    AckTimeout,
    /// Resolutions of an `Unknown` order after querying the venue (EXE-4).
    ResolveAcked,
    ResolveRejected,
    ResolveNotFound,
}

/// Illegal-transition error (EXE-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("illegal OMS transition from {from:?} on {event:?}")]
pub struct IllegalTransition {
    pub from: OrderState,
    pub event: OmsEvent,
}

/// A tracked order.
#[derive(Debug, Clone)]
pub struct Order {
    pub client_id: String,
    pub state: OrderState,
}

impl Order {
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            state: OrderState::RiskChecked,
        }
    }

    /// Apply an event, enforcing the legal transition graph (EXE-2). Illegal
    /// transitions never silently corrupt state.
    pub fn apply(&mut self, event: OmsEvent) -> Result<OrderState, IllegalTransition> {
        use OmsEvent::*;
        use OrderState::*;
        let next = match (self.state, event) {
            (RiskChecked, Submit) => Submitted,
            (Submitted, Ack) => Acked,
            (Submitted, Reject) => Rejected,
            (Submitted, AckTimeout) => Unknown,
            (Acked, Fill { complete: false }) => PartFilled,
            (Acked, Fill { complete: true }) => Filled,
            (Acked, Cancel) => Cancelled,
            (PartFilled, Fill { complete: false }) => PartFilled,
            (PartFilled, Fill { complete: true }) => Filled,
            (PartFilled, Cancel) => Cancelled,
            (Unknown, ResolveAcked) => Acked,
            (Unknown, ResolveRejected) => Rejected,
            (Unknown, ResolveNotFound) => Failed,
            (from, event) => return Err(IllegalTransition { from, event }),
        };
        self.state = next;
        Ok(next)
    }
}

/// Idempotent order store keyed by client order id (EXE-3).
#[derive(Debug, Clone, Default)]
pub struct OrderStore {
    orders: BTreeMap<String, Order>,
}

impl OrderStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Submit (idempotently). A duplicate client id returns the existing order
    /// without creating a second one (EXE-3) — crash + resubmit is safe.
    pub fn submit(&mut self, client_id: &str) -> &mut Order {
        self.orders
            .entry(client_id.to_owned())
            .or_insert_with(|| Order::new(client_id))
    }

    pub fn get(&self, client_id: &str) -> Option<&Order> {
        self.orders.get(client_id)
    }

    pub fn get_mut(&mut self, client_id: &str) -> Option<&mut Order> {
        self.orders.get_mut(client_id)
    }

    /// Client ids currently in the `Unknown` state (freeze the venue until
    /// these resolve — EXE-4/RG-11).
    pub fn unknown_ids(&self) -> Vec<String> {
        self.orders
            .values()
            .filter(|o| o.state == OrderState::Unknown)
            .map(|o| o.client_id.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.orders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }
}
