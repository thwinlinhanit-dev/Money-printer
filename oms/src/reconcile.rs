//! Reconciler (EXE-6). Reconciliation — not order acks — is the source of
//! truth. Every interval and on every reconnect, diff internal positions,
//! balances, and orders against what the venue reports; any mismatch freezes
//! new intents for that venue and alerts (the caller wires RG-11 + kill
//! switch).

use crate::state::{OrderState, OrderStore};
use mp_core::SymbolId;
use std::collections::BTreeMap;

/// Reconciliation status for one venue (positions / balances shape).
#[derive(Debug, Clone, PartialEq)]
pub enum ReconStatus {
    Clean,
    /// Positions that differ, as `(symbol, internal, venue)`.
    Diverged(Vec<(SymbolId, f64, f64)>),
}

impl ReconStatus {
    pub fn is_clean(&self) -> bool {
        matches!(self, ReconStatus::Clean)
    }
}

/// Diff internal vs venue positions with an absolute tolerance (EXE-6).
/// A symbol present on one side only counts as `0.0` on the other — an unknown
/// venue position is exactly the divergence that must freeze trading.
pub fn reconcile(
    internal: &BTreeMap<SymbolId, f64>,
    venue: &BTreeMap<SymbolId, f64>,
    tol: f64,
) -> ReconStatus {
    let mut diffs = Vec::new();
    let mut symbols: Vec<SymbolId> = internal.keys().chain(venue.keys()).copied().collect();
    symbols.sort();
    symbols.dedup();
    for s in symbols {
        let a = internal.get(&s).copied().unwrap_or(0.0);
        let b = venue.get(&s).copied().unwrap_or(0.0);
        if (a - b).abs() > tol {
            diffs.push((s, a, b));
        }
    }
    if diffs.is_empty() {
        ReconStatus::Clean
    } else {
        ReconStatus::Diverged(diffs)
    }
}

/// Balances have exactly the positions shape (asset id → free/used amount),
/// so the same diff applies; the name makes the EXE-6 balance check explicit
/// at call sites.
pub fn reconcile_balances(
    internal: &BTreeMap<SymbolId, f64>,
    venue: &BTreeMap<SymbolId, f64>,
    tol: f64,
) -> ReconStatus {
    reconcile(internal, venue, tol)
}

/// Order-shape reconciliation (EXE-6): internal order store vs the venue's
/// view of the same client ids. A foreign venue order (no internal record) and
/// a lost internal order (venue never saw it) both freeze the venue — neither
/// can be explained without a human.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderReconStatus {
    Clean,
    /// Internal orders the venue has no record of (submission may have failed).
    MissingVenue(Vec<String>),
    /// Venue orders with no internal record (foreign orders).
    MissingInternal(Vec<String>),
    /// Same client id, differing state — `(client_id, internal, venue)`.
    StateMismatch(Vec<(String, OrderState, OrderState)>),
}

impl OrderReconStatus {
    pub fn is_clean(&self) -> bool {
        matches!(self, OrderReconStatus::Clean)
    }
}

/// Diff internal vs venue orders by client id (EXE-6 order shape).
pub fn reconcile_orders(
    internal: &OrderStore,
    venue: &BTreeMap<String, OrderState>,
) -> OrderReconStatus {
    let mut ids: Vec<String> = Vec::new();
    ids.extend(internal.ids());
    ids.extend(venue.keys().cloned());
    ids.sort();
    ids.dedup();

    let mut missing_venue = Vec::new();
    let mut missing_internal = Vec::new();
    let mut mismatch = Vec::new();
    for id in ids {
        match (internal.get(&id), venue.get(&id)) {
            (Some(_), None) => missing_venue.push(id),
            (None, Some(_)) => missing_internal.push(id),
            (Some(a), Some(b)) if a.state != *b => mismatch.push((id, a.state, *b)),
            _ => {}
        }
    }
    if !missing_venue.is_empty() {
        OrderReconStatus::MissingVenue(missing_venue)
    } else if !missing_internal.is_empty() {
        OrderReconStatus::MissingInternal(missing_internal)
    } else if !mismatch.is_empty() {
        OrderReconStatus::StateMismatch(mismatch)
    } else {
        OrderReconStatus::Clean
    }
}
