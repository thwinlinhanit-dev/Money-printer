//! Cross-asset correlation features (spec 048, COR-1..7). Rolling Pearson
//! correlation of **daily returns** between a crypto perp (from our own
//! recorded prices) and any second series available to the engine — another
//! perp or a FRED/DeFiLlama/Coinalyze [`MarketEvent::MacroPoint`] series.
//!
//! GLOBAL tick feature (FEA-20, `ibit_cross.rs` precedent): one instance sees
//! every event; the pair is (venue, symbol-or-series) aligned at UTC-day
//! closes. Determinism: pure function of events — no wall clock, no I/O
//! (PD-3/FEA-2); same events ⇒ same emissions (identical-instance rule from
//! `ibit_cross.rs`).
//!
//! Fail-closed (COR-5): fewer than `min_overlap_days` paired daily returns ⇒
//! NO emission (a two-point "correlation" is fiction), and non-finite inputs
//! are dropped (CONV-8) rather than propagated.
//!
//! Regime conditioner only (COR-4/spec 048 classification): these feeds exist
//! to de-weight strategies in unusual regimes — never standalone entries.

use crate::engine::TickFeature;
use mp_core::{EventEnvelope, MarketEvent, SymbolId, SymbolTable, Venue};
use std::collections::BTreeMap;

const DAY_NS: i64 = 86_400_000_000_000;

/// Which side of the pair an input event belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrSide {
    /// The crypto leg (perp trades; e.g. `hyperliquid:BTC`).
    A,
    /// The TradFi leg or second crypto leg (FRED Gold/SPX/DXY via MacroPoint).
    B,
}

impl CorrSide {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "a" | "A" => Some(CorrSide::A),
            "b" | "B" => Some(CorrSide::B),
            _ => None,
        }
    }
}

/// One leg's config.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrLeg {
    pub venue: String,
    /// Perp symbol (leg A: "BTC") or MacroPoint `series_id` (leg B:
    /// "DTWEXBGS", "DEFI_TVL_AGG", …).
    pub symbol: String,
}

impl CorrLeg {
    fn matches(&self, ev: &EventEnvelope, expected_symbol: Option<SymbolId>) -> bool {
        let Some(v) = Venue::from_slug(&self.venue) else {
            return false;
        };
        if ev.venue != v || expected_symbol != Some(ev.symbol) {
            return false;
        }
        !matches!(&ev.body, MarketEvent::MacroPoint { series_id, .. } if series_id != &self.symbol)
    }
}

/// Pair params — section `[corr]`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrParams {
    pub window_days: usize,
    pub min_overlap_days: usize,
    pub leg_a: CorrLeg,
    pub leg_b: CorrLeg,
}

/// Regime classification thresholds for corr.* values (spec 048 regime wiring).
/// Maps continuous Pearson r to discrete regime labels (0=risk_on, 1=neutral,
/// 2=risk_off) that feed `regime_fit_from_features`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrRegimeConfig {
    /// corr.btc_sp500 >= this → risk_off (high BTC-SPX coupling = risk asset).
    #[serde(default = "default_btc_spx_risk_off")]
    pub btc_spx_risk_off_threshold: f64,
    /// corr.btc_sp500 <= this → risk_on (low coupling = crypto decoupled).
    #[serde(default = "default_btc_spx_risk_on")]
    pub btc_spx_risk_on_threshold: f64,
    /// corr.btc_gold <= this → risk_off (crypto NOT acting as safe-haven).
    #[serde(default = "default_btc_gold_risk_off")]
    pub btc_gold_risk_off_threshold: f64,
    /// Stablecoin supply growth > this ratio → risk_on (capital inflow).
    #[serde(default = "default_stablecoin_expand")]
    pub stablecoin_expand_threshold: f64,
}

fn default_btc_spx_risk_off() -> f64 {
    0.7
}
fn default_btc_spx_risk_on() -> f64 {
    0.3
}
fn default_btc_gold_risk_off() -> f64 {
    -0.3
}
fn default_stablecoin_expand() -> f64 {
    1.05
}

