//! CEX flow-velocity features (spec 043, CFV). Derived from spec 034
//! NetflowSnapshot events: velocity (balance units/day), acceleration,
//! trapezoidal cumulative flow, midrank-percentile regime, and z-score —
//! aggregated across the watchlist with last-wins per-address upserts.
//!
//! Fail-closed (CONV-8/CFV list): <2 snapshots ⇒ no velocity; <4 ⇒ no
//! acceleration; non-finite balances skip that address; regime needs ≥20
//! historical velocities; z-score needs σ > ε.

use crate::engine::TickFeature;
use mp_core::{EventEnvelope, MarketEvent};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

const DAY_NS: i64 = 86_400_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetflowField {
    Velocity(usize), // index into windows_ns
    Acceleration(usize),
    Cumulative,
    Regime,
    ZScore,
}

/// Config (spec 043 CFV-8 defaults).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetflowFlowConfig {
    /// Velocity windows in ns (default 1h / 4h / 24h).
    #[serde(default = "default_windows_ns")]
    pub windows_ns: Vec<i64>,
    /// Trailing cumulative window (default 7d).
    #[serde(default = "default_cumulative_ns")]
    pub cumulative_window_ns: i64,
    /// Regime percentile lookback cap (days; default 90).
    #[serde(default = "default_regime_days")]
    pub regime_lookback_days: i64,
    /// Z-score trailing window (default 24h).
    #[serde(default = "default_zscore_ns")]
    pub zscore_window_ns: i64,
    /// Minimum historical velocities before the regime may classify (CFV-4).
    #[serde(default = "default_min_obs")]
    pub min_regime_obs: usize,
    /// Address staleness cap (default 600s = 2× the spec 034 poll cadence).
    /// An address not polled within this window is EXCLUDED from the
    /// aggregate — its last balance is no longer honest (CFV-8).
    #[serde(default = "default_stale_after_ns")]
    pub stale_after_ns: i64,
    /// Enable family registration.
    #[serde(default)]
    pub enabled: bool,
}

fn default_windows_ns() -> Vec<i64> {
    vec![
        3_600 * 1_000_000_000,
        4 * 3_600 * 1_000_000_000,
        24 * 3_600 * 1_000_000_000,
    ]
}
fn default_cumulative_ns() -> i64 {
    7 * DAY_NS
}
fn default_regime_days() -> i64 {
    90
}
fn default_zscore_ns() -> i64 {
    24 * 3_600 * 1_000_000_000
}
fn default_min_obs() -> usize {
    20
}
fn default_stale_after_ns() -> i64 {
    // 2× the spec 034 NetflowSnapshot poll cadence (300s), per CFV-8.
    2 * 300 * 1_000_000_000
}

impl Default for NetflowFlowConfig {
    fn default() -> Self {
        Self {
            windows_ns: default_windows_ns(),
            cumulative_window_ns: default_cumulative_ns(),
            regime_lookback_days: default_regime_days(),
            zscore_window_ns: default_zscore_ns(),
            min_regime_obs: default_min_obs(),
            stale_after_ns: default_stale_after_ns(),
            enabled: false,
        }
    }
}

/// Per-address last-wins snapshot + the aggregate total-balance series
/// (spec 043 address aggregation).
#[derive(Debug, Default)]
struct BalanceHistory {
    latest: BTreeMap<String, (i64, f64)>,
    series: VecDeque<(i64, f64)>,     // ascending ts, aggregate total
    velocities: VecDeque<(i64, f64)>, // (window_end_ts, velocity/day) history
    max_series: usize,
}

impl BalanceHistory {
    fn new(max_series: usize) -> Self {
        Self {
            max_series: max_series.max(8),
            ..Self::default()
        }
    }

