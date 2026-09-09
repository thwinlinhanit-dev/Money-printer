//! Data quality state (research-lab hardening Phase 1, REL-1..REL-3).
//!
//! A first-class, explicit answer to "is this feature/symbol's data good
//! enough to fire a signal on?" — instead of the legacy implicit answers
//! (non-finite values silently suppressed by FEA-5; missing history silently
//! becoming a neutral 0.5 percentile).
//!
//! Rules (spec 054, Phase 7 extension 2026-09-07):
//! - non-finite values ⇒ [`DataQualityState::Invalid`] (rejected, counted);
//! - a symbol/stream NEVER observed ⇒ [`DataQualityState::Missing`] —
//!   distinct from having *some* data (REL-27: "unknown" is its own state,
//!   never folded into a neutral answer);
//! - fewer than `min_samples` observed ⇒
//!   [`DataQualityState::InsufficientHistory`] — this must BLOCK signal
//!   firing, never be neutralized into 0/0.5/zero;
//! - a HOLE larger than the staleness window between consecutive
//!   observations ⇒ [`DataQualityState::Gap`] — a real data gap is flagged
//!   explicitly (never silently healed by the stream's resumption) and
//!   blocks until `min_samples` fresh observations arrive after the
//!   resumption sample;
//! - data older than the staleness window ⇒ [`DataQualityState::Stale`];
//! - otherwise [`DataQualityState::Healthy`].
//!
//! Every non-`Healthy` state blocks firing ([`DataQualityState::blocks`]);
//! the declaration order matches the Parquet storage codes (0..5), severity
//! is expressed by `blocks`, not by ordering.
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
    /// Fewer than `min_samples` observations seen (but at least one) —
    /// firing is BLOCKED, and the deficiency is explicit, never neutralized
    /// to a 0/0.5 fallback.
    InsufficientHistory,
    /// Last observation is older than the staleness window — firing blocked.
    Stale,
    /// A non-finite value was observed — the stream is rejected (REL-1).
    Invalid,
    /// The symbol/stream has NEVER been observed (REL-27) — "unknown" is its
    /// own state, distinct from partial history, and always blocks.
    Missing,
    /// A hole larger than the staleness window was detected between
    /// consecutive observations (REL-27) — blocks until `min_samples` fresh
    /// observations arrive after the resumption sample.
    Gap,
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
            DataQualityState::Missing => "never observed — no data has ever arrived",
            DataQualityState::Gap => "data gap detected — hole larger than the staleness window",
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
    /// Time of the resumption sample that followed a detected gap (REL-27).
    last_gap_ts_ns: BTreeMap<SymbolId, i64>,
    /// Observations since the resumption sample; the Gap state clears once
    /// this reaches `min_samples`.
    samples_since_gap: BTreeMap<SymbolId, u64>,
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
            last_gap_ts_ns: BTreeMap::new(),
            samples_since_gap: BTreeMap::new(),
        }
    }

    /// Default tracker: 5 samples minimum, 1h staleness window.
    pub fn default_hourly() -> Self {
        Self::new(5, 3_600_000_000_000)
    }

    /// Feed one feature update. Non-finite values are counted and mark the
    /// symbol `Invalid` (REL-1: reject, never pass through). Finite values
    /// count toward history; a hole larger than the staleness window between
    /// consecutive observations triggers the `Gap` quarantine (REL-27).
    /// Returns the state after this observation.
    pub fn observe(&mut self, u: &FeatureUpdate) -> DataQualityState {
        if !u.value.is_finite() {
            *self.invalid_counts.entry(u.symbol).or_insert(0) += 1;
            return DataQualityState::Invalid;
        }
        if let Some(&last) = self.last_seen_ns.get(&u.symbol) {
            if u.ts_ns.saturating_sub(last) > self.stale_after_ns {
                // REL-27: the stream resumed after a real hole — flag it.
                // The resumption sample itself does not count toward healing.
                self.last_gap_ts_ns.insert(u.symbol, u.ts_ns);
                self.samples_since_gap.insert(u.symbol, 0);
            } else if self.last_gap_ts_ns.contains_key(&u.symbol) {
                *self.samples_since_gap.entry(u.symbol).or_insert(0) += 1;
            }
        }
        *self.samples.entry(u.symbol).or_insert(0) += 1;
        self.last_seen_ns.insert(u.symbol, u.ts_ns);
        self.state(u.symbol, u.ts_ns)
    }

    /// Current state for a symbol at `now_ns` (REL-2: every non-`Healthy`
    /// state blocks; invalid is terminal for the stream; a gap blocks until
    /// enough fresh post-resumption samples arrive).
    pub fn state(&self, symbol: SymbolId, now_ns: i64) -> DataQualityState {
        if self.invalid_counts.get(&symbol).copied().unwrap_or(0) > 0 {
            return DataQualityState::Invalid;
        }
        let n = self.samples.get(&symbol).copied().unwrap_or(0);
        if n == 0 {
            return DataQualityState::Missing;
        }
        if n < self.min_samples {
            return DataQualityState::InsufficientHistory;
        }
        if let Some(&since) = self.samples_since_gap.get(&symbol) {
            if since < self.min_samples {
                return DataQualityState::Gap;
            }
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
        // REL-27: zero samples ⇒ Missing (never observed), not neutralized.
        assert_eq!(t.state(BTC, 0), DataQualityState::Missing);
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
        // A fresh observation heals Stale — but the >window hole it closed is
        // a real GAP (REL-27): the resumption sample flags it, and it blocks
        // until enough post-resumption samples arrive.
        t.observe(&update(BTC, 1.0, 3_600_000_000_001));
        assert_eq!(t.state(BTC, 3_600_000_000_001), DataQualityState::Gap);
        // min_samples = 1 here, so the very next sample heals the gap.
        t.observe(&update(BTC, 1.0, 3_600_000_000_002));
        assert_eq!(t.state(BTC, 3_600_000_000_002), DataQualityState::Healthy);
    }

    #[test]
    fn rel_27_missing_is_never_observed_distinct_from_insufficient() {
        let mut t = QualityTracker::new(5, 3_600_000_000_000);
        assert_eq!(t.state(BTC, 0), DataQualityState::Missing);
        assert!(DataQualityState::Missing.blocks());
        assert!(DataQualityState::Missing.reason().contains("never"));
        // One sample flips Missing → InsufficientHistory (some, but too few).
        t.observe(&update(BTC, 1.0, 0));
        assert_eq!(t.state(BTC, 0), DataQualityState::InsufficientHistory);
    }

    #[test]
    fn rel_27_gap_detected_blocks_and_heals_after_min_samples() {
        let mut t = QualityTracker::new(2, 3_600_000_000_000);
        t.observe(&update(BTC, 1.0, 0));
        t.observe(&update(BTC, 1.0, 1));
        assert_eq!(t.state(BTC, 1), DataQualityState::Healthy);
        // A 2h hole through a 1h staleness window: the resumption sample
        // itself triggers the Gap quarantine.
        t.observe(&update(BTC, 1.0, 7_200_000_000_000));
        assert_eq!(t.state(BTC, 7_200_000_000_000), DataQualityState::Gap);
        assert!(DataQualityState::Gap.blocks());
        assert!(DataQualityState::Gap.reason().contains("gap"));
        // 1st post-resumption sample: still quarantined (1 of 2).
        t.observe(&update(BTC, 1.0, 7_200_000_000_001));
        assert_eq!(t.state(BTC, 7_200_000_000_001), DataQualityState::Gap);
        // 2nd post-resumption sample: healed — the hole stays on the record
        // via the tracker's gap bookkeeping, never silently erased.
        t.observe(&update(BTC, 1.0, 7_200_000_000_002));
        assert_eq!(t.state(BTC, 7_200_000_000_002), DataQualityState::Healthy);
        // Invalid still dominates everything (terminal poison).
        t.observe(&update(BTC, f64::NAN, 7_200_000_000_003));
        assert_eq!(t.state(BTC, 7_200_000_000_003), DataQualityState::Invalid);
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