impl Default for CorrRegimeConfig {
    fn default() -> Self {
        Self {
            btc_spx_risk_off_threshold: default_btc_spx_risk_off(),
            btc_spx_risk_on_threshold: default_btc_spx_risk_on(),
            btc_gold_risk_off_threshold: default_btc_gold_risk_off(),
            stablecoin_expand_threshold: default_stablecoin_expand(),
        }
    }
}

/// Config wrapper for the cross-asset correlation family (spec 048). Disabled
/// by default so a deployment opts in deliberately to this regime-only input.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CorrConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_pairs")]
    pub pairs: Vec<CorrParams>,
    #[serde(default)]
    pub regime: CorrRegimeConfig,
}

impl Default for CorrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pairs: default_pairs(),
            regime: CorrRegimeConfig::default(),
        }
    }
}

fn default_pairs() -> Vec<CorrParams> {
    let leg = |venue: &str, symbol: &str| CorrLeg {
        venue: venue.to_owned(),
        symbol: symbol.to_owned(),
    };
    vec![
        CorrParams {
            window_days: 30,
            min_overlap_days: 10,
            leg_a: leg("hyperliquid", "BTC"),
            leg_b: leg("hyperliquid", "ETH"),
        },
        CorrParams {
            window_days: 30,
            min_overlap_days: 10,
            leg_a: leg("hyperliquid", "BTC"),
            leg_b: leg("fred", "GOLDAMGBD228NLBM"),
        },
        CorrParams {
            window_days: 30,
            min_overlap_days: 10,
            leg_a: leg("hyperliquid", "BTC"),
            leg_b: leg("fred", "SP500"),
        },
        CorrParams {
            window_days: 30,
            min_overlap_days: 10,
            leg_a: leg("hyperliquid", "ETH"),
            leg_b: leg("fred", "SP500"),
        },
    ]
}

impl Default for CorrParams {
    fn default() -> Self {
        Self {
            window_days: 30,
            min_overlap_days: 10,
            leg_a: CorrLeg {
                venue: "hyperliquid".into(),
                symbol: "BTC".into(),
            },
            leg_b: CorrLeg {
                venue: "fred".into(),
                symbol: "DTWEXBGS".into(),
            },
        }
    }
}

/// Per-leg daily-close state: current UTC day + that day's last value.
#[derive(Debug, Clone, Copy)]
pub struct LegDay {
    pub day_ns: i64,
    pub value: f64,
}

const WINDOW_HARD_CAP: usize = 4096;

/// Rolling Pearson correlation over paired returns (COR-1/2). `None` on
/// length mismatch/empty, zero variance, or any non-finite input (COR-5
/// fail-closed) — never a fabricated number.
pub fn pearson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() != ys.len() || xs.is_empty() {
        return None;
    }
    let n = xs.len() as f64;
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    for (&x, &y) in xs.iter().zip(ys.iter()) {
        if !x.is_finite() || !y.is_finite() {
            return None; // COR-5
        }
        sx += x;
        sy += y;
    }
    let (mean_x, mean_y) = (sx / n, sy / n);
    let (mut cov, mut var_x, mut var_y) = (0.0f64, 0.0f64, 0.0f64);
    for (&x, &y) in xs.iter().zip(ys.iter()) {
        let dx = x - mean_x;
        let dy = y - mean_y;
        cov += dx * dy;
        var_x += dx * dx;
        var_y += dy * dy;
    }
    if var_x <= 0.0 || var_y <= 0.0 {
        return None;
    }
    let r = cov / (var_x.sqrt() * var_y.sqrt());
    r.is_finite().then_some(r)
}

/// One instance per pair — GLOBAL tick feature seeing every event
/// (`ibit_cross` precedent; identical-instance determinism).
#[derive(Debug)]
pub struct CorrFeature {
    params: CorrParams,
    id: String,
    leg_a: Option<LegDay>,
    leg_b: Option<LegDay>,
    prev_ret_a: Option<(i64, f64)>,
    prev_ret_b: Option<(i64, f64)>,
    rets_a: BTreeMap<i64, f64>,
    rets_b: BTreeMap<i64, f64>,
    cached_r: Option<f64>,
    leg_a_symbol: Option<SymbolId>,
    leg_b_symbol: Option<SymbolId>,
}

