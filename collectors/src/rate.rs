//! Client-side rate budget (COL-4). A token bucket over injected time
//! (no wall clock here — caller passes `now_ns`). Venues punish REST snapshot
//! bursts hardest, so the budget lives per venue in the adapter.

use mp_core::Nanos;

/// Token bucket. `capacity` tokens, refilled `refill_per_sec` continuously.
#[derive(Debug, Clone)]
pub struct RateBudget {
    capacity: f64,
    tokens: f64,
    refill_per_ns: f64,
    last_ns: Nanos,
}

impl RateBudget {
    pub fn new(capacity: f64, refill_per_sec: f64, now_ns: Nanos) -> Self {
        Self {
            capacity: capacity.max(0.0),
            tokens: capacity.max(0.0),
            refill_per_ns: refill_per_sec.max(0.0) / 1_000_000_000.0,
            last_ns: now_ns,
        }
    }

    fn refill(&mut self, now_ns: Nanos) {
        if now_ns > self.last_ns {
            let elapsed = (now_ns - self.last_ns) as f64;
            self.tokens = (self.tokens + elapsed * self.refill_per_ns).min(self.capacity);
            self.last_ns = now_ns;
        }
    }

    /// Try to spend `n` tokens at `now_ns`. Returns `true` if allowed.
    pub fn try_take(&mut self, now_ns: Nanos, n: f64) -> bool {
        self.refill(now_ns);
        if self.tokens + 1e-9 >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    /// Current available tokens (after refilling to `now_ns`).
    pub fn available(&mut self, now_ns: Nanos) -> f64 {
        self.refill(now_ns);
        self.tokens
    }
}
