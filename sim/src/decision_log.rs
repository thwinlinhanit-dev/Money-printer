//! Deterministic decision log (SIM-7). Every intent and fill is recorded with a
//! stable rolling hash; two runs over identical inputs MUST produce identical
//! hashes (CONV-12). This turns the subtlest bug class — nondeterminism — into a
//! red/green signal.

use mp_core::{Fill, OrderIntent};

/// Append-only record of decisions with a rolling FNV-1a hash.
#[derive(Debug, Clone, Default)]
pub struct DecisionLog {
    hash: u64,
    intents: u64,
    fills: u64,
    lines: Vec<String>,
}

impl DecisionLog {
    pub fn new() -> Self {
        Self {
            hash: 0xcbf2_9ce4_8422_2325,
            ..Default::default()
        }
    }

    fn absorb(&mut self, s: &str) {
        for b in s.as_bytes() {
            self.hash ^= *b as u64;
            self.hash = self.hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.lines.push(s.to_string());
    }

    pub fn record_intent(&mut self, seq: u64, i: &OrderIntent) {
        self.intents += 1;
        // Canonical, float-stable rendering (bit patterns avoid formatting drift).
        let s = format!(
            "I|{seq}|{}|{:?}|{}|{:?}|{:?}|{:?}|{}",
            i.strategy.0, i.side, i.symbol.0, i.kind, i.qty, i.tif, i.reduce_only,
        );
        self.absorb(&s);
    }

    pub fn record_fill(&mut self, seq: u64, f: &Fill) {
        self.record_fill_tagged(seq, f, false);
    }

    /// Record a fill, tagging whether it is an `optimistic_maker` fill (SIM-12:
    /// a resting-limit fill our L1/L2 model assumed but didn't queue-model).
    pub fn record_fill_tagged(&mut self, seq: u64, f: &Fill, optimistic_maker: bool) {
        self.fills += 1;
        let s = format!(
            "F|{seq}|{}|{:?}|{}|{}|{}|{:?}|{}",
            f.symbol.0,
            f.side,
            f.price.to_bits(),
            f.qty.to_bits(),
            f.fee.to_bits(),
            f.liquidity,
            optimistic_maker,
        );
        self.absorb(&s);
    }

    /// Stable hash of the whole decision sequence.
    pub fn hash(&self) -> u64 {
        self.hash
    }

    pub fn intent_count(&self) -> u64 {
        self.intents
    }

    pub fn fill_count(&self) -> u64 {
        self.fills
    }

    /// The canonical decision lines, in order.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// First index at which two logs diverge, or `None` if one is a prefix of
    /// the other and they share every common line (SIM-11 `replay-live`: any
    /// divergence between a live session and its deterministic replay is a P1
    /// bug). A length mismatch with identical prefix still diverges — at the
    /// index where the shorter log ends.
    pub fn first_divergence(&self, other: &DecisionLog) -> Option<usize> {
        let n = self.lines.len().min(other.lines.len());
        for i in 0..n {
            if self.lines[i] != other.lines[i] {
                return Some(i);
            }
        }
        if self.lines.len() != other.lines.len() {
            Some(n)
        } else {
            None
        }
    }
}
