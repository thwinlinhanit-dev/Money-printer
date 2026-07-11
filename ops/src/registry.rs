//! Alert catalog (OPS-4). The canonical list of alert ids and their severity.
//! Every entry MUST have a matching `ops/runbooks/{id}.md`; the guardrails lint
//! (`ops/ci/guardrails.sh`) enforces ids ↔ files mechanically, and
//! `registry_every_alert_has_a_runbook` proves it in-crate. Adding an alert
//! without its runbook fails CI.
//!
//! Grep-friendly on purpose: each row is one `alert!(...)` line the bash lint
//! can extract.

use crate::alert::Severity;

/// A catalog entry: the id and its severity. The runbook path is derived
/// (`ops/runbooks/{id}.md`).
#[derive(Debug, Clone, Copy)]
pub struct AlertSpec {
    pub id: &'static str,
    pub severity: Severity,
}

macro_rules! alert {
    ($id:literal, $sev:ident) => {
        AlertSpec {
            id: $id,
            severity: Severity::$sev,
        }
    };
}

/// The canonical alert catalog. P1/P2 ids from the spec 009 alert-policy table,
/// plus the dead-man and ops-health alerts (OPS-2/OPS-7).
pub const ALERTS: &[AlertSpec] = &[
    alert!("process-deadman", P2),
    alert!("stream-gap", P2),
    alert!("collector-down", P2),
    alert!("disk-high", P2),
    alert!("determinism-diff", P2),
    alert!("clock-skew", P2),
    alert!("keyfile-perms", P2),
    alert!("recon-diverged", P1),
    alert!("unknown-order", P1),
    alert!("oms-down", P1),
    alert!("killswitch-tripped", P1),
];

/// Runbook path for an alert id (spec 009 convention).
pub fn runbook_path(id: &str) -> String {
    format!("ops/runbooks/{id}.md")
}

/// Look up a catalog entry by id.
pub fn spec_for(id: &str) -> Option<&'static AlertSpec> {
    ALERTS.iter().find(|a| a.id == id)
}
