//! Data quality state (research-lab hardening Phase 1, REL-1..REL-3).
//!
//! A first-class, explicit answer to "is this feature/symbol's data good
//! enough to fire a signal on?" — instead of the legacy implicit answers
//! (non-finite values silently suppressed by FEA-5; missing history silently
//! becoming a neutral 0.5 percentile).
//!
//! Rules (spec 054):
//! - non-finite values ⇒ [`DataQualityState::Invalid`] (rejected, counted);
//! - insufficient history (fewer than `min_samples` observed) ⇒
//!   [`DataQualityState::InsufficientHistory`] — this must BLOCK signal
//!   firing, never be neutralized into 0/0.5/zero;
//! - data older than the staleness window ⇒ [`DataQualityState::Stale`];
//! - otherwise [`DataQualityState::Healthy`].
//!
//! Pure: no I/O, no wall clock (PD-3). `now_ns` is a parameter wherever a
//! staleness decision is made; replay passes the injected clock.

use crate::engine::FeatureUpdate;
use mp_core::SymbolId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Explicit data-quality state for a (symbol, feature stream) pair.
///
/// Ordering matters: `Invalid` is the worst, `Healthy` the best. Any
/// non-`Healthy` state blocks signal firing by construction (REL-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum DataQualityState {
    /// Enough fresh, finite observations — signal firing is permitted.
    #[default]
    Healthy,
    /// Fewer than `min_samples` observations seen — firing is BLOCKED, and
    /// the deficiency is explicit, never neutralized to a 0/0.5 fallback.
    InsufficientHistory,
    /// Last observation is older than the staleness window — firing blocked.
    Stale,
    /// A non-finite value was observed — the stream is rejected (REL-1).
    Invalid,
}

impl DataQualityState {
    pub fn reason(self) -> &'static str {
        match self {
            DataQualityState::Healthy => "sufficient fresh finite observations",
            DataQualityState::InsufficientHistory => {
                "insufficient history — fewer than the required minimum samples"
            }
            DataQualityState::Stale => "last observation older than the staleness window",
            DataQualityState::Invalid => "non-finite value observed — stream rejected",
        }
    }

    /// Whether this state blocks signal firing (REL-2: insufficient history,
    /// stale, and invalid all block; only Healthy permits).
    pub fn blocks(self) -> bool {
        self != DataQualityState::Healthy
    }
}

/// Per-symbol quality tracker (pure, replayable).
///
/// Feed every `FeatureUpdate` through [`QualityTracker::observe`]; query the
/// resulting state with [`QualityTracker::state`] at the moment a signal
/// would fire. The tracker is the observation engine's quality source, and it
/// is what makes "insufficient history" EXPLICIT at fire time instead of a
/// silent neutral fallback.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityTracker {
    /// Minimum observed samples per symbol before `Healthy`.
    min_samples: u64,
    /// A symbol with no observation fresher than this (ns) is `Stale`.
    stale_after_ns: i64,
    /// Observed sample counts per symbol.
    samples: BTreeMap<SymbolId, u64>,
    /// Latest observation time per symbol.
    last_seen_ns: BTreeMap<SymbolId, i64>,
    /// Non-finite observations per symbol (REL-1 counter — never silent).
    invalid_counts: BTreeMap<SymbolId, u64>,
}

impl QualityTracker {
    /// `min_samples` must be ≥ 1 for `Healthy` to ever be reachable;
    /// `stale_after_ns` must be > 0.
    pub fn new(min_samples: u64, stale_after_ns: i64) -> Self {
        Self {
            min_samples: min_samples.max(1),
            stale_after_ns: stale_after_ns.max(1),
            samples: BTreeMap::new(),
            last_seen_ns: BTreeMap::new(),
            invalid_counts: BTreeMap::new(),
        }
    }

    /// Default tracker: 5 samples minimum, 1h staleness window.
    pub fn default_hourly() -> Self {
        Self::new(5, 3_600_000_000_000)
    }

    /// Feed one feature update. Non-finite values are counted and mark the
    /// symbol `Invalid` (REL-1: reject, never pass through). Finite values
    /// count toward history. Returns the state after this observation.
    pub fn observe(&mut self, u: &FeatureUpdate) -> DataQualityState {
        if !u.value.is_finite() {
            *self.invalid_counts.entry(u.symbol).or_insert(0) += 1;
            return DataQualityState::Invalid;
        }
        *self.samples.entry(u.symbol).or_insert(0) += 1;
        self.last_seen_ns.insert(u.symbol, u.ts_ns);
        self.state(u.symbol, u.ts_ns)
    }