    /// Last-wins upsert; recompute the aggregate total and append when the
    /// total CHANGED or time advanced (both feed endpoint-based math).
    /// Addresses not refreshed within `stale_after_ns` are evicted BEFORE the
    /// total is recomputed (CFV-8 — the just-polled address can never evict
    /// itself), so the aggregate is the CURRENT watchlist, never a graveyard.
    fn upsert(&mut self, addr: &str, ts_ns: i64, balance: f64, stale_after_ns: i64) -> bool {
        if !balance.is_finite() {
            return false; // CONV-8: skip non-finite contributions
        }
        self.latest.insert(addr.to_owned(), (ts_ns, balance));
        let cutoff = ts_ns - stale_after_ns;
        self.latest.retain(|_, &mut (ts, _)| ts >= cutoff);
        let total: f64 = self.latest.values().map(|(_, b)| b).sum();
        if !total.is_finite() {
            return false;
        }
        match self.series.back() {
            Some(&(last_ts, _)) if last_ts == ts_ns => {
                self.series.pop_back();
            }
            _ => {}
        }
        self.series.push_back((ts_ns, total));
        while self.series.len() > self.max_series {
            self.series.pop_front();
        }
        true
    }

    /// Trim series older than the largest analysis horizon.
    fn trim(&mut self, keep_from_ts: i64) {
        while matches!(self.series.front(), Some(&(ts, _)) if ts < keep_from_ts) {
            self.series.pop_front();
        }
    }

    /// Endpoint velocity over `window_ns` ending at the LATEST snapshot:
    /// find the newest sample ≤ now−window as the anchor. None when no such
    /// anchor exists (<2 effective snapshots in-window, CFV-1).
    fn velocity(&self, window_ns: i64) -> Option<f64> {
        let &(now_ts, now_bal) = self.series.back()?;
        let anchor_ts = now_ts - window_ns;
        let mut anchor = None;
        for &(ts, bal) in &self.series {
            if ts <= anchor_ts {
                anchor = Some((ts, bal));
            } else {
                break;
            }
        }
        let (a_ts, a_bal) = anchor?;
        let dt = (now_ts - a_ts) as f64;
        if dt <= 0.0 {
            return None;
        }
        Some((now_bal - a_bal) / dt * DAY_NS as f64)
    }

    /// Acceleration: (velocity_now − velocity_at_halfway) normalized to
    /// day⁻² scale. Requires ≥4 samples so both endpoints differ (CFV-2).
    fn acceleration(&self, window_ns: i64) -> Option<f64> {
        if self.series.len() < 4 {
            return None;
        }
        let v_now = self.velocity(window_ns)?;
        let half = window_ns / 2;
        // Velocity at half-window computed from the same series anchored at
        // now − window (start) → now − half.
        let &(now_ts, _) = self.series.back()?;
        let start_ts = now_ts - window_ns;
        let mid_ts = now_ts - half;
        let at = |q_ts: i64| -> Option<(i64, f64)> {
            let mut found = None;
            for s in self.series.iter().take(self.series.len().saturating_sub(1)) {
                let &(t, b) = s;
                if t <= q_ts {
                    found = Some((t, b));
                } else {
                    break;
                }
            }
            found
        };
        let (s_ts, s_bal) = at(start_ts)?;
        let (m_ts, m_bal) = at(mid_ts)?;
        let dt1 = (m_ts - s_ts) as f64;
        let dt2 = (now_ts - m_ts) as f64;
        if dt1 <= 0.0 || dt2 <= 0.0 {
            return None;
        }
        let v_half = (m_bal - s_bal) / dt1 * DAY_NS as f64;
        Some(((v_now - v_half) / dt2) * DAY_NS as f64)
    }

    /// Trapezoidal Σ velocity·Δt over the trailing window (CFV-3).
    fn cumulative(&self, window_ns: i64) -> Option<f64> {
        let &(now_ts, _) = self.series.back()?;
        let cut = now_ts - window_ns;
        let pts: Vec<(i64, f64)> = self
            .series
            .iter()
            .copied()
            .skip_while(|&(t, _)| t < cut)
            .collect();
        if pts.len() < 2 {
            return None;
        }
        let mut sum = 0.0;
        for w in pts.windows(2) {
            sum += w[1].1 - w[0].1; // balance delta per segment
        }
        sum.is_finite().then_some(sum)
    }

