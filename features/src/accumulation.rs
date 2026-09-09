//! Accumulation Detector (spec 045, ACC). Compound screener rule that
//! fires when three independent sub-signals co-occur:
//!
//! 1. `oi_rising` — OI delta positive + trending regime + OI quadrant 1 or 2
//! 2. `smart_money_buying` — cohort smart flow positive + net delta positive
//! 3. `exchange_outflow` — netflow velocity negative + regime == Outflow
//!
//! All three must be true simultaneously (AND logic, ACC-6). Thresholds are
//! dynamic percentile-based (ACC-3). A 4h cooldown prevents duplicate hits
//! per asset (ACC-4). Produces [`ScreenerHit`] events consumable by the
//! research pipeline (RES-4 event study, spec 025 signal catalog).

use crate::data_quality::DataQualityState;
use crate::engine::FeatureUpdate;
use crate::screener::ScreenerHit;
use mp_core::SymbolId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

const DAY_NS: i64 = 86_400_000_000_000;

/// Three-state sub-signal value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubSignal {
    Inactive,
    Active,
}

/// Config (spec 045 ACC-8: deny_unknown_fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccumulationConfig {
    /// Feature name for OI delta (e.g., "oi.delta.4h").
    #[serde(default = "default_oi_delta_feature")]
    pub oi_delta_feature: String,
    /// Feature name for trend regime (e.g., "regime.trend").
    #[serde(default = "default_regime_trend_feature")]
    pub regime_trend_feature: String,
    /// Feature name for OI quadrant (e.g., "oi.quadrant.4h").
    #[serde(default = "default_oi_quadrant_feature")]
    pub oi_quadrant_feature: String,
    /// Feature name for smart money net delta (e.g., "cohort.net_delta.smart_money").
    #[serde(default = "default_smart_delta_feature")]
    pub smart_delta_feature: String,
    /// Feature name for smart money flow (e.g., "cohort.smart_flow.24h").
    #[serde(default = "default_smart_flow_feature")]
    pub smart_flow_feature: String,
    /// Feature name for whale OI ratio (e.g., "cohort.whale_ratio").
    #[serde(default = "default_whale_ratio_feature")]
    pub whale_ratio_feature: String,
    /// Feature name for netflow velocity (e.g., "netflow.velocity.w2").
    #[serde(default = "default_netflow_velocity_feature")]
    pub netflow_velocity_feature: String,
    /// Feature name for netflow regime (e.g., "netflow.regime").
    #[serde(default = "default_netflow_regime_feature")]
    pub netflow_regime_feature: String,
    /// Cooldown between hits per asset (ns, default 4h).
    #[serde(default = "default_cooldown_ns")]
    pub cooldown_ns: i64,
    /// Percentile window for dynamic thresholds (ns, default 7d).
    #[serde(default = "default_percentile_window_ns")]
    pub percentile_window_ns: i64,
    /// OI delta percentile threshold (default 0.80 = top 20%).
    #[serde(default = "default_oi_percentile")]
    pub oi_delta_percentile: f64,
    /// Smart flow percentile threshold (default 0.70 = top 30%).
    #[serde(default = "default_smart_flow_percentile")]
    pub smart_flow_percentile: f64,
    /// Minimum whale OI ratio floor (default 0.10).
    #[serde(default = "default_whale_ratio_floor")]
    pub whale_ratio_floor: f64,
    /// OI quadrant values that count as "rising with support" (default [1, 2]).
    #[serde(default = "default_oi_quadrants")]
    pub oi_quadrants: Vec<i64>,
    /// Trend regime values that count as "trending" (default [0] = Trend).
    #[serde(default = "default_trend_values")]
    pub trend_values: Vec<f64>,
    /// Netflow regime value for "Outflow" (default 2.0).
    #[serde(default = "default_outflow_regime")]
    pub outflow_regime: f64,
    /// Enable the accumulation detector.
    #[serde(default)]
    pub enabled: bool,
}

