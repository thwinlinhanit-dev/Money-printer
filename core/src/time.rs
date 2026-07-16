//! Injected clock (CONV-4, CONV-5). Decision-path code obtains time ONLY
//! through [`Clock`]; it must never call `SystemTime::now` directly. The sole
//! wall-clock reader is [`WallClock`], isolated in `wall_clock.rs` so the
//! guardrail can allow exactly that one file.

/// Nanoseconds since the Unix epoch, UTC (CONV-4). Always the `_ns` suffix.
pub type Nanos = i64;

/// Source of "now". Injected everywhere; [`SimClock`] in replay/backtest,
/// [`WallClock`](crate::WallClock) in live/paper.
pub trait Clock: Send + Sync {
    /// Current time in nanoseconds since the Unix epoch, UTC.
    fn now_ns(&self) -> Nanos;
}

/// Deterministic clock driven by event timestamps (CONV-5). The replay loop
/// advances it as it consumes events; decision code sees only event time.
#[derive(Debug, Default)]
pub struct SimClock {
    now_ns: std::sync::atomic::AtomicI64,
}

impl SimClock {
    /// Create a sim clock starting at `start_ns`.
    pub fn new(start_ns: Nanos) -> Self {
        Self {
            now_ns: std::sync::atomic::AtomicI64::new(start_ns),
        }
    }

    /// Advance the clock to `ts_ns`. Monotonic: never moves backwards
    /// (out-of-order input is a bug the replay loop must not create).
    pub fn set(&self, ts_ns: Nanos) {
        // Monotonic guard: store the max of current and new.
        let mut cur = self.now_ns.load(std::sync::atomic::Ordering::Relaxed);
        while ts_ns > cur {
            match self.now_ns.compare_exchange_weak(
                cur,
                ts_ns,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => cur = observed,
            }
        }
    }
}

impl Clock for SimClock {
    fn now_ns(&self) -> Nanos {
        self.now_ns.load(std::sync::atomic::Ordering::Relaxed)
    }
}