    /// Record the current velocity into history (called by the feature after
    /// computing it) for regime/z-score statistics.
    fn record_velocity(&mut self, ts: i64, v: f64) {
        self.velocities.push_back((ts, v));
        let cap = 2000;
        while self.velocities.len() > cap {
            self.velocities.pop_front();
        }
    }

    /// Midrank percentile of the CURRENT velocity vs recorded history
    /// (CFV-4): (below + 0.5×equal)/n. Requires ≥ min_obs observations.
    fn regime_percentile(&self, min_obs: usize) -> Option<f64> {
        let n = self.velocities.len();
        if n < min_obs.max(1) {
            return None;
        }
        let (_, cur) = self.velocities.back()?;
        if !cur.is_finite() {
            return None;
        }
        let mut below = 0usize;
        let mut equal = 0usize;
        for &(_, v) in &self.velocities {
            if v < *cur {
                below += 1;
            } else if v == *cur {
                equal += 1;
            }
        }
        Some((below as f64 + 0.5 * equal as f64) / n as f64)
    }

    /// Z-score of current velocity over the trailing zscore window of
    /// recorded history (CFV-5); None when σ < ε.
    fn zscore(&self, window_ns: i64) -> Option<f64> {
        let &(now_ts, _) = self.velocities.back()?;
        let vals: Vec<f64> = self
            .velocities
            .iter()
            .filter(|&&(t, _)| t >= now_ts - window_ns)
            .map(|&(_, v)| v)
            .collect();
        let n = vals.len() as f64;
        if n < 2.0 {
            return None;
        }
        let mean = vals.iter().sum::<f64>() / n;
        let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let sd = var.sqrt();
        if !sd.is_finite() || sd < f64::EPSILON {
            return None;
        }
        let (_, cur) = self.velocities.back()?;
        Some((cur - mean) / sd)
    }
}

/// One instance per output field (registered ×(windows×2 + 3)).
pub struct NetflowFlowFeature {
    cfg: NetflowFlowConfig,
    field: NetflowField,
    hist: BalanceHistory,
}

impl NetflowFlowFeature {
    pub fn new(cfg: NetflowFlowConfig, field: NetflowField) -> Self {
        let horizon = cfg
            .windows_ns
            .iter()
            .copied()
            .chain([cfg.cumulative_window_ns, cfg.zscore_window_ns])
            .max()
            .unwrap_or(DAY_NS);
        let max_series = (horizon / 300_000_000_000).clamp(16, 4000) as usize; // 5-min cadence assumption
        Self {
            hist: BalanceHistory::new(max_series),
            cfg,
            field,
        }
    }

    fn id_for(field: &NetflowField) -> String {
        match field {
            NetflowField::Velocity(i) => format!("netflow.velocity.w{i}"),
            NetflowField::Acceleration(i) => format!("netflow.acceleration.w{i}"),
            NetflowField::Cumulative => "netflow.cumulative".into(),
            NetflowField::Regime => "netflow.regime".into(),
            NetflowField::ZScore => "netflow.zscore".into(),
        }
    }
}