fn default_oi_delta_feature() -> String {
    "oi.delta.4h".into()
}
fn default_regime_trend_feature() -> String {
    "regime.trend".into()
}
fn default_oi_quadrant_feature() -> String {
    "oi.quadrant.4h".into()
}
fn default_smart_delta_feature() -> String {
    "cohort.net_delta.smart_money".into()
}
fn default_smart_flow_feature() -> String {
    "cohort.smart_flow.24h".into()
}
fn default_whale_ratio_feature() -> String {
    "cohort.whale_ratio".into()
}
fn default_netflow_velocity_feature() -> String {
    "netflow.velocity.w2".into()
}
fn default_netflow_regime_feature() -> String {
    "netflow.regime".into()
}
fn default_cooldown_ns() -> i64 {
    4 * 3600 * 1_000_000_000
} // 4h
fn default_percentile_window_ns() -> i64 {
    7 * DAY_NS
} // 7d
fn default_oi_percentile() -> f64 {
    0.80
}
fn default_smart_flow_percentile() -> f64 {
    0.70
}
fn default_whale_ratio_floor() -> f64 {
    0.10
}
fn default_oi_quadrants() -> Vec<i64> {
    vec![1, 2]
}
fn default_trend_values() -> Vec<f64> {
    vec![0.0]
}
fn default_outflow_regime() -> f64 {
    2.0
}

impl Default for AccumulationConfig {
    fn default() -> Self {
        Self {
            oi_delta_feature: default_oi_delta_feature(),
            regime_trend_feature: default_regime_trend_feature(),
            oi_quadrant_feature: default_oi_quadrant_feature(),
            smart_delta_feature: default_smart_delta_feature(),
            smart_flow_feature: default_smart_flow_feature(),
            whale_ratio_feature: default_whale_ratio_feature(),
            netflow_velocity_feature: default_netflow_velocity_feature(),
            netflow_regime_feature: default_netflow_regime_feature(),
            cooldown_ns: default_cooldown_ns(),
            percentile_window_ns: default_percentile_window_ns(),
            oi_delta_percentile: default_oi_percentile(),
            smart_flow_percentile: default_smart_flow_percentile(),
            whale_ratio_floor: default_whale_ratio_floor(),
            oi_quadrants: default_oi_quadrants(),
            trend_values: default_trend_values(),
            outflow_regime: default_outflow_regime(),
            enabled: false,
        }
    }
}

/// Rolling value buffer for one feature (per symbol).
#[derive(Debug, Default)]
struct ValueBuffer {
    values: VecDeque<(i64, f64)>, // (ts_ns, value) ascending
    max_len: usize,
}

impl ValueBuffer {
    fn new(max_len: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(max_len.min(4096)),
            max_len: max_len.max(8),
        }
    }

    fn push(&mut self, ts_ns: i64, value: f64) {
        self.values.push_back((ts_ns, value));
        while self.values.len() > self.max_len {
            self.values.pop_front();
        }
    }

    /// Midrank percentile of the LATEST value within the trailing window (ACC-3).
    /// `None` = fewer than 5 in-window observations — insufficient history,
    /// which must BLOCK the sub-signal, never become a neutral 0.5 (spec 054
    /// REL-2; the pre-hardening caller used `unwrap_or(0.5)` here, silently
    /// fabricating a median for short history).
    fn percentile_in_window(&self, now_ns: i64, window_ns: i64) -> Option<f64> {
        let cutoff = now_ns.saturating_sub(window_ns);
        let in_window: Vec<f64> = self
            .values
            .iter()
            .filter(|&&(t, _)| t >= cutoff)
            .map(|&(_, v)| v)
            .collect();
        let n = in_window.len();
        if n < 5 {
            return None;
        } // need enough observations (REL-2)
        let cur = *in_window.last()?;
        let mut below = 0usize;
        let mut equal = 0usize;
        for &v in &in_window {
            if v < cur {
                below += 1;
            } else if v == cur {
                equal += 1;
            }
        }
        Some((below as f64 + 0.5 * equal as f64) / n as f64)
    }

    /// Number of observations within the trailing window (evidence for the
    /// hit snapshot, spec 054 REL-6: sample counts make the percentile's
    /// support explicit).
    fn window_samples(&self, now_ns: i64, window_ns: i64) -> u64 {
        let cutoff = now_ns.saturating_sub(window_ns);
        self.values.iter().filter(|&&(t, _)| t >= cutoff).count() as u64
    }

    /// Latest value.
    fn latest(&self) -> Option<f64> {
        self.values.back().map(|&(_, v)| v)
    }
}

