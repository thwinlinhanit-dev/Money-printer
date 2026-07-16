//! SplitMix64 — the workspace's only seeded PRNG on decision paths (CONV-11).
//! Same seed ⇒ same sequence. No external RNG dependency.

/// Deterministic 64-bit PRNG.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Current internal state (for save/restore across strategy callbacks).
    pub fn state(&self) -> u64 {
        self.state
    }

    /// Restore from a previously observed state.
    pub fn from_state(state: u64) -> Self {
        Self { state }
    }

    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform value in `[0, n)` (`n > 0`). Slight modulo bias is fine for
    /// backoff jitter and Monte-Carlo block picks.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        self.next_u64() % n
    }
}