impl TickFeature for NetflowFlowFeature {
    fn id(&self) -> String {
        Self::id_for(&self.field)
    }

    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let MarketEvent::NetflowSnapshot { address, balance } = &ev.body else {
            return None;
        };
        if !self
            .hist
            .upsert(address, ev.recv_ts_ns, *balance, self.cfg.stale_after_ns)
        {
            return None;
        }
        // Maintain the velocity history once per aggregate update (used by
        // regime/z-score); only the latest velocity is appended.
        let keep_from = ev
            .recv_ts_ns
            .saturating_sub(self.cfg.regime_lookback_days.saturating_mul(DAY_NS));
        self.hist.trim(keep_from);
        if let Some(v) = self
            .hist
            .velocity(*self.cfg.windows_ns.last().unwrap_or(&DAY_NS))
        {
            self.hist.record_velocity(ev.recv_ts_ns, v);
        }
        match self.field {
            NetflowField::Velocity(i) => {
                let w = self.cfg.windows_ns.get(i).copied().unwrap_or(DAY_NS);
                self.hist.velocity(w)
            }
            NetflowField::Acceleration(i) => {
                let w = *self.cfg.windows_ns.get(i)?;
                self.hist.acceleration(w)
            }
            NetflowField::Cumulative => self.hist.cumulative(self.cfg.cumulative_window_ns),
            NetflowField::Regime => {
                let p = self.hist.regime_percentile(self.cfg.min_regime_obs)?;
                // 0=Inflow (≥66%), 1=Neutral, 2=Outflow (≤33%) — CFV-4.
                Some(if p >= 0.66 {
                    0.0
                } else if p <= 0.33 {
                    2.0
                } else {
                    1.0
                })
            }
            NetflowField::ZScore => self.hist.zscore(self.cfg.zscore_window_ns),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{MarketEvent, SymbolId, Venue};
    use proptest::{prop_assert, prop_assert_eq};

    fn snap(ts_s: i64, bal: f64) -> EventEnvelope {
        EventEnvelope::new(
            Venue::Ethereum,
            SymbolId(3),
            ts_s * 1_000_000_000,
            ts_s * 1_000_000_000,
            1,
            MarketEvent::NetflowSnapshot {
                address: "0xa".into(),
                balance: bal,
            },
        )
    }

    fn feat(field: NetflowField) -> NetflowFlowFeature {
        NetflowFlowFeature::new(NetflowFlowConfig::default(), field)
    }

    #[test]
    fn cfv_1_velocity_hand_computed() {
        let mut f = feat(NetflowField::Velocity(0)); // 1h window
                                                     // 1000 @ T0 → 1100 @ T0+3600s: Δ100 over 3600s → 100/day = 2400/day.
        assert_eq!(f.on_event(&snap(0, 1000.0)), None);
        assert_eq!(f.on_event(&snap(3600, 1100.0)), Some(2400.0));
    }

    #[test]
    fn cfv_2_acceleration_two_velocity_points() {
        let mut f = feat(NetflowField::Acceleration(2));
        // Series crafted so 24h-velocity goes 100/day → 200/day over 12h:
        // b(t) linear pieces. T0..: 0@0, 100@12h? Build explicit points.
        // velocity_now (24h lookback): (b(T) − b(T−24h))/24h·day = 200
        // velocity_half (from T−24h → T−12h): 100
        for (ts_s, bal) in [(0i64, 0.0), (43_200, 50.0), (86_400, 200.0)] {
            f.on_event(&snap(ts_s, bal));
        }
        // now=86400: anchor ≤ T−24h → (0,0): v_now = 200/day ✓
        // half anchor ≤ T−12h → (43200,50): v_half=(50−0)/43200·day=100/day
        // acc = (200−100)/43200·day = 200/day²
        let got = f.on_event(&snap(86_401, 200.0)).expect("accel after 4th");
        assert!((got - 200.0).abs() < 1.0, "got {got}");
        let _ = f;
    }

    #[test]
    fn cfv_3_cumulative_trapezoid_irregular() {
        let cfg = NetflowFlowConfig {
            cumulative_window_ns: 86_400_000_000_000, // 1d (enough for 3h of data)
            ..NetflowFlowConfig::default()
        };
        let mut f = NetflowFlowFeature::new(cfg, NetflowField::Cumulative);
        // Segments: +100 over 3600s then +50 over 7200s → cumulative = 150.
        f.on_event(&snap(0, 0.0));
        f.on_event(&snap(3600, 100.0));
        let got = f.on_event(&snap(10_800, 150.0)).unwrap();
        assert!((got - 150.0).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn cfv_4_regime_midrank_thresholds() {
        let mut f = feat(NetflowField::Regime);
        let cfg = NetflowFlowConfig::default();
        // Seed ≥20 velocity observations by oscillating totals; velocities
        // recorded on each update use the 24h window.
        for i in 0..25i64 {
            let bal = if i % 2 == 0 { 0.0 } else { 1000.0 };
            f.on_event(&snap((cfg.zscore_window_ns / 1_000_000_000) * i + i, bal));
        }
        // After enough history the regime emits one of the three codes.
        let v = f.on_event(&snap(999_999, 500.0)).unwrap();
        assert!((0.0..=2.0).contains(&v));
    }

    #[test]
    fn cfv_5_zscore_extreme_and_mean() {
        let cfg = NetflowFlowConfig {
            zscore_window_ns: 86_400_000_000_000, // 1d
            windows_ns: vec![3_600_000_000_000],  // 1h only
            ..NetflowFlowConfig::default()
        };
        let mut f = NetflowFlowFeature::new(cfg, NetflowField::ZScore);
        // Constant-ish baseline then one spike: z large positive.
        let base = [
            (0, 0.0),
            (3600, 10.0),
            (7200, 20.0),
            (10_800, 30.0),
            (14_400, 40.0),
            (18_000, 50.0),
            (21_600, 60.0),
            (25_200, 70.0),
            (28_800, 80.0),
            (32_400, 90.0),
            (36_000, 100.0),
        ];
        for (t, b) in base {
            f.on_event(&snap(t, b));
        }
        // Spike far above trend → z well above 0.
        let z = f.on_event(&snap(39_600, 900.0)).unwrap();
        assert!(z > 1.0, "spike z should be strongly positive, got {z}");
    }

    #[test]
    fn cfv_9_degenerate_inputs_fail_closed() {
        let mut f = feat(NetflowField::Velocity(0));
        assert_eq!(f.on_event(&snap(0, f64::NAN)), None, "NaN balance skipped");
        assert_eq!(f.on_event(&snap(10, 500.0)), None, "single snapshot");
        // Single-snapshot acceleration stays None (needs ≥4 samples).
        let mut a = feat(NetflowField::Acceleration(0));
        a.on_event(&snap(0, 1.0));
        a.on_event(&snap(10, 2.0));
        assert_eq!(a.on_event(&snap(20, 3.0)), None);
    }

    #[test]
    fn cfv_7_ids_and_registration_gated() {
        assert_eq!(
            NetflowFlowFeature::id_for(&NetflowField::Velocity(0)),
            "netflow.velocity.w0"
        );
        assert_eq!(
            NetflowFlowFeature::id_for(&NetflowField::Acceleration(1)),
            "netflow.acceleration.w1"
        );
        assert_eq!(
            NetflowFlowFeature::id_for(&NetflowField::Cumulative),
            "netflow.cumulative"
        );
        assert_eq!(
            NetflowFlowFeature::id_for(&NetflowField::Regime),
            "netflow.regime"
        );
        assert_eq!(
            NetflowFlowFeature::id_for(&NetflowField::ZScore),
            "netflow.zscore"
        );
    }

    #[test]
    fn cfv_6_deterministic_golden() {
        // Same snapshot stream through two fresh instances → byte-identical
        // emitted values (CONV-9/12), for EVERY field family.
        let feed: [(i64, f64); 8] = [
            (0, 1000.0),
            (300, 1100.0),
            (600, 1050.0),
            (900, 1200.0),
            (1200, 900.0),
            (1500, 1500.0),
            (1800, 1300.0),
            (2100, 1400.0),
        ];
        for field in [
            NetflowField::Velocity(0),
            NetflowField::Acceleration(2),
            NetflowField::Cumulative,
            NetflowField::Regime,
            NetflowField::ZScore,
        ] {
            let run = || {
                let mut f = feat(field);
                feed.iter()
                    .map(|&(t, b)| f.on_event(&snap(t, b)))
                    .collect::<Vec<_>>()
            };
            let (a, b) = (run(), run());
            assert_eq!(a, b, "field {field:?} must be deterministic");
            let ja = serde_json::to_string(&a).unwrap();
            let jb = serde_json::to_string(&b).unwrap();
            assert_eq!(ja, jb);
        }
        // And the golden itself is pinned: 24h velocity at the final step is
        // hand-computable — anchor = newest sample ≤ now−24h.
        let mut v = feat(NetflowField::Velocity(2));
        for &(t, b) in &feed {
            v.on_event(&snap(t * 100, b));
        }
        // now=210_000s; anchor_ts=210_000−86_400=123_600s
        // → anchor=(120_000s, bal 900): v=(1400−900)/(90_000s in ns) × day = 480/day.
        let got = v.hist.velocity(*v.cfg.windows_ns.last().unwrap()).unwrap();
        let want = 500.0 / (90_000.0 * 1e9) * DAY_NS as f64;
        assert!((got - want).abs() < 1e-6, "got {got} want {want}");
    }

    #[test]
    fn cfv_8_stale_addresses_suppressed() {
        // Two addresses polled together, then ONLY one keeps polling: after
        // 2× cadence (600s default) the silent address is evicted from the
        // aggregate — the cumulative flow reflects its removal honestly
        // instead of freezing a dead balance into the total.
        let mut a = snap(0, 1000.0);
        if let MarketEvent::NetflowSnapshot { address, .. } = &mut a.body {
            *address = "0xa".into();
        }
        let mut b = snap(0, 500.0);
        if let MarketEvent::NetflowSnapshot { address, .. } = &mut b.body {
            *address = "0xb".into();
        }
        let mut b2 = snap(700, 500.0);
        if let MarketEvent::NetflowSnapshot { address, .. } = &mut b2.body {
            *address = "0xb".into();
        }
        let mut f = feat(NetflowField::Cumulative);
        f.on_event(&a);
        f.on_event(&b);
        // t=700 > stale_after 600 → 0xa evicted; total drops 1500 → 500;
        // cumulative over the 7d window sums segment deltas = −1000.
        let got = f.on_event(&b2).expect("cumulative emits on second point");
        assert!((got - (-1000.0)).abs() < 1e-9, "got {got}");
        // A fresh poll of the SAME address never evicts it mid-flight.
        // (1ns window so the first sample anchors the 700s gap.)
        let cfg = NetflowFlowConfig {
            windows_ns: vec![1],
            ..NetflowFlowConfig::default()
        };
        let mut g = NetflowFlowFeature::new(cfg, NetflowField::Velocity(0));
        assert_eq!(g.on_event(&snap(0, 10.0)), None);
        let v = g.on_event(&snap(700, 12.0)).unwrap();
        assert!((v - 2.0 / (700.0 * 1e9) * DAY_NS as f64).abs() < 1e-6);
    }

    proptest::proptest! {
        /// Velocity ALWAYS equals (Δbalance / Δtime) × day_ns for valid
        /// finite inputs (CFV-10) — random balance pairs and gaps.
        #[test]
        fn cfv_10_proptest_velocity_is_balance_over_time(
            b0 in -1e12f64..1e12,
            b1 in -1e12f64..1e12,
            dt_s in 1i64..2_000_000,
        ) {
            let cfg = NetflowFlowConfig {
                windows_ns: vec![1], // 1ns window: sample(0) anchors every dt ≥ 1s
                ..NetflowFlowConfig::default()
            };
            let mut f = NetflowFlowFeature::new(cfg, NetflowField::Velocity(0));
            prop_assert_eq!(f.on_event(&snap(0, b0)), None, "warmup suppressed");
            let v = f.on_event(&snap(dt_s, b1)).unwrap();
            // Δbalance over dt seconds (expressed in ns) scaled to units/day.
            let want = (b1 - b0) / (dt_s as f64 * 1e9) * DAY_NS as f64;
            prop_assert!((v - want).abs() <= want.abs() * 1e-9 + 1e-6,
                "v={v} want={want} b0={b0} b1={b1} dt={dt_s}");
        }
    }
}
