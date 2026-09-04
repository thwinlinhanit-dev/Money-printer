//! Signal observation engine (research-lab hardening Phase 3, REL-8..REL-11).
//!
//! When a signal fires, persist an IMMUTABLE [`SignalObservation`]: which
//! signal (via its research identity), at what exact time, on what symbol and
//! venue, in which direction, with the feature snapshot that caused the fire,
//! and under what data-quality state. Observations are deterministic and
//! replayable (same inputs ⇒ same observation ids), and forward outcomes
//! (Phase 4) are attached AFTER the fact — never during signal generation.
//!
//! Pure: no I/O, no wall clock (PD-3). `created_at_ns` is injected; the Parquet
//! persistence lives in mp-storage (`observation_store.rs`).

use crate::data_quality::{DataQualityState, QualityTracker};
use crate::engine::FeatureUpdate;
use crate::signal_identity::SignalResearchIdentity;
use mp_core::{fnv1a_absorb, FNV1A_OFFSET, SymbolId, Venue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Trade direction of an observation (the signal's implied position).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Long,
    Short,
}

impl Direction {
    /// Signed multiplier for return math (Long +1, Short −1).
    pub fn sign(self) -> f64 {
        match self {
            Direction::Long => 1.0,
            Direction::Short => -1.0,
        }
    }
}

/// One forward outcome for one horizon, attached after the fact (Phase 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationOutcome {
    pub horizon_ns: i64,
    pub entry_price: f64,
    pub exit_price: f64,
    /// Direction-signed gross return (exit/entry − 1), pre-cost.
    pub gross_return: f64,
    /// `gross_return − round_trip_cost` (net of the assumed cost model).
    pub net_return: f64,
    /// Max favorable excursion (direction-signed best return within window).
    pub mfe: f64,
    /// Max adverse excursion (direction-signed worst return within window).
    pub mae: f64,
    /// `gross_return > 0` — the binary hit/miss definition (REL-13).
    pub hit: bool,
}

/// An immutable signal observation (REL-8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalObservation {
    /// Deterministic id: FNV-1a(identity fingerprint ‖ timestamp ‖ seq).
    pub observation_id: u64,
    /// The signal's canonical research identity at fire time (REL-4).
    pub identity: SignalResearchIdentity,
    /// Event time the signal fired (injected clock, ns).
    pub timestamp_ns: i64,
    pub symbol: SymbolId,
    pub venue: Venue,
    pub direction: Direction,
    /// Only the features that caused the fire (the triggering updates).
    pub feature_snapshot: BTreeMap<String, f64>,
    /// Data quality at fire time — non-`Healthy` observations never exist
    /// (the engine refuses to record them, REL-2/REL-9), but the state is
    /// stored so readers can audit the condition the fire satisfied.
    pub quality: DataQualityState,
    /// Wall-clock-independent creation time (injected).
    pub created_at_ns: i64,
    /// Forward outcomes, attached post-run (empty until Phase 4 backfill).
    pub outcomes: Vec<ObservationOutcome>,
}

/// Deterministic observation id: FNV-1a over (identity fingerprint, ts, seq).
/// `seq` is the per-engine monotonic fire counter, so two observations at the
/// same timestamp for the same signal are still distinct — and identical
/// replays produce identical ids (REL-10).
pub fn observation_id(identity_fingerprint: &str, ts_ns: i64, seq: u64) -> u64 {
    let mut h = FNV1A_OFFSET;
    h = fnv1a_absorb(h, identity_fingerprint.as_bytes());
    h = fnv1a_absorb(h, &ts_ns.to_le_bytes());
    h = fnv1a_absorb(h, &seq.to_le_bytes());
    h
}

/// The observation engine (REL-9): tracks per-symbol data quality and records
/// immutable observations for fires that pass the quality gate.
#[derive(Debug, Clone)]
pub struct ObservationEngine {
    identity: SignalResearchIdentity,
    quality: QualityTracker,
    created_at_ns: i64,
    seq: u64,
    observations: Vec<SignalObservation>,
    /// Fires refused by the quality gate (REL-2 observability — a blocked
    /// signal is never silent).
    blocked: u64,
}

impl ObservationEngine {
    /// `quality` decides which fires are recorded; `created_at_ns` is the
    /// injected creation time (a fixed value in replay — PD-3).
    pub fn new(
        identity: SignalResearchIdentity,
        quality: QualityTracker,
        created_at_ns: i64,
    ) -> Self {
        Self {
            identity,
            quality,
            created_at_ns,
            seq: 0,
            observations: Vec::new(),
            blocked: 0,
        }
    }

    /// Convenience constructor with the default hourly tracker (5 samples,
    /// 1h staleness).
    pub fn with_default_quality(identity: SignalResearchIdentity, created_at_ns: i64) -> Self {
        Self::new(identity, QualityTracker::default_hourly(), created_at_ns)
    }

    /// Feed a feature update for quality tracking (the engine records quality
    /// as of the last observed update).
    pub fn on_feature_update(&mut self, u: &FeatureUpdate) {
        self.quality.observe(u);
    }

    /// Current quality state for a symbol (observability).
    pub fn quality(&self, symbol: SymbolId, now_ns: i64) -> DataQualityState {
        self.quality.state(symbol, now_ns)
    }

    /// How many fires were refused by the quality gate (REL-2: blocked state
    /// is explicit, never silent).
    pub fn blocked_count(&self) -> u64 {
        self.blocked
    }

