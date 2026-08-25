//! OI/price regime inputs for the accumulation detector (spec 045
//! §Sub-Signal Definitions): `oi.delta.{w}`, `oi.quadrant.{w}` and
//! `regime.trend` — the three feature families the detector consumes that
//! the raw catalog did not yet provide.
//!
//! * [`OiLevelDelta`] / [`OiQuadrant`] consume spec-004 `OpenInterest`
//!   events (ABSOLUTE levels) plus last-trade price; windows are endpoint-
//!   anchored (newest sample ≤ now−window), the honest as-of pattern used
//!   by netflow velocity. Quadrants follow the standard model the spec
//!   fixes: 1=↑px↑OI new longs, 2=↑px↓OI short covering, 3=↓px↓OI long
//!   flush, 4=↓px↑OI new shorts. Zero moves emit None — no fabricated
//!   classification at an inflection point.
//! * [`TrendRegime`] is a BAR feature reusing swing's pure
//!   [`trend_strength`](crate::swing::trend_strength) efficiency ratio:
//!   |strength| ≥ threshold ⇒ 0.0 (Trend) else 1.0 (Chop) — encoding fixed
//!   by the detector's `trend_values = [0.0]` default.
//!
//! Deterministic (PD-3): BTreeMap-free per-symbol state, no wall clock,
//! fail-closed warmups (FEA-3).

use crate::bar::Bar;
use crate::engine::{BarFeature, TickFeature};
use mp_core::{EventEnvelope, MarketEvent};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Endpoint anchor: newest `(ts, value)` at or before `ts_ns - window_ns`.
fn anchored(series: &VecDeque<(i64, f64)>, ts_ns: i64, window_ns: i64) -> Option<f64> {
    let cut = ts_ns.saturating_sub(window_ns);
    let mut found = None;
    for &(t, v) in series {
        if t <= cut {
            found = Some(v);
        } else {
            break;
        }
    }
    found
}

/// Window label: whole hours → "4h", else raw ns (same convention as
/// cohort smart_flow).
pub(crate) fn window_label(window_ns: i64) -> String {
    if window_ns % 3_600_000_000_000 == 0 && window_ns >= 3_600_000_000_000 {
        format!("{}h", window_ns / 3_600_000_000_000)
    } else {
        format!("{window_ns}ns")
    }
}

const MAX_SAMPLES: usize = 4096;

/// Shared per-symbol series: absolute OI levels + reference prices.
#[derive(Debug, Default)]
struct Series {
    oi: VecDeque<(i64, f64)>,
    price: VecDeque<(i64, f64)>,
}

impl Series {
    fn push_oi(&mut self, ts: i64, level: f64) {
        if !level.is_finite() || level < 0.0 {
            return; // CONV-8: corrupt frame never enters the series
        }
        self.oi.push_back((ts, level));
        while self.oi.len() > MAX_SAMPLES {
            self.oi.pop_front();
        }
    }

    fn push_price(&mut self, ts: i64, px: f64) {
        if !px.is_finite() || px <= 0.0 {
            return;
        }
        self.price.push_back((ts, px));
        while self.price.len() > MAX_SAMPLES {
            self.price.pop_front();
        }
    }
}

/// Config (`[oi_regime]`, CONV-16 deny_unknown_fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OiRegimeConfig {
    /// Analysis windows in ns (default [4h] → `oi.delta.4h`,
    /// `oi.quadrant.4h` — the detector's consumption defaults).
    #[serde(default = "default_windows_ns")]
    pub windows_ns: Vec<i64>,
    /// Bar lookback for the trend efficiency ratio (default 20, matching
    /// the materialized swing.trend_strength.20 family).
    #[serde(default = "default_trend_lookback")]
    pub trend_lookback_bars: usize,
    /// |efficiency ratio| at or above this ⇒ Trend (0.0); else Chop (1.0).
    #[serde(default = "default_trend_threshold")]
    pub trend_threshold: f64,
}

fn default_windows_ns() -> Vec<i64> {
    vec![4 * 3_600_000_000_000]
}
fn default_trend_lookback() -> usize {
    20
}
fn default_trend_threshold() -> f64 {
    0.25
}

impl Default for OiRegimeConfig {
    fn default() -> Self {
        Self {
            windows_ns: default_windows_ns(),
            trend_lookback_bars: default_trend_lookback(),
            trend_threshold: default_trend_threshold(),
        }
    }
}

/// `oi.delta.{w}` — net OI change over the trailing window, emitted on each
/// OpenInterest event once a same-window anchor exists (warmup suppressed,
/// FEA-3).
pub struct OiLevelDelta {
    window_ns: i64,
    series: Series,
}

impl OiLevelDelta {
    pub fn new(window_ns: i64) -> Self {
        Self {
            window_ns,
            series: Series::default(),
        }
    }
}

impl TickFeature for OiLevelDelta {
    fn id(&self) -> String {
        format!("oi.delta.{}", window_label(self.window_ns))
    }

    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let MarketEvent::OpenInterest { oi_contracts, .. } = &ev.body else {
            return None;
        };
        self.series.push_oi(ev.recv_ts_ns, *oi_contracts);
        let now = self.series.oi.back().map(|&(_, v)| v)?;
        let anchor = anchored(&self.series.oi, ev.recv_ts_ns, self.window_ns)?;
        Some(now - anchor)
    }
}

