//! Token-usage accumulator (spec 010 cost accounting). Providers parse
//! [`Usage`] out of every response; the research caller needs the aggregates,
//! not per-call values that were previously parsed and discarded. Thread-safe
//! (`Arc<AtomicU64>` a copyable snapshot) so one accumulator can be shared
//! across call sites on the blocking transport path.
//!
//! Not on any decision path (spec 010) — this is cost telemetry for the owner.

use crate::provider::Usage;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Default maximum tracked usage before accumulation fails closed
/// (`UsageOverflow`). ~2^40 tokens per direction is far past any plausible
/// owner budget; the cap exists so a misbehaving provider cannot silently
/// wrap the counters.
pub const DEFAULT_USAGE_CAP: u64 = 1u64 << 40;

/// Accumulator error.
#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    /// Accumulated usage would exceed the configured cap.
    #[error("llm usage accumulator overflow (> {0} tokens)")]
    Overflow(u64),
}

/// Thread-safe running totals of input/output tokens across provider calls.
/// Cheap to clone (shares the same counters).
#[derive(Debug, Clone)]
pub struct UsageAccumulator {
    input: Arc<AtomicU64>,
    output: Arc<AtomicU64>,
    calls: Arc<AtomicU64>,
    cap: u64,
}

impl Default for UsageAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageAccumulator {
    /// New accumulator with [`DEFAULT_USAGE_CAP`].
    pub fn new() -> Self {
        Self::with_cap(DEFAULT_USAGE_CAP)
    }

    /// New accumulator with a caller-chosen cap (0 = disabled cap is **not**
    /// supported on purpose: fail closed beats unbounded).
    pub fn with_cap(cap: u64) -> Self {
        debug_assert!(cap > 0, "usage cap must be positive (fail closed)");
        UsageAccumulator {
            input: Arc::new(AtomicU64::new(0)),
            output: Arc::new(AtomicU64::new(0)),
            calls: Arc::new(AtomicU64::new(0)),
            cap: cap.max(1),
        }
    }

    /// Record one completion's usage. Bounded arithmetic: the running totals
    /// may never exceed the cap (fail closed with `UsageError::Overflow`).
    pub fn record(&self, usage: Usage) -> Result<(), UsageError> {
        let next_in = self.input.load(Ordering::Relaxed) + u64::from(usage.input_tokens);
        let next_out = self.output.load(Ordering::Relaxed) + u64::from(usage.output_tokens);
        if next_in > self.cap || next_out > self.cap {
            return Err(UsageError::Overflow(self.cap));
        }
        self.input.store(next_in, Ordering::Relaxed);
        self.output.store(next_out, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Running totals plus the number of recorded calls (provider-reported
    /// counters saturate at `u32::MAX` per `u32_at`'s fail-closed rule, so the
    /// `Usage` conversion cannot truncate).
    pub fn totals(&self) -> UsageTotals {
        UsageTotals {
            input_tokens: self.input.load(Ordering::Relaxed),
            output_tokens: self.output.load(Ordering::Relaxed),
            calls: self.calls.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time snapshot of accumulated usage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub calls: u64,
}

impl UsageTotals {
    /// The [`Usage`] view (tokens only) for callers that aggregate per-model.
    pub fn usage(&self) -> Usage {
        Usage {
            input_tokens: u32::try_from(self.input_tokens).unwrap_or(u32::MAX),
            output_tokens: u32::try_from(self.output_tokens).unwrap_or(u32::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn res_6_usage_accumulates_across_calls() {
        let acc = UsageAccumulator::new();
        acc.record(Usage {
            input_tokens: 12,
            output_tokens: 5,
        })
        .unwrap();
        acc.record(Usage {
            input_tokens: 9,
            output_tokens: 2,
        })
        .unwrap();
        let totals = acc.totals();
        assert_eq!(totals.input_tokens, 21);
        assert_eq!(totals.output_tokens, 7);
        assert_eq!(totals.calls, 2);
        assert_eq!(totals.usage().input_tokens, 21);
    }

    #[test]
    fn usage_overflow_fails_closed() {
        let acc = UsageAccumulator::with_cap(10);
        acc.record(Usage {
            input_tokens: 8,
            output_tokens: 0,
        })
        .unwrap();
        let err = acc
            .record(Usage {
                input_tokens: 8,
                output_tokens: 0,
            })
            .unwrap_err();
        assert!(matches!(err, UsageError::Overflow(10)));
        // The rejected record was not folded in.
        assert_eq!(acc.totals().input_tokens, 8);
    }

    #[test]
    fn usage_clone_shares_counters() {
        let acc = UsageAccumulator::new();
        let shared = acc.clone();
        acc.record(Usage {
            input_tokens: 1,
            output_tokens: 1,
        })
        .unwrap();
        assert_eq!(shared.totals().calls, 1);
    }
}
