//! Kill switches (EXE-7): one-way latches. Once tripped they stay tripped until
//! a human explicitly resets — the safety asymmetry made mechanical. The system
//! can always take risk OFF by itself; only a human turns it back ON.

use mp_core::{StrategyId, Venue};
use std::collections::BTreeSet;

/// A tripped scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Global,
    Venue(Venue),
    Strategy(StrategyId),
}

/// Set of tripped kill switches. Latches are one-way (EXE-7).
#[derive(Debug, Clone, Default)]
pub struct KillSwitches {
    tripped: BTreeSet<Scope>,
}

impl KillSwitches {
    pub fn new() -> Self {
        Self::default()
    }

    /// Trip a scope (idempotent). Never requires a human — risk-off is free.
    pub fn trip(&mut self, scope: Scope) {
        self.tripped.insert(scope);
    }

    /// Reset a scope — requires a human (EXE-7). Agents cannot pass `true`.
    pub fn reset(&mut self, scope: &Scope, human: bool) -> Result<(), ResetRefused> {
        if !human {
            return Err(ResetRefused);
        }
        self.tripped.remove(scope);
        Ok(())
    }

    /// Whether an order for `(venue, strategy)` is blocked by any latch.
    pub fn blocks(&self, venue: Venue, strategy: &StrategyId) -> bool {
        self.tripped.contains(&Scope::Global)
            || self.tripped.contains(&Scope::Venue(venue))
            || self.tripped.contains(&Scope::Strategy(strategy.clone()))
    }

    pub fn is_tripped(&self, scope: &Scope) -> bool {
        self.tripped.contains(scope)
    }

    pub fn any_tripped(&self) -> bool {
        !self.tripped.is_empty()
    }
}

/// Returned when a reset is attempted without human authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("kill-switch reset requires a human (EXE-7)")]
pub struct ResetRefused;