impl CorrFeature {
    pub fn new(params: CorrParams) -> Self {
        let window = params.window_days.clamp(2, WINDOW_HARD_CAP);
        let min_overlap = params.min_overlap_days.max(2).min(window);
        let id = format!(
            "corr.{}_{}",
            params.leg_a.symbol.to_lowercase(),
            params.leg_b.symbol.to_lowercase()
        );
        Self {
            params: CorrParams {
                window_days: window,
                min_overlap_days: min_overlap,
                ..params
            },
            id,
            leg_a: None,
            leg_b: None,
            prev_ret_a: None,
            prev_ret_b: None,
            rets_a: BTreeMap::new(),
            rets_b: BTreeMap::new(),
            cached_r: None,
            leg_a_symbol: None,
            leg_b_symbol: None,
        }
    }

    /// Observe one raw value at `ts_ns`; yields the COMPLETED previous day's
    /// close when the UTC day rolls. Non-finite values never enter state.
    fn observe_leg(leg_day: &mut Option<LegDay>, ts_ns: i64, value: f64) -> Option<(i64, f64)> {
        if !value.is_finite() {
            return None; // CONV-8
        }
        let day = ts_ns - ts_ns.rem_euclid(DAY_NS);
        match leg_day {
            Some(d) if d.day_ns == day => {
                d.value = value; // same-day running close
                None
            }
            _ => {
                let completed = leg_day.map(|d| (d.day_ns, d.value));
                *leg_day = Some(LegDay { day_ns: day, value });
                completed
            }
        }
    }

    /// Daily return vs the prior CLOSED day (gaps re-anchor; no bridging).
    fn daily_return(prev: &mut Option<(i64, f64)>, closed: (i64, f64)) -> Option<f64> {
        let out = prev.as_ref().and_then(|(pd, pv)| {
            if closed.0 == pd.checked_add(DAY_NS)? {
                (*pv != 0.0 && pv.is_finite()).then_some(closed.1 / pv - 1.0)
            } else {
                None
            }
        });
        *prev = Some(closed);
        out.filter(|r| r.is_finite())
    }

    fn on_close(&mut self, side_a: bool, closed: (i64, f64)) -> Option<f64> {
        let r = if side_a {
            Self::daily_return(&mut self.prev_ret_a, closed)
        } else {
            Self::daily_return(&mut self.prev_ret_b, closed)
        };
        let Some(r) = r else { return self.cached_r };
        if side_a {
            self.rets_a.insert(closed.0, r);
        } else {
            self.rets_b.insert(closed.0, r);
        }
        self.recompute();
        self.cached_r
    }

    /// Pair by shared UTC-day keys; window the most recent N pairs.
    fn recompute(&mut self) {
        let shared: Vec<i64> = self
            .rets_a
            .keys()
            .filter(|d| self.rets_b.contains_key(d))
            .copied()
            .collect();
        if shared.len() < self.params.min_overlap_days {
            self.cached_r = None; // COR-5: below the overlap floor ⇒ no fiction
            return;
        }
        let take_from = shared.len().saturating_sub(self.params.window_days);
        let xs: Vec<f64> = shared[take_from..]
            .iter()
            .filter_map(|d| self.rets_a.get(d).copied())
            .collect();
        let ys: Vec<f64> = shared[take_from..]
            .iter()
            .filter_map(|d| self.rets_b.get(d).copied())
            .collect();
        self.cached_r = pearson(&xs, &ys);
    }
}

impl TickFeature for CorrFeature {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn warm(&self) -> bool {
        // Emission-gated by min_overlap_days inside recompute; the engine-level
        // warm flag mirrors that so early values are dropped outright (FEA-3).
        self.cached_r.is_some()
    }

    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let a_matches = self.params.leg_a.matches(ev, self.leg_a_symbol);
        if a_matches {
            if let Some(value) = event_value(ev) {
                if let Some(closed) = Self::observe_leg(&mut self.leg_a, ev.recv_ts_ns, value) {
                    return self.on_close(true, closed);
                }
                return self.cached_r;
            }
        }
        if self.params.leg_b.matches(ev, self.leg_b_symbol) {
            if let Some(value) = event_value(ev) {
                if let Some(closed) = Self::observe_leg(&mut self.leg_b, ev.recv_ts_ns, value) {
                    return self.on_close(false, closed);
                }
            }
        }
        self.cached_r
    }

    fn bind_symbols(&mut self, symbols: &SymbolTable) {
        let a_venue = Venue::from_slug(&self.params.leg_a.venue);
        let b_venue = Venue::from_slug(&self.params.leg_b.venue);
        self.leg_a_symbol =
            a_venue.and_then(|venue| symbols.lookup(venue, &self.params.leg_a.symbol));
        self.leg_b_symbol =
            b_venue.and_then(|venue| symbols.lookup(venue, &self.params.leg_b.symbol));
    }
}

