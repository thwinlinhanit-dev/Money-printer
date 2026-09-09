//! Backtester observation recorder (research-lab hardening Phase 3/4,
//! REL-8..REL-14). Write-only side state: records immutable
//! [`SignalObservation`]s when strategies emit intents and attaches forward
//! outcomes post-run from the recorded mark series. It NEVER feeds back into
//! dispatch, so the decision-log hash is byte-identical with or without
//! recording (the golden determinism tests prove this).
//!
//! Deterministic by construction: engines are created up front (one per
//! strategy id, in registration order), every feature update is fed to every
//! engine in stream order, and observation ids derive from the identity
//! fingerprint + timestamp + per-engine sequence.

use mp_core::{OrderIntent, Side, SymbolId};
use mp_features::observation::{Direction, ObservationEngine, SignalObservation};
use mp_features::outcome::attach_outcomes;
use mp_features::signal_identity::SignalResearchIdentity;
use mp_features::{DEFAULT_REGIME_FEATURE, FeatureUpdate, QualityTracker};
use std::collections::BTreeMap;

/// The recorder owned by the backtester while observation recording is on.
/// The identity dimensions are consumed at construction (each strategy's
/// engine is built with its own identity) and not stored beyond that.
#[derive(Debug, Clone)]
pub struct ObservationRecorder {
    /// Round-trip cost (fraction) for net returns (default taker+maker fee).
    round_trip_cost: f64,
    /// One engine per strategy id (created up front, deterministic order).
    engines: BTreeMap<String, ObservationEngine>,
    /// Recorded observations in fire order.
    observations: Vec<SignalObservation>,
    /// Per-symbol mark series (t, price) — the post-run outcome source.
    mark_history: BTreeMap<SymbolId, Vec<(i64, f64)>>,
    /// Last-seen regime value per symbol (REL-25/R-6 fire context), captured
    /// write-only from the feature stream. Holds only FINITE values actually
    /// observed from `regime.trend` updates — a symbol the feature never
    /// reached stays absent, and its fires carry NO regime tag (never
    /// fabricated, R-1). Read at fire time to enrich the snapshot.
    regime_last: BTreeMap<SymbolId, f64>,
}

impl ObservationRecorder {
    pub fn new(
        strategy_ids: &[String],
        params_hash: String,
        feature_version: u16,
        data_schema_version: u16,
        cost_model_hash: String,
        created_at_ns: i64,
        round_trip_cost: f64,
    ) -> Self {
        let mut engines = BTreeMap::new();
        for sid in strategy_ids {
            let identity = SignalResearchIdentity {
                signal_id: sid.clone(),
                feature_version,
                data_schema_version,
                params_hash: params_hash.clone(),
                cost_model_hash: cost_model_hash.clone(),
            };
            engines.insert(
                sid.clone(),
                ObservationEngine::new(identity, QualityTracker::default_hourly(), created_at_ns),
            );
        }
        Self {
            round_trip_cost,
            engines,
            observations: Vec::new(),
            mark_history: BTreeMap::new(),
            regime_last: BTreeMap::new(),
        }
    }

    /// Record a mark sample (the latest trade/mark/mid price at `ts`).
    pub fn record_mark(&mut self, symbol: SymbolId, price: f64, ts_ns: i64) {
        if price.is_finite() && price > 0.0 {
            self.mark_history.entry(symbol).or_default().push((ts_ns, price));
        }
    }

    /// Feed a feature update to every engine (identical stream to every
    /// strategy's quality tracker — determinism by construction). Also
    /// captures the last-seen `regime.trend` value per symbol (REL-25 fire
    /// context) — pure side-state, never fed back into dispatch.
    pub fn on_feature_update(&mut self, u: &FeatureUpdate) {
        if u.name == DEFAULT_REGIME_FEATURE && u.value.is_finite() {
            self.regime_last.insert(u.symbol, u.value);
        }
        for e in self.engines.values_mut() {
            e.on_feature_update(u);
        }
    }

