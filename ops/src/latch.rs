//! Kill-latch file (OPS-3, RG-10 bridge). `/kill` and `/flatten` from the
//! Telegram bot write this file; the risk gate loads it into `KillSwitches` at
//! its boundary. This works even when `oms` is wedged — it is a file the gate
//! reads, not an RPC to oms (spec 009). Latches are one-way; only a human
//! clears the file (EXE-7 asymmetry).
//!
//! ## Persistence contract (audit H-2 / M-3)
//!
//! `KillSwitches::trip` is RAM-only. The latch FILE is the durable record, so
//! the gate boundary MUST round-trip it (a process restart after a daily-loss
//! breach must NOT reset the loss budget or revive killed strategies):
//!
//! **Live-path wiring (where the gate is evaluated — today only
//! `sim/src/engine.rs:732` via `trip_on_breach`; no ops binary evaluates the
//! gate yet).** At every site that runs `gate::evaluate`:
//!
//! 1. Before evaluation: `let mut kills = load_kill_switches_fail_closed(latch);`
//!    (merges the durable latch into the RAM set; corrupt file ⇒ a
//!    Global-tripped set — the gate blocks, it never resumes). Runtime trips
//!    already in `kills` are preserved (one-way; the file adds, never removes).
//! 2. After `trip_on_breach` yields trips:
//!    `persist_trips(latch, trips, "RG-8/9 daily-loss breach", now_ns)` —
//!    appends them to the file atomically (tmp+fsync+rename, M-3, like the
//!    telegram delivered log) so the next process restart inherits them.
//! 3. On load/merge failure the caller must translate to BLOCKED (P1), never
//!    to "resume". `load_kill_switches_fail_closed` makes that mechanical.
//!
//! Fail-closed decision table ([`load_kill_switches`]):
//!
//! | File state                  | Result |
//! |-----------------------------|--------|
//! | Missing (NotFound)          | Ok — empty set (fresh state, fine) |
//! | Valid latch JSON            | Ok — scopes applied |
//! | Unparseable / schema-wrong / unreadable (any other IO error) | Err — caller MUST treat the gate as blocked; with `load_kill_switches_fail_closed` this is a tripped `Scope::Global` |

use mp_core::{StrategyId, Venue};
use mp_risk::{KillSwitches, Scope};
use serde::{Deserialize, Serialize};
use std::io::Write;

/// A latched scope, in a portable serde form (mirrors `mp_risk::Scope`, which
/// is not itself `Serialize`). `venue` values use the core `Venue` encoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "lowercase")]
pub enum LatchScope {
    Global,
    Venue { venue: Venue },
    Strategy { id: String },
}

impl LatchScope {
    fn to_scope(&self) -> Scope {
        match self {
            LatchScope::Global => Scope::Global,
            LatchScope::Venue { venue } => Scope::Venue(*venue),
            LatchScope::Strategy { id } => Scope::Strategy(StrategyId::new(id.clone())),
        }
    }
}

/// The on-disk latch: which scopes are killed, why, and when. Serialized as
/// JSON. Append-scoped by rewriting the whole file (small, human-auditable).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KillLatch {
    pub scopes: Vec<LatchScope>,
    pub reason: String,
    /// Injected clock stamp (ns UTC) — the writer supplies it (PD-3).
    pub ts_ns: i64,
}

impl KillLatch {
    pub fn new(reason: impl Into<String>, ts_ns: i64) -> Self {
        KillLatch {
            scopes: Vec::new(),
            reason: reason.into(),
            ts_ns,
        }
    }

    /// `/flatten` = GLOBAL kill (spec 009). Convenience constructor.
    pub fn global(reason: impl Into<String>, ts_ns: i64) -> Self {
        KillLatch {
            scopes: vec![LatchScope::Global],
            reason: reason.into(),
            ts_ns,
        }
    }

    pub fn kill(mut self, scope: LatchScope) -> Self {
        self.scopes.push(scope);
        self
    }

    /// Serialize to the JSON the gate side reads.
    pub fn to_json(&self) -> Result<String, LatchError> {
        serde_json::to_string_pretty(self).map_err(|e| LatchError::Encode(e.to_string()))
    }

    /// Parse a latch file's contents.
    pub fn from_json(s: &str) -> Result<Self, LatchError> {
        serde_json::from_str(s).map_err(|e| LatchError::Decode(e.to_string()))
    }

    /// Apply every latched scope onto a `KillSwitches` (idempotent, one-way).
    /// This is the RG-10 hand-off the gate consults.
    pub fn apply_to(&self, kills: &mut KillSwitches) {
        for s in &self.scopes {
            kills.trip(s.to_scope());
        }
    }

    /// Build a fresh `KillSwitches` from this latch.
    pub fn to_kill_switches(&self) -> KillSwitches {
        let mut k = KillSwitches::new();
        self.apply_to(&mut k);
        k
    }
}

/// Latch encode/decode errors.
#[derive(Debug, thiserror::Error)]
pub enum LatchError {
    #[error("latch encode error: {0}")]
    Encode(String),
    #[error("latch decode error: {0}")]
    Decode(String),
    #[error("latch io error: {0}")]
    Io(String),
    /// The on-disk latch exists but cannot be trusted (unparseable,
    /// schema-wrong, or unreadable). NEVER maps to "no kills": the caller must
    /// treat the gate as blocked (H-2 fail-closed).
    #[error("latch corrupt — fail closed (treat the gate as blocked): {0}")]
    Corrupt(String),
}