    /// Record a fire. REL-2/REL-9: a non-`Healthy` quality state BLOCKS the
    /// observation — insufficient history / stale / invalid data never become
    /// a recorded signal. Returns the observation when recorded, `None` when
    /// blocked (counted in [`Self::blocked_count`]).
    pub fn record(
        &mut self,
        timestamp_ns: i64,
        symbol: SymbolId,
        venue: Venue,
        direction: Direction,
        snapshot: BTreeMap<String, f64>,
    ) -> Option<SignalObservation> {
        let q = self.quality.state(symbol, timestamp_ns);
        if q.blocks() {
            self.blocked += 1;
            return None;
        }
        self.seq += 1;
        let obs = SignalObservation {
            observation_id: observation_id(&self.identity.fingerprint(), timestamp_ns, self.seq),
            identity: self.identity.clone(),
            timestamp_ns,
            symbol,
            venue,
            direction,
            feature_snapshot: snapshot,
            quality: q,
            created_at_ns: self.created_at_ns,
            outcomes: Vec::new(),
        };
        self.observations.push(obs.clone());
        Some(obs)
    }

    /// All recorded observations, in fire order (deterministic).
    pub fn observations(&self) -> &[SignalObservation] {
        &self.observations
    }

    /// Consume the recorded observations (for persistence at the binary edge).
    pub fn into_observations(self) -> Vec<SignalObservation> {
        self.observations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal_identity::SignalResearchIdentity;
    use mp_core::Venue;

    const BTC: SymbolId = SymbolId(1);
    const T0: i64 = 1_784_505_600_000_000_000; // deterministic fixed instant

    fn identity() -> SignalResearchIdentity {
        SignalResearchIdentity::new("whale_imb", 1, "params-1", "cost-1")
    }

    fn update(sym: SymbolId, name: &str, val: f64, ts: i64) -> FeatureUpdate {
        FeatureUpdate {
            symbol: sym,
            feature: SymbolId(9),
            name: name.into(),
            venue: Venue::Bybit,
            value: val,
            ts_ns: ts,
            ver: 1,
        }
    }

    #[test]
    fn rel_8_observation_is_immutable_and_complete() {
        let mut e = ObservationEngine::with_default_quality(identity(), T0);
        for i in 0..5 {
            e.on_feature_update(&update(BTC, "funding.rate", 0.0001 * i as f64, T0 + i));
        }
        let snap = BTreeMap::from([("funding.rate".into(), 0.0004)]);
        let obs = e
            .record(T0 + 5, BTC, Venue::Bybit, Direction::Long, snap.clone())
            .expect("healthy ⇒ recorded");
        assert_eq!(obs.symbol, BTC);
        assert_eq!(obs.venue, Venue::Bybit);
        assert_eq!(obs.direction, Direction::Long);
        assert_eq!(obs.feature_snapshot, snap);
        assert_eq!(obs.quality, DataQualityState::Healthy);
        assert_eq!(obs.identity, identity());
        assert!(obs.outcomes.is_empty(), "outcomes attach after the fact");
        assert_eq!(e.observations().len(), 1);
    }

    #[test]
    fn rel_9_insufficient_history_blocks_recording() {
        let mut e = ObservationEngine::with_default_quality(identity(), T0);
        // No feature updates at all ⇒ InsufficientHistory ⇒ the fire is blocked.
        assert!(e
            .record(T0, BTC, Venue::Bybit, Direction::Long, BTreeMap::new())
            .is_none());
        assert_eq!(e.blocked_count(), 1);
        assert!(e.observations().is_empty());
        // One update is still insufficient (min 5).
        e.on_feature_update(&update(BTC, "funding.rate", 0.0001, T0));
        assert!(e
            .record(T0 + 1, BTC, Venue::Bybit, Direction::Short, BTreeMap::new())
            .is_none());
        assert_eq!(e.blocked_count(), 2);
    }

    #[test]
    fn rel_10_observation_ids_are_deterministic() {
        let run = || {
            let mut e = ObservationEngine::with_default_quality(identity(), T0);
            for i in 0..5 {
                e.on_feature_update(&update(BTC, "funding.rate", 0.0001, T0 + i));
            }
            let mut ids = Vec::new();
            for i in 0..3 {
                if let Some(o) = e.record(
                    T0 + 10 + i,
                    BTC,
                    Venue::Bybit,
                    if i % 2 == 0 { Direction::Long } else { Direction::Short },
                    BTreeMap::from([("funding.rate".into(), 0.0001)]),
                ) {
                    ids.push(o.observation_id);
                }
            }
            ids
        };
        let (a, b) = (run(), run());
        assert_eq!(a, b, "identical replays ⇒ identical observation ids");
        // Distinct (ts, seq) pairs ⇒ distinct ids even for the same signal.
        let mut sorted = a.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), a.len());
    }

    #[test]
    fn rel_11_replay_produces_identical_observation_bytes() {
        let run = || {
            let mut e = ObservationEngine::with_default_quality(identity(), T0);
            for i in 0..5 {
                e.on_feature_update(&update(BTC, "funding.rate", 0.0001, T0 + i));
            }
            e.record(
                T0 + 10,
                BTC,
                Venue::Bybit,
                Direction::Long,
                BTreeMap::from([("funding.rate".into(), 0.0001)]),
            );
            serde_json::to_string(e.observations()).unwrap()
        };
        assert_eq!(run(), run());
    }
}