    /// Record one intent as an observation (direction from the order side,
    /// snapshot = the feature update(s) that caused the fire, ENRICHED with
    /// the symbol's last-seen regime value when one exists — REL-25: the
    /// regime gate needs fire-time context). Fires blocked
    /// by data quality are counted, never recorded (REL-9).
    pub fn record_intent(&mut self, intent: &OrderIntent, now_ns: i64, snapshot: &BTreeMap<String, f64>) {
        let Some(engine) = self.engines.get_mut(&intent.strategy.0) else {
            return;
        };
        let direction = match intent.side {
            Side::Buy => Direction::Long,
            Side::Sell => Direction::Short,
        };
        let mut snap = snapshot.clone();
        if let Some(&r) = self.regime_last.get(&intent.symbol) {
            // The trigger feature wins if it IS the regime feature.
            snap.entry(DEFAULT_REGIME_FEATURE.to_string()).or_insert(r);
        }
        if let Some(obs) = engine.record(now_ns, intent.symbol, intent.venue, direction, snap)
        {
            self.observations.push(obs);
        }
    }

    /// Fires refused by the quality gate across all strategies.
    pub fn blocked_count(&self) -> u64 {
        self.engines.values().map(ObservationEngine::blocked_count).sum()
    }

    /// Recorded observations (fire order).
    pub fn observations(&self) -> &[SignalObservation] {
        &self.observations
    }

    /// Attach forward outcomes post-run (Phase 4): every observation gets
    /// outcomes for each horizon whose window closed within the recorded mark
    /// series of its symbol. Pure with respect to the run — only the recorded
    /// marks matter, so no lookahead is possible (REL-14).
    pub fn attach_outcomes(&mut self, horizons: &[i64]) {
        let mut updated = Vec::with_capacity(self.observations.len());
        for obs in self.observations.iter() {
            let marks = self.mark_history.get(&obs.symbol).cloned().unwrap_or_default();
            let attached = attach_outcomes(std::slice::from_ref(obs), &marks, horizons, self.round_trip_cost);
            updated.push(attached.into_iter().next().unwrap_or_else(|| obs.clone()));
        }
        self.observations = updated;
    }