/// `oi.quadrant.{w}` — OI×price quadrant over the trailing window
/// (spec 045 §1: 1=new longs, 2=short covering; 3/4 complete the standard
/// model). Price reference = last trade print (MarkPrice fills the gap on
/// venues without trades). Requires BOTH anchors and strictly-signed moves;
/// otherwise None.
pub struct OiQuadrant {
    window_ns: i64,
    series: Series,
}

impl OiQuadrant {
    pub fn new(window_ns: i64) -> Self {
        Self {
            window_ns,
            series: Series::default(),
        }
    }
}

impl TickFeature for OiQuadrant {
    fn id(&self) -> String {
        format!("oi.quadrant.{}", window_label(self.window_ns))
    }

    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        match &ev.body {
            MarketEvent::OpenInterest { oi_contracts, .. } => {
                self.series.push_oi(ev.recv_ts_ns, *oi_contracts);
            }
            MarketEvent::Trade { price, .. } => {
                self.series.push_price(ev.recv_ts_ns, *price);
            }
            MarketEvent::MarkPrice { mark, .. } => {
                self.series.push_price(ev.recv_ts_ns, *mark);
            }
            _ => return None,
        }
        // Evaluate on OI events only (the quadrant clock follows OI cadence).
        let MarketEvent::OpenInterest { .. } = &ev.body else {
            return None;
        };
        let now_oi = self.series.oi.back()?.1;
        let now_px = self.series.price.back()?.1;
        let anchor_oi = anchored(&self.series.oi, ev.recv_ts_ns, self.window_ns)?;
        let anchor_px = anchored(&self.series.price, ev.recv_ts_ns, self.window_ns)?;
        let doi = now_oi - anchor_oi;
        let dpx = now_px - anchor_px;
        match (
            dpx.is_sign_positive(),
            dpx != 0.0,
            doi.is_sign_positive(),
            doi != 0.0,
        ) {
            (true, true, true, true) => Some(1.0),
            (true, true, false, true) => Some(2.0),
            (false, true, false, true) => Some(3.0),
            (false, true, true, true) => Some(4.0),
            _ => None, // zero move on either leg — no classification
        }
    }
}

/// `regime.trend` — bar-close HTF trend regime from swing's efficiency
/// ratio: 0.0 = Trend (|strength| ≥ threshold), 1.0 = Chop.
pub struct TrendRegime {
    lookback: usize,
    threshold: f64,
    closes: VecDeque<f64>,
}

impl TrendRegime {
    pub fn new(lookback: usize, threshold: f64) -> Self {
        Self {
            lookback: lookback.max(1),
            threshold,
            closes: VecDeque::new(),
        }
    }
}

impl BarFeature for TrendRegime {
    fn id(&self) -> String {
        "regime.trend".into()
    }

