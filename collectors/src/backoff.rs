//! Jittered exponential reconnect backoff (COL-1). Deterministic given a seed
//! (CONV-11) so reconnect behavior is testable and reproducible.

use crate::rng::SplitMix64;

/// Full-jitter exponential backoff: delay ∈ `[0, min(cap, base·2^attempt)]`.
#[derive(Debug, Clone)]
pub struct Backoff {
    base_ms: u64,
    cap_ms: u64,
    attempt: u32,
    rng: SplitMix64,
}

impl Backoff {
    /// `base_ms` starting ceiling, `cap_ms` maximum, `seed` for jitter.
    pub fn new(base_ms: u64, cap_ms: u64, seed: u64) -> Self {
        Self {
            base_ms: base_ms.max(1),
            cap_ms: cap_ms.max(1),
            attempt: 0,
            rng: SplitMix64::new(seed),
        }
    }

    /// Ceiling for the current attempt, `base·2^attempt` clamped to `cap`.
    fn ceiling_ms(&self) -> u64 {
        let shifted = self.base_ms.checked_shl(self.attempt).unwrap_or(u64::MAX);
        shifted.min(self.cap_ms)
    }

    /// Delay for the next reconnect attempt, advancing the attempt counter.
    pub fn next_delay_ms(&mut self) -> u64 {
        let ceil = self.ceiling_ms();
        let delay = self.rng.below(ceil + 1); // inclusive of ceil
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    /// Reset after a successful, stable connection.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Current attempt count (0 before the first failure).
    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}