/// Load the durable latch file into a fresh `KillSwitches` (H-2). The
/// production reader the gate boundary calls before `gate::evaluate` — see the
/// module docs for the decision table and wiring. Missing file = empty set
/// (fresh state); anything untrustworthy = `Err` the caller must translate to
/// BLOCKED (`load_kill_switches_fail_closed` does that mechanically).
pub fn load_kill_switches(path: &std::path::Path) -> Result<KillSwitches, LatchError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            // Schema gate before serde: a file that parses as JSON but is not
            // the latch shape (e.g. `{"reason": "x"}` — serde would silently
            // default `scopes` to empty and UNLATCH everything) is corrupt.
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                LatchError::Corrupt(format!("{} unparseable: {e}", path.display()))
            })?;
            let Some(scopes) = value.get("scopes") else {
                return Err(LatchError::Corrupt(format!(
                    "{} schema-wrong: no `scopes` field",
                    path.display()
                )));
            };
            if !scopes.is_array() {
                return Err(LatchError::Corrupt(format!(
                    "{} schema-wrong: `scopes` is not an array",
                    path.display()
                )));
            }
            let latch: KillLatch = serde_json::from_value(value)
                .map_err(|e| LatchError::Corrupt(format!("{} schema-wrong: {e}", path.display())))?;
            Ok(latch.to_kill_switches())
        }
        // A missing latch file is the fresh, healthy state — not an error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KillSwitches::new()),
        // Any other read failure (permissions, IO) cannot be verified: fail
        // closed. The caller must block, never resume.
        Err(e) => Err(LatchError::Corrupt(format!("{} unreadable: {e}", path.display()))),
    }
}

/// Fail-closed variant of [`load_kill_switches`]: any error (corrupt,
/// schema-wrong, unreadable) collapses to a GLOBAL-tripped set, so the gate
/// blocks every order. Never returns an "empty" set for a file that exists and
/// could not be trusted. Missing file still loads as empty (fresh state).
pub fn load_kill_switches_fail_closed(path: &std::path::Path) -> KillSwitches {
    match load_kill_switches(path) {
        Ok(kills) => kills,
        Err(e) => {
            tracing::error!(
                path = %path.display(),
                error = %e,
                "kill-latch unusable — failing CLOSED (global trip) until a human restores the file"
            );
            let mut kills = KillSwitches::new();
            kills.trip(Scope::Global);
            kills
        }
    }
}

/// Durable atomic latch write (M-3, same pattern as the telegram delivered
/// log): write to `{path}.tmp`, `File::sync_all`, then rename over `path`. A
/// crash or concurrent reader sees either the previous latch or the new one —
/// never a torn file.
pub fn write_latch_atomic(path: &std::path::Path, latch: &KillLatch) -> Result<(), LatchError> {
    let json = latch.to_json()?;
    let tmp = tmp_path(path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LatchError::Io(format!("{}: {e}", parent.display())))?;
        }
    }
    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| LatchError::Io(format!("{}: {e}", tmp.display())))?;
    file.write_all(json.as_bytes())
        .map_err(|e| LatchError::Io(format!("{}: {e}", tmp.display())))?;
    file.sync_all()
        .map_err(|e| LatchError::Io(format!("{}: {e}", tmp.display())))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| LatchError::Io(format!("{}: {e}", path.display())))?;
    Ok(())
}

fn tmp_path(path: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.tmp", path.display()))
}

/// Append newly tripped scopes to the durable latch (H-2 step 2): the
/// one-way, idempotent persistence half of the gate boundary. Already-latched
/// scopes are never duplicated; the existing reason/ts are preserved when the
/// latch already exists (the original human-auditable cause wins). The write
/// is atomic ([`write_latch_atomic`]).
///
/// Fail-closed: an existing CORRUPT latch is REFUSED, never overwritten —
/// rewriting it would silently drop scopes we could not read and revive killed
/// strategies on restart. The caller must surface the error as BLOCKED + P1.
pub fn persist_trips(
    path: &std::path::Path,
    trips: impl IntoIterator<Item = LatchScope>,
    reason: &str,
    ts_ns: i64,
) -> Result<KillLatch, LatchError> {
    // Validate the existing file FIRST (also answers "missing = fresh").
    let existing = load_kill_switches(path)
        .map_err(|e| LatchError::Corrupt(format!("refusing to overwrite: {e}")))?;
    let mut latch = if existing.any_tripped() {
        // Schema-validated above, so this second read/parse cannot fail.
        let text =
            std::fs::read_to_string(path).map_err(|e| LatchError::Io(format!("{}: {e}", path.display())))?;
        KillLatch::from_json(&text)
            .map_err(|e| LatchError::Corrupt(format!("{}: {e}", path.display())))?
    } else {
        KillLatch::new(reason, ts_ns)
    };
    for trip in trips {
        if !latch.scopes.contains(&trip) {
            latch.scopes.push(trip);
        }
    }
    write_latch_atomic(path, &latch)?;
    Ok(latch)
}