/// Per-symbol accumulator state.
#[derive(Debug, Default)]
struct SymbolState {
    oi_delta: ValueBuffer,
    smart_flow: ValueBuffer,
    netflow_velocity: ValueBuffer,
    /// Last hit timestamp per symbol (ACC-4 cooldown).
    last_hit_ns: i64,
    /// Latest cached feature values (for snapshot).
    features: BTreeMap<String, f64>,
}

/// Accumulation detector — consumes streaming FeatureUpdates, evaluates
/// three sub-signals per symbol, and produces ScreenerHits on co-occurrence.
pub struct AccumulationDetector {
    cfg: AccumulationConfig,
    /// Per-symbol state keyed by SymbolId.
    states: BTreeMap<SymbolId, SymbolState>,
    /// History window length (number of samples to retain).
    buffer_len: usize,
}

impl AccumulationDetector {
    pub fn new(cfg: AccumulationConfig) -> Self {
        Self {
            cfg,
            states: BTreeMap::new(),
            buffer_len: 4096,
        }
    }

    /// Feature name list for iteration (avoids borrowing self.cfg during mutation).
    #[cfg(test)]
    fn feature_names(&self) -> Vec<String> {
        self.input_features()
    }

    /// The eight input feature names this detector consumes (ACC-2
    /// observability: studies report which legs received data).
    pub fn input_features(&self) -> Vec<String> {
        vec![
            self.cfg.oi_delta_feature.clone(),
            self.cfg.regime_trend_feature.clone(),
            self.cfg.oi_quadrant_feature.clone(),
            self.cfg.smart_delta_feature.clone(),
            self.cfg.smart_flow_feature.clone(),
            self.cfg.whale_ratio_feature.clone(),
            self.cfg.netflow_velocity_feature.clone(),
            self.cfg.netflow_regime_feature.clone(),
        ]
    }