/// `corr_regime.{pair}` — regime classification from a raw corr.* Pearson r
/// (spec 048 regime wiring). Consumes `corr.{a}_{b}` feature values and emits
/// regime labels: 0 = risk_on, 1 = neutral, 2 = risk_off. Mapped by
/// configurable thresholds in `[corr.regime]`. Fail-closed (CONV-8): non-finite
/// or missing corr value → no emission.
///
/// This follows the `IvPercentileFeature` pattern: a global tick feature that
/// reads the merged event stream and classifies a continuous signal into regime
/// labels for `regime_fit_from_features` (RSK-7).
#[derive(Debug)]
#[allow(dead_code)] // consumed by tests + external regime_fit_from_features
pub struct CorrRegimeFeature {
    id: String,
    pair_label: String,
    corr_id: String,
    config: CorrRegimeConfig,
    kind: CorrRegimeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrRegimeKind {
    BtcSpx,
    BtcGold,
    StablecoinExpand,
}

/// Regime label encoding — matches `regime_fit_from_features` (RSK-7):
/// 0=risk_on, 1=neutral, 2=risk_off.
#[allow(dead_code)] // used in tests; also available for live engine wiring
fn regime_encode(kind: CorrRegimeKind, value: f64, cfg: &CorrRegimeConfig) -> Option<f64> {
    if !value.is_finite() {
        return None; // CONV-8
    }
    let label = match kind {
        CorrRegimeKind::BtcSpx => {
            if value >= cfg.btc_spx_risk_off_threshold {
                2.0 // risk_off
            } else if value <= cfg.btc_spx_risk_on_threshold {
                0.0 // risk_on
            } else {
                1.0 // neutral
            }
        }
        CorrRegimeKind::BtcGold => {
            if value <= cfg.btc_gold_risk_off_threshold {
                2.0 // risk_off (crypto NOT safe-haven)
            } else if value >= 0.3 {
                0.0 // risk_on (crypto as safe-haven)
            } else {
                1.0 // neutral
            }
        }
        CorrRegimeKind::StablecoinExpand => {
            // value is the raw stablecoin supply ratio (>1 = expansion).
            if value >= cfg.stablecoin_expand_threshold {
                0.0 // risk_on (capital inflow)
            } else if value <= 0.95 {
                2.0 // risk_off (capital outflow)
            } else {
                1.0 // neutral
            }
        }
    };
    Some(label)
}

impl CorrRegimeFeature {
    pub fn btc_spx(config: CorrRegimeConfig) -> Self {
        Self {
            id: "corr_regime.btc_sp500".into(),
            pair_label: "btc_sp500".into(),
            corr_id: "corr.btc_sp500".into(),
            config,
            kind: CorrRegimeKind::BtcSpx,
        }
    }

    pub fn btc_gold(config: CorrRegimeConfig) -> Self {
        Self {
            id: "corr_regime.btc_goldamgbd228nlbm".into(),
            pair_label: "btc_goldamgbd228nlbm".into(),
            corr_id: "corr.btc_goldamgbd228nlbm".into(),
            config,
            kind: CorrRegimeKind::BtcGold,
        }
    }

