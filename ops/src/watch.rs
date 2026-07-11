//! Host-health checks (OPS-7): disk budget (STO-7), clock skew, and key-file
//! permissions. Pure decision functions — the opsd binary samples the host
//! (statvfs, NTP, stat) and passes readings in; the thresholds and alert
//! construction live here where they are testable.

use crate::alert::{Alert, Severity};

/// Disk usage above `max_frac` (default 0.85, STO-7) raises `disk-high` (P2).
/// Never auto-deletes anything — alert only (W-6).
pub fn disk_alert(used_frac: f64, max_frac: f64, dedupe_ns: i64) -> Option<Alert> {
    if used_frac > max_frac {
        Some(Alert::new(
            "disk-high",
            Severity::P2,
            dedupe_ns,
            format!("disk {used_frac:.1}% used (budget {max_frac:.1}%)"),
        ))
    } else {
        None
    }
}

/// Clock skew beyond 100ms raises `clock-skew` (P2) — lead-lag research and
/// venue timestamp deltas are garbage on a skewed clock (OPS-7).
pub fn clock_skew_alert(skew_ns: i64, dedupe_ns: i64) -> Option<Alert> {
    const MAX_SKEW_NS: i64 = 100_000_000; // 100ms
    if skew_ns.abs() > MAX_SKEW_NS {
        Some(Alert::new(
            "clock-skew",
            Severity::P2,
            dedupe_ns,
            format!("clock skew {skew_ns}ns exceeds 100ms"),
        ))
    } else {
        None
    }
}

/// A credential/cert file readable by group/other raises `keyfile-perms`
/// (P2). `mode` is the unix permission bits (e.g. 0o600).
pub fn keyfile_perms_alert(path: &str, mode: u32, dedupe_ns: i64) -> Option<Alert> {
    if mode & 0o077 != 0 {
        Some(
            Alert::new(
                "keyfile-perms",
                Severity::P2,
                dedupe_ns,
                format!("{path} has mode {mode:o}; require 0600"),
            )
            .with_dedupe_key(format!("keyfile-perms/{path}")),
        )
    } else {
        None
    }
}