    /// Current state for a symbol at `now_ns` (REL-2: insufficient history
    /// and stale both block; invalid is terminal for the stream).
    pub fn state(&self, symbol: SymbolId, now_ns: i64) -> DataQualityState {
        if self.invalid_counts.get(&symbol).copied().unwrap_or(0) > 0 {
            return DataQualityState::Invalid;
        }
        let n = self.samples.get(&symbol).copied().unwrap_or(0);
        if n < self.min_samples {
            return DataQualityState::InsufficientHistory;
        }
        let last = self.last_seen_ns.get(&symbol).copied().unwrap_or(0);
        if now_ns.saturating_sub(last) > self.stale_after_ns {
            return DataQualityState::Stale;
        }
        DataQualityState::Healthy
    }

    /// Observed sample count for a symbol.
    pub fn samples(&self, symbol: SymbolId) -> u64 {
        self.samples.get(&symbol).copied().unwrap_or(0)
    }

    /// Non-finite observation count for a symbol (never silently dropped).
    pub fn invalid_count(&self, symbol: SymbolId) -> u64 {
        self.invalid_counts.get(&symbol).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{SymbolId, Venue};

    const BTC: SymbolId = SymbolId(1);

    fn update(sym: SymbolId, val: f64, ts_ns: i64) -> FeatureUpdate {
        FeatureUpdate {
            symbol: sym,
            feature: sym,
            name: "test.feat".into(),
            venue: Venue::Bybit,
            value: val,
            ts_ns,
            ver: 1,
        }
    }

    #[test]
    fn rel_1_non_finite_is_invalid_and_counts() {
        let mut t = QualityTracker::new(5, 3_600_000_000_000);
        for i in 0..4 {
            assert_eq!(t.observe(&update(BTC, 1.0, i)), DataQualityState::InsufficientHistory);
        }
        // The 5th sample reaches the minimum: Healthy.
        assert_eq!(t.observe(&update(BTC, 1.0, 4)), DataQualityState::Healthy);
        // A NaN poisons the stream: Invalid, counted, and it blocks forever.
        assert_eq!(t.observe(&update(BTC, f64::NAN, 5)), DataQualityState::Invalid);
        assert_eq!(t.state(BTC, 6), DataQualityState::Invalid);
        assert_eq!(t.invalid_count(BTC), 1);
        assert!(DataQualityState::Invalid.blocks());
    }

    #[test]
    fn rel_2_insufficient_history_blocks_and_is_explicit() {
        let mut t = QualityTracker::new(5, 3_600_000_000_000);
        assert_eq!(t.state(BTC, 0), DataQualityState::InsufficientHistory);
        // 3 of 5 samples: still insufficient — and never "0.5/neutral".
        for i in 0..3 {
            t.observe(&update(BTC, 1.0, i));
        }
        let s = t.state(BTC, 3);
        assert_eq!(s, DataQualityState::InsufficientHistory);
        assert!(s.blocks());
        // 5 samples: Healthy.
        for i in 3..5 {
            t.observe(&update(BTC, 1.0, i));
        }
        assert_eq!(t.state(BTC, 5), DataQualityState::Healthy);
        assert!(!DataQualityState::Healthy.blocks());
    }

    #[test]
    fn rel_3_stale_blocks_after_window() {
        let mut t = QualityTracker::new(1, 3_600_000_000_000);
        t.observe(&update(BTC, 1.0, 0));
        assert_eq!(t.state(BTC, 3_599_999_999_999), DataQualityState::Healthy);
        // Age == window is still fresh; older than the window ⇒ stale.
        assert_eq!(t.state(BTC, 3_600_000_000_000), DataQualityState::Healthy);
        assert_eq!(t.state(BTC, 3_600_000_000_001), DataQualityState::Stale);
        assert!(DataQualityState::Stale.blocks());
        // A fresh observation heals it (unless the stream was Invalid).
        t.observe(&update(BTC, 1.0, 3_600_000_000_001));
        assert_eq!(t.state(BTC, 3_600_000_000_001), DataQualityState::Healthy);
    }

    #[test]
    fn rel_4_tracker_is_replayable_and_deterministic() {
        let feed = |vals: &[f64]| {
            let mut t = QualityTracker::new(2, 3_600_000_000_000);
            let mut out = Vec::new();
            for (i, v) in vals.iter().enumerate() {
                out.push(t.observe(&update(BTC, *v, i as i64)));
            }
            (out, t.state(BTC, vals.len() as i64))
        };
        let (a, sa) = feed(&[1.0, 2.0, 3.0]);
        let (b, sb) = feed(&[1.0, 2.0, 3.0]);
        assert_eq!(a, b);
        assert_eq!(sa, sb);
        assert_eq!(sa, DataQualityState::Healthy);
    }
}