    fn warm(&self) -> bool {
        self.closes.len() > self.lookback
    }

    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.closes.push_back(bar.close);
        while self.closes.len() > self.lookback + 1 {
            self.closes.pop_front();
        }
        let closes: Vec<f64> = self.closes.iter().copied().collect();
        let strength = crate::swing::trend_strength(&closes, self.lookback)?;
        Some(if strength.abs() >= self.threshold {
            0.0
        } else {
            1.0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{Side, SymbolId, Venue};

    const HOUR_NS: i64 = 3_600_000_000_000;
    const HOUR_S: i64 = 3_600;

    fn env(ts_s: i64, body: MarketEvent) -> EventEnvelope {
        EventEnvelope::new(
            Venue::Bybit,
            SymbolId(2),
            ts_s * 1_000_000_000,
            ts_s * 1_000_000_000,
            1,
            body,
        )
    }

    fn oi(ts_s: i64, level: f64) -> EventEnvelope {
        env(
            ts_s,
            MarketEvent::OpenInterest {
                oi_contracts: level,
                oi_notional: f64::NAN,
            },
        )
    }

    fn trade(ts_s: i64, px: f64) -> EventEnvelope {
        env(
            ts_s,
            MarketEvent::Trade {
                price: px,
                qty: 1.0,
                side: Side::Buy,
                trade_id: 1,
            },
        )
    }

    #[test]
    fn acc_sig1_oi_delta_window_endpoints() {
        let mut f = OiLevelDelta::new(HOUR_NS);
        assert_eq!(f.on_event(&oi(0, 1000.0)), None, "warmup");
        assert_eq!(f.on_event(&oi(1_800, 1100.0)), None, "no anchor yet");
        assert_eq!(f.on_event(&oi(3_600, 1200.0)), Some(200.0));
        // Negative delta flows through honestly.
        assert_eq!(f.on_event(&oi(7_200, 900.0)), Some(-300.0));
        // Corrupt frames never enter the series; the last VALID reading
        // remains the reference, so the honest delta vs its own anchor is 0.
        assert_eq!(f.on_event(&oi(10_800, f64::NAN)), Some(0.0));
    }

    #[test]
    fn acc_sig1_quadrants_match_spec_model() {
        // Q1 ↑px↑OI new longs → 1; Q2 ↑px↓OI short covering → 2;
        // Q3 ↓px↓OI long flush → 3; Q4 ↓px↑OI new shorts → 4.
        let cases: [(f64, f64, f64, f64, f64); 4] = [
            (100.0, 110.0, 1_000.0, 1_200.0, 1.0), // px+ oi+
            (100.0, 110.0, 1_000.0, 800.0, 2.0),   // px+ oi-
            (100.0, 90.0, 1_000.0, 800.0, 3.0),    // px- oi-
            (100.0, 90.0, 1_000.0, 1_200.0, 4.0),  // px- oi+
        ];
        for (p0, p1, o0, o1, want) in cases {
            let mut f = OiQuadrant::new(HOUR_NS);
            f.on_event(&trade(0, p0));
            f.on_event(&oi(0, o0));
            assert_eq!(
                f.on_event(&trade(HOUR_S + 60, p1)),
                None,
                "evaluates on OI events"
            );
            assert_eq!(
                f.on_event(&oi(HOUR_S + 60, o1)),
                Some(want),
                "p{p0}->{p1} oi{o0}->{o1}"
            );
        }
    }

    #[test]
    fn acc_sig1_quadrant_zero_move_is_suppressed() {
        // Flat price over the window → no quadrant (no fabricated signal).
        let mut f = OiQuadrant::new(HOUR_NS);
        f.on_event(&trade(0, 100.0));
        f.on_event(&oi(0, 1_000.0));
        assert_eq!(f.on_event(&oi(2 * HOUR_S, 1_500.0)), None);
    }

    #[test]
    fn acc_sig3_trend_regime_encoding_matches_detector_default() {
        // Detector consumes trend_values=[0.0] ⇒ 0 MUST mean Trend.
        // Rising closes → high efficiency → Trend(0.0).
        let mut up = TrendRegime::new(5, 0.25);
        let mk = |close: f64| Bar {
            open: close,
            high: close,
            low: close,
            close,
            vol: 0.0,
            buy_vol: 0.0,
            sell_vol: 0.0,
            vwap: close,
            n_trades: 1,
            first_ts_ns: 0,
            last_ts_ns: 0,
            close_ts_ns: 0,
        };
        for k in 0..6i64 {
            let r = up.on_bar(&mk(100.0 + 10.0 * k as f64));
            if k == 5 {
                assert_eq!(r, Some(0.0), "clean uptrend ⇒ Trend");
            }
        }
        // Alternating closes → near-zero efficiency → Chop(1.0). The
        // sawtooth's efficiency is exactly 1/4 at lookback 5, so use a
        // threshold strictly above it to prove the Chop side of the gate.
        let mut chop = TrendRegime::new(5, 0.3);
        for k in 0..6i64 {
            let r = chop.on_bar(&mk(if k % 2 == 0 { 100.0 } else { 101.0 }));
            if k == 5 {
                assert_eq!(r, Some(1.0), "sawtooth ⇒ Chop");
            }
        }
    }
}

#[cfg(test)]
mod wiring_tests {
    use crate::config::FeaturesConfig;
    use crate::engine_from_config;
    use mp_core::{EventEnvelope, MarketEvent, SymbolId, Venue};

    fn env(ts_s: i64, body: MarketEvent) -> EventEnvelope {
        EventEnvelope::new(
            Venue::Hyperliquid,
            SymbolId(3),
            ts_s * 1_000_000_000,
            ts_s * 1_000_000_000,
            1,
            body,
        )
    }

    #[test]
    fn engine_path_emits_oi_delta_and_quadrant_names() {
        let mut cfg = FeaturesConfig::default();
        cfg.oi_regime.enabled = true;
        let mut e = engine_from_config(&cfg).expect("build");
        // Warm price + OI, then advance past the 4h window.
        e.on_event(&env(
            0,
            MarketEvent::Trade {
                price: 100.0,
                qty: 1.0,
                side: mp_core::Side::Buy,
                trade_id: 1,
            },
        ));
        e.on_event(&env(
            0,
            MarketEvent::OpenInterest {
                oi_contracts: 1000.0,
                oi_notional: f64::NAN,
            },
        ));
        e.on_event(&env(
            3600 * 5,
            MarketEvent::Trade {
                price: 110.0,
                qty: 1.0,
                side: mp_core::Side::Buy,
                trade_id: 2,
            },
        ));
        let ups = e.on_event(&env(
            3600 * 5,
            MarketEvent::OpenInterest {
                oi_contracts: 1200.0,
                oi_notional: f64::NAN,
            },
        ));
        let names: Vec<String> = ups.iter().map(|u| u.name.clone()).collect();
        assert!(
            names.iter().any(|n| n == "oi.delta.4h"),
            "oi.delta.4h missing from {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "oi.quadrant.4h"),
            "oi.quadrant.4h missing from {names:?}"
        );
    }
}