    /// Feed one feature update. Returns a ScreenerHit when all three
    /// sub-signals fire simultaneously for a symbol.
    pub fn on_update(&mut self, update: &FeatureUpdate, now_ns: i64) -> Option<ScreenerHit> {
        let oi_feat = self.cfg.oi_delta_feature.clone();
        let sf_feat = self.cfg.smart_flow_feature.clone();
        let nv_feat = self.cfg.netflow_velocity_feature.clone();
        let oi_pctile = self.cfg.oi_delta_percentile;
        let sf_pctile = self.cfg.smart_flow_percentile;
        let pw_ns = self.cfg.percentile_window_ns;
        let wr_floor = self.cfg.whale_ratio_floor;
        let trend_vals = self.cfg.trend_values.clone();
        let oi_quads = self.cfg.oi_quadrants.clone();
        let outflow_reg = self.cfg.outflow_regime;
        let cooldown = self.cfg.cooldown_ns;
        let sd_feat = self.cfg.smart_delta_feature.clone();
        let wr_feat = self.cfg.whale_ratio_feature.clone();
        let nr_feat = self.cfg.netflow_regime_feature.clone();
        let rt_feat = self.cfg.regime_trend_feature.clone();
        let oq_feat = self.cfg.oi_quadrant_feature.clone();

        let state = self
            .states
            .entry(update.symbol)
            .or_insert_with(|| SymbolState {
                oi_delta: ValueBuffer::new(self.buffer_len),
                smart_flow: ValueBuffer::new(self.buffer_len),
                netflow_velocity: ValueBuffer::new(self.buffer_len),
                ..SymbolState::default()
            });

        state.features.insert(update.name.clone(), update.value);
        let fname = &update.name;
        if fname == &oi_feat {
            state.oi_delta.push(update.ts_ns, update.value);
        } else if fname == &sf_feat {
            state.smart_flow.push(update.ts_ns, update.value);
        } else if fname == &nv_feat {
            state.netflow_velocity.push(update.ts_ns, update.value);
        }

        // Evaluate (borrow-free: state is borrowed mutably but we only read config clones).
        if now_ns - state.last_hit_ns < cooldown {
            return None;
        }
        let oi_delta = state.oi_delta.latest()?;
        let regime_trend = state.features.get(&rt_feat).copied()?;
        let oi_quadrant = state.features.get(&oq_feat).copied()?;
        let smart_delta = state.features.get(&sd_feat).copied()?;
        let smart_flow = state.smart_flow.latest()?;
        let whale_ratio = state.features.get(&wr_feat).copied()?;
        let velocity = state.netflow_velocity.latest()?;
        let netflow_regime = state.features.get(&nr_feat).copied()?;

        // Sub-signal 1: OI rising. Spec 054 REL-2: an `None` percentile
        // (fewer than 5 in-window samples) BLOCKS the sub-signal — it is
        // never neutralized to 0.5 (the pre-hardening `unwrap_or(0.5)`
        // silently scored short history as a median).
        let oi_pctl = state.oi_delta.percentile_in_window(now_ns, pw_ns);
        let oi_active = oi_pctl.is_some()
            && trend_vals.contains(&regime_trend)
            && oi_quads.contains(&(oi_quadrant as i64))
            && oi_pctl.unwrap_or(f64::NAN) >= oi_pctile
            && oi_delta > 0.0;
        if !oi_active {
            return None;
        }

        // Sub-signal 2: Smart money buying (same REL-2 blocking rule).
        let sf_pctl = state.smart_flow.percentile_in_window(now_ns, pw_ns);
        let sm_active = sf_pctl.is_some()
            && whale_ratio >= wr_floor
            && smart_flow > 0.0
            && smart_delta > 0.0
            && sf_pctl.unwrap_or(f64::NAN) >= sf_pctile;
        if !sm_active {
            return None;
        }

        // Sub-signal 3: Exchange outflow.
        let ex_active = (netflow_regime - outflow_reg).abs() < f64::EPSILON && velocity < 0.0;
        if !ex_active {
            return None;
        }

        // All three active — fire. Evidence representation (spec 054 REL-6):
        // the snapshot carries the per-leg percentile + in-window sample
        // counts that PRODUCED the decision, so a consumer can see how much
        // history supported each leg instead of only the raw feature values.
        state.last_hit_ns = now_ns;
        let mut snapshot = BTreeMap::new();
        snapshot.insert("sub_signal.oi_rising".into(), 1.0);
        snapshot.insert("sub_signal.smart_money_buying".into(), 1.0);
        snapshot.insert("sub_signal.exchange_outflow".into(), 1.0);
        snapshot.insert("accumulation_score".into(), 3.0);
        if let Some(p) = oi_pctl {
            snapshot.insert("evidence.oi_delta.percentile".into(), p);
        }
        snapshot.insert(
            "evidence.oi_delta.samples".into(),
            state.oi_delta.window_samples(now_ns, pw_ns) as f64,
        );
        if let Some(p) = sf_pctl {
            snapshot.insert("evidence.smart_flow.percentile".into(), p);
        }
        snapshot.insert(
            "evidence.smart_flow.samples".into(),
            state.smart_flow.window_samples(now_ns, pw_ns) as f64,
        );
        snapshot.insert("evidence.insufficient_history".into(), 0.0);
        for (k, v) in &state.features {
            snapshot.insert(format!("raw.{k}"), *v);
        }
        Some(ScreenerHit {
            rule_id: "accumulation_detector".into(),
            symbol: update.symbol,
            ts_ns: now_ns,
            snapshot,
            quality: crate::data_quality::DataQualityState::Healthy,
        })
    }

