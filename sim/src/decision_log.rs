//! Deterministic decision log (SIM-7). Every intent and fill is recorded with a
//! stable rolling hash; two runs over identical inputs MUST produce identical
//! hashes (CONV-12).

use crate::fills::FillOptimism;
use mp_core::{fnv1a_absorb, Fill, OrderIntent, FNV1A_OFFSET};

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
            hash: FNV1A_OFFSET,
            ..Default::default()
        }
    }

    fn absorb(&mut self, s: &str) {
        self.hash = fnv1a_absorb(self.hash, s.as_bytes());
        self.lines.push(s.to_string());
    }

    pub fn record_intent(&mut self, seq: u64, i: &OrderIntent) {
        self.intents += 1;
        let s = format!(
            "I|{seq}|{}|{:?}|{}|{:?}|{:?}|{:?}|{}",
            i.strategy.0, i.side, i.symbol.0, i.kind, i.qty, i.tif, i.reduce_only,
        );
        self.absorb(&s);
    }

    pub fn record_fill(&mut self, seq: u64, f: &Fill) {
        self.record_fill_tagged(seq, f, FillOptimism::None);
    }

    /// Record a fill with its optimism tag (`none` / `maker` / `tape`).
    pub fn record_fill_tagged(&mut self, seq: u64, f: &Fill, optimism: FillOptimism) {
        self.fills += 1;
        let s = format!(
            "F|{seq}|{}|{:?}|{}|{}|{}|{:?}|{}",
            f.symbol.0,
            f.side,
            f.price.to_bits(),
            f.qty.to_bits(),
            f.fee.to_bits(),
            f.liquidity,
            optimism.as_str(),
        );
        self.absorb(&s);
    }

    pub fn record_feature(&mut self, seq: u64, u: &mp_features::FeatureUpdate) {
        let s = format!(
            "U|{seq}|{}|{:?}|{}|{}|{}|{}",
            u.feature,
            u.venue,
            u.symbol.0,
            u.ts_ns,
            u.value.to_bits(),
            u.ver,
        );
        self.absorb(&s);
    }

    pub fn record_verdict(&mut self, seq: u64, intent: mp_core::IntentId, v: mp_risk::Verdict) {
        let s = format!("V|{seq}|{}|{v:?}", intent.0);
        self.absorb(&s);
    }

    pub fn hash(&self) -> u64 {
        self.hash
    }

    pub fn intent_count(&self) -> u64 {
        self.intents
    }

    pub fn fill_count(&self) -> u64 {
        self.fills
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

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