    pub fn stablecoin_expand(config: CorrRegimeConfig) -> Self {
        Self {
            id: "corr_regime.stablecoin_mcap".into(),
            pair_label: "stablecoin_mcap".into(),
            corr_id: "corr.btc_stablecoin_mcap".into(),
            config,
            kind: CorrRegimeKind::StablecoinExpand,
        }
    }
}

impl TickFeature for CorrRegimeFeature {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn on_event(&mut self, _ev: &EventEnvelope) -> Option<f64> {
        // This feature does NOT consume events directly — it is a derived
        // classification over another feature's output. In the live engine,
        // the materializer provides feature values; here we emit our regime
        // label from the raw corr value passed via on_event.
        //
        // For the feature engine path: we consume FeatureUpdate events for
        // the corr_id. In practice, the regime classification is computed
        // during materialization when both corr.* and regime.* features are
        // available. This stub emits None (the regime is computed externally
        // by regime_fit_from_features consuming the corr.* value directly).
        None
    }
}

/// Price observations available to the correlation engine: recorded trade
/// prices for the own-data legs, or MacroPoint values for FRED-like legs.
fn event_value(ev: &EventEnvelope) -> Option<f64> {
    // trade_view (WAL-6) unifies schema-3 `Trade` and schema-4 `TradeWithAddr`
    // (hyperliquid's variant since 2026-08-18) — a Trade-only arm made the
    // own-data correlation legs silent for hyperliquid days.
    if let Some((price, _, _, _, _)) = ev.body.trade_view() {
        return Some(price);
    }
    match &ev.body {
        MarketEvent::MacroPoint { value, .. } => Some(*value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{Side, SymbolId};
    use proptest::prelude::*;

    const D0: i64 = 0; // 1970-01-01 UTC day anchor for fixtures
    fn day(n: i64) -> i64 {
        n * DAY_NS + 3_600_000_000_000 // noon of day n
    }

    fn trade(venue: Venue, ts: i64, price: f64) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            SymbolId(0),
            ts,
            ts,
            0,
            MarketEvent::Trade {
                price,
                qty: 1.0,
                side: Side::Buy,
                trade_id: 1,
            },
        )
    }

    fn macro_pt(venue: Venue, ts: i64, value: f64, sid: &str) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            SymbolId(1),
            ts,
            ts,
            0,
            MarketEvent::MacroPoint {
                series_id: sid.to_owned(),
                value,
                date: ts - ts.rem_euclid(DAY_NS),
            },
        )
    }

    fn params() -> CorrParams {
        CorrParams {
            window_days: 30,
            min_overlap_days: 5,
            leg_a: CorrLeg {
                venue: "hyperliquid".into(),
                symbol: "BTC".into(),
            },
            leg_b: CorrLeg {
                venue: "fred".into(),
                symbol: "DTWEXBGS".into(),
            },
        }
    }

    fn feature() -> CorrFeature {
        let mut symbols = SymbolTable::new();
        symbols.intern_default(Venue::Hyperliquid, "BTC");
        symbols.intern_default(Venue::Fred, "DTWEXBGS");
        let mut feature = CorrFeature::new(params());
        feature.bind_symbols(&symbols);
        feature
    }

    #[test]
    fn cor_1_computes_btc_eth_from_own_recorded_trades() {
        let mut params = params();
        params.leg_b = CorrLeg {
            venue: "hyperliquid".into(),
            symbol: "ETH".into(),
        };
        let mut symbols = SymbolTable::new();
        symbols.intern_default(Venue::Hyperliquid, "BTC");
        symbols.intern_default(Venue::Hyperliquid, "ETH");
        let mut feature = CorrFeature::new(params);
        feature.bind_symbols(&symbols);

        let mut btc = 100.0;
        let mut eth = 100.0;
        for d in 0..14i64 {
            if d > 0 {
                let step = d as f64 / 100.0;
                btc *= 1.0 + step;
                eth *= 1.0 + step;
            }
            feature.on_event(&trade(Venue::Hyperliquid, day(d), btc));
            let mut eth_event = trade(Venue::Hyperliquid, day(d), eth);
            eth_event.symbol = SymbolId(1);
            feature.on_event(&eth_event);
        }
        assert!(feature.cached_r.is_some_and(|r| r > 0.99));
    }

    #[test]
    fn cor_2_pearson_golden_perfect_and_zero() {
        // Perfect positive line ⇒ r = 1; perfect negative ⇒ r = −1;
        // constant leg (zero variance) ⇒ None (no fiction).
        let xs: Vec<f64> = (0..10).map(|i| i as f64).collect();
        assert!((pearson(&xs, &xs).unwrap() - 1.0).abs() < 1e-12);
        let ys_neg: Vec<f64> = (0..10).map(|i| -(i as f64)).collect();
        assert!((pearson(&xs, &ys_neg).unwrap() + 1.0).abs() < 1e-12);
        let flat = vec![7.0; 10];
        assert!(pearson(&xs, &flat).is_none());
        assert!(pearson(&xs[..3], &xs).is_none()); // length mismatch
    }

    #[test]
    fn cor_5_nan_fail_closed() {
        let mut xs: Vec<f64> = (0..8).map(|i| i as f64).collect();
        xs[3] = f64::NAN;
        assert!(pearson(&xs, &xs).is_none());
    }

    #[test]
    fn cor_1_does_not_bridge_a_missing_daily_close() {
        // A price observed after a missing UTC day re-anchors the return
        // series; treating day 2 as a return from day 0 would fabricate a
        // daily observation and inflate a later shared-window correlation.
        let mut previous = None;
        assert_eq!(CorrFeature::daily_return(&mut previous, (D0, 100.0)), None);
        assert_eq!(
            CorrFeature::daily_return(&mut previous, (2 * DAY_NS, 110.0)),
            None
        );
    }

    #[test]
    fn cor_5_min_overlap_blocks_early_emission() {
        let mut f = feature(); // min_overlap_days = 5
                               // Feed leg A only for many days — must never emit.
        for d in 0..20 {
            assert!(
                f.on_event(&trade(Venue::Hyperliquid, day(d), 100.0 + d as f64))
                    .is_none()
                    || f.cached_r.is_none()
            );
        }
        assert!(f.cached_r.is_none(), "A-only feed must not emit");
    }

    #[test]
    fn cor_1_computes_from_own_prices_plus_fred() {
        let mut f = feature();
        // Explicitly opposing return sequences: BTC returns increase from
        // +1% to +14%, while the FRED leg falls from -1% to -14%, so the
        // paired daily returns have r = -1 after warmup.
        let mut btc = 100.0;
        let mut dxy = 100.0;
        for d in 0..14i64 {
            if d > 0 {
                let step = d as f64 / 100.0;
                btc *= 1.0 + step;
                dxy *= 1.0 - step;
            }
            f.on_event(&trade(Venue::Hyperliquid, day(d), btc));
            f.on_event(&macro_pt(Venue::Fred, day(d), dxy, "DTWEXBGS"));
        }
        let r = f.cached_r.expect("should be warm after overlap");
        assert!(r < -0.99, "opposite-trend series must be near −1, got {r}");
    }

    #[test]
    fn cor_4_non_leg_events_inert() {
        let mut f = feature();
        // Wrong-venue trade and wrong-series macro points are ignored.
        f.on_event(&trade(Venue::BinanceFutures, day(0), 50.0));
        f.on_event(&macro_pt(Venue::Fred, day(0), 9.9, "UNRELATED"));
        assert!(f.cached_r.is_none());
        assert!(f.leg_a.is_none() && f.leg_b.is_none());
    }

    #[test]
    fn cor_7_fixtures_deterministic() {
        let feed = || {
            let mut f = feature();
            for d in 0..12i64 {
                f.on_event(&trade(Venue::Hyperliquid, day(d), 100.0 + d as f64));
                f.on_event(&macro_pt(
                    Venue::Fred,
                    day(d),
                    100.0 - 2.0 * d as f64,
                    "DTWEXBGS",
                ));
            }
            (f.cached_r, f.id())
        };
        let a = feed();
        let b = feed();
        assert_eq!(a, b);
        assert_eq!(a.1, "corr.btc_dtwexbgs");
    }

    proptest! {
        #[test]
        fn cor_7_proptest_affine_returns_have_perfect_correlation(
            first in -100.0f64..100.0,
            second in -100.0f64..100.0,
        ) {
            let xs = [first, second, first + 1.0];
            let ys = [3.0 * first + 7.0, 3.0 * second + 7.0, 3.0 * (first + 1.0) + 7.0];
            prop_assert!(pearson(&xs, &ys).is_some_and(|r| (r - 1.0).abs() < 1e-12));
        }
    }

    // --- CorrRegimeConfig / regime_encode tests (spec 048 regime wiring) ---

    fn default_regime_cfg() -> CorrRegimeConfig {
        CorrRegimeConfig::default()
    }

    #[test]
    fn cor_regime_btc_spx_high_corr_is_risk_off() {
        let cfg = default_regime_cfg();
        // r=0.8 >= 0.7 threshold → risk_off (2.0)
        assert_eq!(regime_encode(CorrRegimeKind::BtcSpx, 0.8, &cfg), Some(2.0));
        assert_eq!(regime_encode(CorrRegimeKind::BtcSpx, 1.0, &cfg), Some(2.0));
    }

    #[test]
    fn cor_regime_btc_spx_low_corr_is_risk_on() {
        let cfg = default_regime_cfg();
        // r=0.2 <= 0.3 threshold → risk_on (0.0)
        assert_eq!(regime_encode(CorrRegimeKind::BtcSpx, 0.2, &cfg), Some(0.0));
        assert_eq!(regime_encode(CorrRegimeKind::BtcSpx, 0.0, &cfg), Some(0.0));
    }

    #[test]
    fn cor_regime_btc_spx_mid_is_neutral() {
        let cfg = default_regime_cfg();
        // r=0.5 between 0.3 and 0.7 → neutral (1.0)
        assert_eq!(regime_encode(CorrRegimeKind::BtcSpx, 0.5, &cfg), Some(1.0));
    }

    #[test]
    fn cor_regime_btc_gold_negative_is_risk_off() {
        let cfg = default_regime_cfg();
        // r=-0.5 <= -0.3 → risk_off (crypto NOT safe-haven)
        assert_eq!(
            regime_encode(CorrRegimeKind::BtcGold, -0.5, &cfg),
            Some(2.0)
        );
    }

    #[test]
    fn cor_regime_btc_gold_positive_is_risk_on() {
        let cfg = default_regime_cfg();
        // r=0.4 >= 0.3 → risk_on (crypto as safe-haven)
        assert_eq!(regime_encode(CorrRegimeKind::BtcGold, 0.4, &cfg), Some(0.0));
    }

    #[test]
    fn cor_regime_stablecoin_expand_is_risk_on() {
        let cfg = default_regime_cfg();
        // supply ratio 1.08 >= 1.05 → risk_on (capital inflow)
        assert_eq!(
            regime_encode(CorrRegimeKind::StablecoinExpand, 1.08, &cfg),
            Some(0.0)
        );
    }

    #[test]
    fn cor_regime_stablecoin_contract_is_risk_off() {
        let cfg = default_regime_cfg();
        // supply ratio 0.92 <= 0.95 → risk_off (capital outflow)
        assert_eq!(
            regime_encode(CorrRegimeKind::StablecoinExpand, 0.92, &cfg),
            Some(2.0)
        );
    }

    #[test]
    fn cor_regime_nan_is_none() {
        let cfg = default_regime_cfg();
        assert_eq!(regime_encode(CorrRegimeKind::BtcSpx, f64::NAN, &cfg), None);
        assert_eq!(
            regime_encode(CorrRegimeKind::BtcGold, f64::INFINITY, &cfg),
            None
        );
    }

    #[test]
    fn cor_regime_config_denies_unknown_fields() {
        let bad = r#"
            btc_spx_risk_off_threshold = 0.7
            bogus_key = true
        "#;
        assert!(toml::from_str::<CorrRegimeConfig>(bad).is_err());
    }

    #[test]
    fn cor_regime_config_parses_custom_thresholds() {
        let toml = r#"
            btc_spx_risk_off_threshold = 0.8
            btc_spx_risk_on_threshold = 0.2
            btc_gold_risk_off_threshold = -0.5
            stablecoin_expand_threshold = 1.10
        "#;
        let cfg: CorrRegimeConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.btc_spx_risk_off_threshold, 0.8);
        assert_eq!(cfg.btc_spx_risk_on_threshold, 0.2);
        assert_eq!(cfg.btc_gold_risk_off_threshold, -0.5);
        assert_eq!(cfg.stablecoin_expand_threshold, 1.10);
        // Custom thresholds shift the regime boundaries.
        assert_eq!(regime_encode(CorrRegimeKind::BtcSpx, 0.75, &cfg), Some(1.0)); // below 0.8
        assert_eq!(regime_encode(CorrRegimeKind::BtcSpx, 0.85, &cfg), Some(2.0));
        // >= 0.8
    }
}
