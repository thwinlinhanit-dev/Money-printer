//! Reconciler (EXE-6). Reconciliation — not order acks — is the source of
//! truth. Every interval and on every reconnect, diff internal positions
//! against what the venue reports; any mismatch freezes new intents for that
//! venue and alerts (the caller wires RG-11 + kill switch).

use mp_core::SymbolId;
use std::collections::BTreeMap;

/// Reconciliation status for one venue.
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
