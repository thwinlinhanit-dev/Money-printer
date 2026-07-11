//! Kill-latch file (OPS-3, RG-10 bridge). `/kill` and `/flatten` from the
//! Telegram bot write this file; the risk gate loads it into `KillSwitches` at
//! its boundary. This works even when `oms` is wedged — it is a file the gate
//! reads, not an RPC to oms (spec 009). Latches are one-way; only a human
//! clears the file (EXE-7 asymmetry).

use mp_core::{StrategyId, Venue};
use mp_risk::{KillSwitches, Scope};
use serde::{Deserialize, Serialize};

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
}