    /// Explicit quality per symbol (spec 054 REL-2/REL-27): `Missing` when
    /// the symbol has never been observed, `InsufficientHistory` while either
    /// percentile window has fewer than 5 in-window samples, else `Healthy`.
    /// The detector never fires while this is not `Healthy`, and this
    /// accessor makes the blocked state OBSERVABLE instead of silent — a
    /// user can now see *why* a symbol is not producing hits.
    pub fn quality(&self, symbol: SymbolId, now_ns: i64) -> DataQualityState {
        let Some(state) = self.states.get(&symbol) else {
            return DataQualityState::Missing;
        };
        let oi = state.oi_delta.percentile_in_window(now_ns, self.cfg.percentile_window_ns);
        let sf = state.smart_flow.percentile_in_window(now_ns, self.cfg.percentile_window_ns);
        if oi.is_some() && sf.is_some() {
            DataQualityState::Healthy
        } else {
            DataQualityState::InsufficientHistory
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{SymbolId, Venue};

    const BTC: SymbolId = SymbolId(1);

    fn update(sym: SymbolId, name: &str, val: f64, ts_ns: i64) -> FeatureUpdate {
        FeatureUpdate {
            symbol: sym,
            feature: sym,
            name: name.to_string(),
            venue: Venue::Bybit,
            value: val,
            ts_ns,
            ver: 1,
        }
    }

    fn cfg() -> AccumulationConfig {
        AccumulationConfig {
            cooldown_ns: 1000,                                // very short for tests
            percentile_window_ns: 100 * 3600 * 1_000_000_000, // 100h (wide enough to include all hourly samples)
            ..AccumulationConfig::default()
        }
    }

    /// Seed a symbol with all required feature values at a given timestamp.
    #[allow(clippy::too_many_arguments)]
    fn seed_all(
        det: &mut AccumulationDetector,
        sym: SymbolId,
        ts_ns: i64,
        oi_delta: f64,
        regime: f64,
        quadrant: f64,
        smart_delta: f64,
        smart_flow: f64,
        whale_ratio: f64,
        velocity: f64,
        netflow_regime: f64,
    ) {
        let names = det.feature_names();
        let vals = [
            oi_delta,
            regime,
            quadrant,
            smart_delta,
            smart_flow,
            whale_ratio,
            velocity,
            netflow_regime,
        ];
        for (fname, val) in names.iter().zip(vals.iter()) {
            det.on_update(&update(sym, fname, *val, ts_ns), ts_ns);
        }
    }

    #[test]
    fn acc_1_screener_rule_produces_hits_on_fixture_data() {
        let mut det = AccumulationDetector::new(cfg());
        // Seed with enough history to establish dynamic thresholds.
        for i in 0..10i64 {
            let ts = i * 3_600_000_000_000; // hourly
            seed_all(
                &mut det, BTC, ts, 100.0, 0.0,
                1.0, // oi_delta=100, trend=0 (Trend), quadrant=1
                50.0, 200.0, 0.15, // smart_delta=50, smart_flow=200, whale_ratio=0.15
                -500.0, 2.0,
            ); // velocity=-500 (outflow), regime=2 (Outflow)
        }
        // Now trigger with a HIGH oi_delta that should be above 80th percentile.
        let ts = 10 * 3_600_000_000_000;
        let hit = seed_and_check(
            &mut det, BTC, ts, 500.0, 0.0, 1.0, 100.0, 400.0, 0.20, -1000.0, 2.0,
        );
        assert!(hit.is_some(), "ACC-1: all three sub-signals should fire");
        let h = hit.unwrap();
        assert_eq!(h.rule_id, "accumulation_detector");
        assert_eq!(h.symbol, BTC);
        assert_eq!(h.snapshot.get("accumulation_score"), Some(&3.0));
        assert_eq!(h.snapshot.get("sub_signal.oi_rising"), Some(&1.0));
        assert_eq!(h.snapshot.get("sub_signal.smart_money_buying"), Some(&1.0));
        assert_eq!(h.snapshot.get("sub_signal.exchange_outflow"), Some(&1.0));
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_and_check(
        det: &mut AccumulationDetector,
        sym: SymbolId,
        ts_ns: i64,
        oi_delta: f64,
        regime: f64,
        quadrant: f64,
        smart_delta: f64,
        smart_flow: f64,
        whale_ratio: f64,
        velocity: f64,
        netflow_regime: f64,
    ) -> Option<ScreenerHit> {
        let mut last_hit = None;
        let names = det.feature_names();
        let vals = [
            oi_delta,
            regime,
            quadrant,
            smart_delta,
            smart_flow,
            whale_ratio,
            velocity,
            netflow_regime,
        ];
        for (fname, val) in names.iter().zip(vals.iter()) {
            if let Some(h) = det.on_update(&update(sym, fname, *val, ts_ns), ts_ns) {
                last_hit = Some(h);
            }
        }
        last_hit
    }

    #[test]
    fn acc_2_partial_match_does_not_fire() {
        let mut det = AccumulationDetector::new(cfg());
        // Seed history.
        for i in 0..10i64 {
            let ts = i * 3_600_000_000_000;
            seed_all(
                &mut det, BTC, ts, 100.0, 0.0, 1.0, 50.0, 200.0, 0.15, -500.0, 2.0,
            );
        }
        // Only oi_rising fires (others don't).
        let ts = 10 * 3_600_000_000_000;
        let hit = seed_and_check(
            &mut det, BTC, ts, 500.0, 0.0, 1.0, // oi_rising fires
            -10.0, -50.0, 0.20, // smart_money NOT buying (negative flow/delta)
            500.0, 1.0,
        ); // exchange NOT outflow (positive velocity, regime=1)
        assert!(hit.is_none(), "ACC-2: 2/3 signals should not fire");
    }

    #[test]
    fn acc_4_cooldown_suppresses_duplicate_hits() {
        let mut det = AccumulationDetector::new(cfg());
        for i in 0..10i64 {
            let ts = i * 3_600_000_000_000;
            seed_all(
                &mut det, BTC, ts, 100.0, 0.0, 1.0, 50.0, 200.0, 0.15, -500.0, 2.0,
            );
        }
        let ts = 10 * 3_600_000_000_000;
        let hit1 = seed_and_check(
            &mut det, BTC, ts, 500.0, 0.0, 1.0, 100.0, 400.0, 0.20, -1000.0, 2.0,
        );
        assert!(hit1.is_some(), "first hit should fire");
        // Second hit immediately (within cooldown) — should be suppressed.
        let hit2 = seed_and_check(
            &mut det,
            BTC,
            ts + 1,
            500.0,
            0.0,
            1.0,
            100.0,
            400.0,
            0.20,
            -1000.0,
            2.0,
        );
        assert!(hit2.is_none(), "ACC-4: cooldown should suppress duplicate");
    }

    #[test]
    fn acc_6_and_logic_requires_all_three() {
        let mut det = AccumulationDetector::new(cfg());
        for i in 0..10i64 {
            let ts = i * 3_600_000_000_000;
            seed_all(
                &mut det, BTC, ts, 100.0, 0.0, 1.0, 50.0, 200.0, 0.15, -500.0, 2.0,
            );
        }
        let ts = 10 * 3_600_000_000_000;
        // Only smart_money_buying fires.
        let hit = seed_and_check(
            &mut det, BTC, ts, 10.0, 0.0, 1.0, // oi NOT above threshold
            100.0, 500.0, 0.20, // smart money active
            100.0, 1.0,
        ); // exchange NOT outflow
        assert!(
            hit.is_none(),
            "ACC-6: single sub-signal alone must not fire"
        );
    }

    #[test]
    fn acc_8_check_config_rejects_unknown_fields() {
        // Empty config should parse fine.
        let result = serde_json::from_str::<AccumulationConfig>("{}");
        assert!(result.is_ok());
        // Unknown field must be rejected (deny_unknown_fields).
        let result_json =
            serde_json::from_str::<AccumulationConfig>(r#"{"enabled": true, "bad_key": "oops"}"#);
        assert!(
            result_json.is_err(),
            "ACC-8: unknown fields must be rejected"
        );
    }

    #[test]
    fn acc_3_dynamic_thresholds_adapt_to_distribution() {
        let mut det = AccumulationDetector::new(cfg());
        // Seed with a distribution of oi_delta values, then check threshold.
        for i in 0..20i64 {
            let ts = i * 3_600_000_000_000;
            let oi = (i as f64) * 10.0; // 0, 10, 20, ..., 190
            seed_all(
                &mut det, BTC, ts, oi, 0.0, 1.0, 50.0, 200.0, 0.15, -500.0, 2.0,
            );
        }
        // At 80th percentile of [0,10,...,190], threshold ~152.
        // OI=160 should be above threshold.
        let ts = 20 * 3_600_000_000_000;
        let hit = seed_and_check(
            &mut det, BTC, ts, 160.0, 0.0, 1.0, 100.0, 400.0, 0.20, -1000.0, 2.0,
        );
        assert!(
            hit.is_some(),
            "ACC-3: OI=160 should be above 80th percentile threshold"
        );
    }

    #[test]
    fn acc_9_signal_catalog_entry_valid() {
        // Verify that AccumulationConfig has the expected defaults for catalog.
        let c = AccumulationConfig::default();
        assert!(!c.enabled, "should be disabled by default");
        assert_eq!(c.oi_delta_percentile, 0.80);
        assert_eq!(c.smart_flow_percentile, 0.70);
        assert_eq!(c.whale_ratio_floor, 0.10);
        assert_eq!(c.outflow_regime, 2.0);
        assert_eq!(c.cooldown_ns, 4 * 3600 * 1_000_000_000);
    }

    #[test]
    fn acc_7_offline_online_identity_golden() {
        // ACC-7 / FEA-4 one-code-path: the SAME detector type replayed over
        // the same update stream in two fresh instances produces the
        // byte-identical ScreenerHit sequence — there is no separate offline
        // vs online evaluation logic to drift.
        let stream: Vec<(SymbolId, String, f64, i64)> = {
            let mut v: Vec<(SymbolId, String, f64, i64)> = Vec::new();
            for i in 0..10i64 {
                let ts = i * 3_600_000_000_000;
                for (name, val) in [
                    ("oi.delta.4h", 100.0),
                    ("regime.trend", 0.0),
                    ("oi.quadrant.4h", 1.0),
                    ("cohort.net_delta.smart_money", 50.0),
                    ("cohort.smart_flow.24h", 200.0),
                    ("cohort.whale_ratio", 0.15),
                    ("netflow.velocity.w2", -500.0),
                    ("netflow.regime", 2.0),
                ] {
                    v.push((BTC, name.to_string(), val, ts));
                }
            }
            for (name, val) in [
                ("oi.delta.4h", 500.0),
                ("regime.trend", 0.0),
                ("oi.quadrant.4h", 1.0),
                ("cohort.net_delta.smart_money", 100.0),
                ("cohort.smart_flow.24h", 400.0),
                ("cohort.whale_ratio", 0.20),
                ("netflow.velocity.w2", -1000.0),
                ("netflow.regime", 2.0),
            ] {
                v.push((BTC, name.to_string(), val, 10 * 3_600_000_000_000));
            }
            v
        };
        let run = || {
            let mut det = AccumulationDetector::new(cfg());
            let hits: Vec<ScreenerHit> = stream
                .iter()
                .filter_map(|(sym, name, val, ts)| {
                    det.on_update(&update(*sym, name, *val, *ts), *ts)
                })
                .collect();
            serde_json::to_string(
                &hits
                    .iter()
                    .map(|h| (h.rule_id.as_str(), h.symbol, h.ts_ns, &h.snapshot))
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let (a, b) = (run(), run());
        assert_eq!(a, b, "identical replays must be golden-identical");
        assert!(
            a.contains("accumulation_detector"),
            "stream must produce the hit"
        );
    }

    #[test]
    fn rel_2_insufficient_history_blocks_and_is_observable() {
        // Spec 054 REL-2: with fewer than 5 in-window samples the percentile
        // is None and the sub-signal BLOCKS (never a neutral 0.5), and the
        // `quality()` accessor makes the blocked state explicit.
        let mut det = AccumulationDetector::new(cfg());
        // 2 samples only — below the 5-sample minimum.
        for i in 0..2i64 {
            seed_all(
                &mut det, BTC, i * 3_600_000_000_000, 100.0, 0.0, 1.0, 50.0, 200.0, 0.15,
                -500.0, 2.0,
            );
        }
        assert_eq!(
            det.quality(BTC, 2 * 3_600_000_000_000),
            DataQualityState::InsufficientHistory
        );
        // Even an extreme trigger must NOT fire: insufficient history blocks.
        let hit = seed_and_check(
            &mut det, BTC, 2 * 3_600_000_000_000, 1_000_000.0, 0.0, 1.0, 100.0, 400.0, 0.20,
            -1000.0, 2.0,
        );
        assert!(hit.is_none(), "REL-2: insufficient history must block");
        // Once seeded to 10 samples, quality is Healthy and it fires.
        for i in 2..10i64 {
            let ts = i * 3_600_000_000_000;
            seed_all(
                &mut det, BTC, ts, 100.0, 0.0, 1.0, 50.0, 200.0, 0.15, -500.0, 2.0,
            );
        }
        assert_eq!(det.quality(BTC, 10 * 3_600_000_000_000), DataQualityState::Healthy);
    }

    #[test]
    fn rel_6_hit_snapshot_carries_per_leg_evidence() {
        let mut det = AccumulationDetector::new(cfg());
        for i in 0..10i64 {
            let ts = i * 3_600_000_000_000;
            seed_all(
                &mut det, BTC, ts, 100.0, 0.0, 1.0, 50.0, 200.0, 0.15, -500.0, 2.0,
            );
        }
        let ts = 10 * 3_600_000_000_000;
        let hit = seed_and_check(
            &mut det, BTC, ts, 500.0, 0.0, 1.0, 100.0, 400.0, 0.20, -1000.0, 2.0,
        )
        .expect("fire");
        assert!(hit.snapshot.contains_key("evidence.oi_delta.percentile"));
        assert!(hit.snapshot.contains_key("evidence.oi_delta.samples"));
        assert!(hit.snapshot.contains_key("evidence.smart_flow.percentile"));
        assert!(hit.snapshot.contains_key("evidence.smart_flow.samples"));
        assert_eq!(hit.snapshot.get("evidence.insufficient_history"), Some(&0.0));
        // Sample counts reflect the seeded history (10 hourly) + the trigger
        // update itself (11 in-window at fire time).
        assert_eq!(hit.snapshot.get("evidence.oi_delta.samples"), Some(&11.0));
        assert_eq!(hit.snapshot.get("evidence.smart_flow.samples"), Some(&11.0));
    }

    #[test]
    fn acc_10_cooldown_crosses_bar_boundary() {
        // Default 4h cooldown against hourly bars: a hit at bar T blocks
        // re-fires INSIDE the window; the first hourly boundary at/after
        // expiry (exactly T+4h) is eligible again as an independent signal.
        let mut det = AccumulationDetector::new(AccumulationConfig {
            cooldown_ns: 4 * 3600 * 1_000_000_000,
            ..cfg()
        });
        let hour = 3_600_000_000_000i64;
        for i in 0..10i64 {
            seed_all(
                &mut det,
                BTC,
                i * hour,
                100.0,
                0.0,
                1.0,
                50.0,
                200.0,
                0.15,
                -500.0,
                2.0,
            );
        }
        let t_hit = 10 * hour;
        assert!(seed_and_check(
            &mut det, BTC, t_hit, 500.0, 0.0, 1.0, 100.0, 400.0, 0.20, -1000.0, 2.0
        )
        .is_some());
        // +1h, +2h, +3h → still inside cooldown → suppressed.
        for k in 1..=3i64 {
            assert!(
                seed_and_check(
                    &mut det,
                    BTC,
                    t_hit + k * hour,
                    600.0,
                    0.0,
                    1.0,
                    120.0,
                    450.0,
                    0.22,
                    -1100.0,
                    2.0
                )
                .is_none(),
                "+{k}h must be suppressed"
            );
        }
        // Exactly +4h — the next hourly bar boundary after expiry — fires.
        assert!(
            seed_and_check(
                &mut det,
                BTC,
                t_hit + 4 * hour,
                700.0,
                0.0,
                1.0,
                140.0,
                500.0,
                0.25,
                -1200.0,
                2.0
            )
            .is_some(),
            "T+4h boundary is a new independent signal"
        );
    }
}