    /// Consume the recorder for persistence at the binary edge.
    pub fn into_observations(self) -> Vec<SignalObservation> {
        self.observations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{IntentId, OrderKind, StrategyId, TimeInForce, Venue};
    use mp_features::engine::FeatureUpdate;

    const T0: i64 = 1_784_505_600_000_000_000;

    fn intent(strategy: &str, side: Side) -> OrderIntent {
        OrderIntent {
            intent_id: IntentId(1),
            strategy: StrategyId::new(strategy),
            venue: Venue::Bybit,
            symbol: SymbolId(1),
            side,
            kind: OrderKind::Market,
            qty: mp_core::SizeUnit::Contracts(1.0),
            tif: TimeInForce::Ioc,
            reduce_only: false,
            tag: "obs".into(),
        }
    }

    fn feat(sym: SymbolId, val: f64, ts: i64) -> FeatureUpdate {
        FeatureUpdate {
            symbol: sym,
            feature: SymbolId(9),
            name: "funding.rate".into(),
            venue: Venue::Bybit,
            value: val,
            ts_ns: ts,
            ver: 1,
        }
    }

    #[test]
    fn obs_recorder_records_and_blocks_deterministically() {
        let mut r = ObservationRecorder::new(
            &["carry-v1".into()],
            "params".into(),
            1,
            6,
            "cost".into(),
            T0,
            0.0,
        );
        // Fewer than 5 updates ⇒ InsufficientHistory ⇒ fires blocked.
        for i in 0..3 {
            r.on_feature_update(&feat(SymbolId(1), 1.0, T0 + i));
        }
        r.record_intent(&intent("carry-v1", Side::Buy), T0 + 3, &BTreeMap::new());
        assert!(r.observations().is_empty());
        assert_eq!(r.blocked_count(), 1);
        // 5+ updates ⇒ Healthy ⇒ recorded.
        for i in 3..5 {
            r.on_feature_update(&feat(SymbolId(1), 1.0, T0 + i));
        }
        r.record_intent(&intent("carry-v1", Side::Buy), T0 + 5, &BTreeMap::new());
        assert_eq!(r.observations().len(), 1);
        assert_eq!(r.observations()[0].direction, Direction::Long);
        assert_eq!(r.observations()[0].identity.signal_id, "carry-v1");
    }

    #[test]
    fn obs_recorder_attaches_outcomes_post_run() {
        let mut r = ObservationRecorder::new(
            &["carry-v1".into()],
            "params".into(),
            1,
            6,
            "cost".into(),
            T0,
            0.0,
        );
        // Marks every 1s from T0 (100..109); the fire at T0+5s.
        for i in 0..10 {
            let t = T0 + i * 1_000_000_000;
            r.on_feature_update(&feat(SymbolId(1), 1.0, t));
            r.record_mark(SymbolId(1), 100.0 + i as f64, t);
        }
        r.record_intent(&intent("carry-v1", Side::Buy), T0 + 5 * 1_000_000_000, &BTreeMap::new());
        assert!(r.observations()[0].outcomes.is_empty());
        // Post-run attach: entry = last mark ≤ T0+5s = 105; 1s horizon exit =
        // last mark ≤ T0+6s = 106 ⇒ gross = +1/105 ≈ +0.00952.
        r.attach_outcomes(&[1_000_000_000]);
        let o = &r.observations()[0];
        assert_eq!(o.outcomes.len(), 1);
        assert_eq!(o.outcomes[0].entry_price, 105.0);
        assert_eq!(o.outcomes[0].exit_price, 106.0);
        assert!((o.outcomes[0].gross_return - (1.0 / 105.0)).abs() < 1e-12);
    }

    // REL-25/R-6: the recorder enriches fire snapshots with the symbol's
    // last-seen regime.trend value (finite, actually observed) — and a
    // symbol the feature never reached carries NO regime tag (never
    // fabricated, R-1). NaN updates do not poison the captured value.
    #[test]
    fn rel_25_recorder_captures_regime_context_into_snapshots() {
        let mut r = ObservationRecorder::new(
            &["sig".into()],
            "params".into(),
            1,
            6,
            "cost".into(),
            T0,
            0.0,
        );
        // Warm the quality tracker (5 updates) and set a regime value.
        for i in 0..5 {
            r.on_feature_update(&feat(SymbolId(1), 1.0, T0 + i));
        }
        let mut regime = feat(SymbolId(1), 1.0, T0 + 5); // 1.0 = CHOP
        regime.name = "regime.trend".into();
        r.on_feature_update(&regime);
        r.record_intent(&intent("sig", Side::Buy), T0 + 6, &BTreeMap::new());
        let snap = &r.observations()[0].feature_snapshot;
        assert_eq!(
            snap.get("regime.trend"),
            Some(&1.0),
            "fire snapshot carries the last-seen regime value"
        );

        // Last-seen wins: a later finite regime value overwrites the capture.
        // (A NaN regime update is SKIPPED by the capture's is_finite guard —
        // and separately poisons the quality tracker to Invalid, blocking
        // later fires on that symbol, which the dirty-fixture test covers.)
        let mut regime2 = feat(SymbolId(1), 0.0, T0 + 7); // 0.0 = TREND
        regime2.name = "regime.trend".into();
        r.on_feature_update(&regime2);
        let mut regime3 = feat(SymbolId(1), 1.0, T0 + 8); // back to CHOP
        regime3.name = "regime.trend".into();
        r.on_feature_update(&regime3);
        r.record_intent(&intent("sig", Side::Buy), T0 + 9, &BTreeMap::new());
        assert_eq!(
            r.observations()[1].feature_snapshot.get("regime.trend"),
            Some(&1.0),
            "a NaN regime update never poisons the captured context"
        );

        // A symbol with no regime history gets NO tag — the absent case stays
        // absent (R-1: unknown is never fabricated).
        for i in 0..5 {
            r.on_feature_update(&feat(SymbolId(2), 1.0, T0 + i));
        }
        let mut other = intent("sig", Side::Buy);
        other.symbol = SymbolId(2);
        r.record_intent(&other, T0 + 20, &BTreeMap::new());
        assert!(
            !r.observations()[2]
                .feature_snapshot
                .contains_key("regime.trend"),
            "no regime history ⇒ no regime tag"
        );
    }
}