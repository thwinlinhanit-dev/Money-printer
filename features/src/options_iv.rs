//! IV surface builder & volatility analytics (spec 038, IVS-1..12).
//!
//! Turns per-contract `mark_iv` from OptionTicker into structural vol
//! features: ATM IV (interpolated at moneyness = 1.0, IVS-2), term structure
//! with nearest-available-expiry fallback (IVS-3), 25Δ risk reversal +
//! wing richness (IVS-4), OI-weighted DVOL-style vol index (IVS-7), and a
//! rolling-percentile vol regime classifier (IVS-6, reverse-ordinal encoding
//! 0=Rich/1=Fair/2=Cheap — see spec 038 warning). VRP is a pure function
//! (`vrp`) per the spec 038 v1 wiring decision.
//!
//! Pure function of events (PD-3): time references come exclusively from
//! envelope timestamps; iteration is sorted (CONV-10); non-finite inputs are
//! skipped fail-closed (CONV-8).

use crate::options_greeks::{ChainMap, ContractKey, NS_PER_DAY, TickerSnap};
use mp_core::event::{EventEnvelope, OptionKind};
use std::collections::BTreeMap;

/// Volatility risk premium: `IV_ATM − RV`. Fail-closed: `None` unless both
/// sides are finite (IVS-5).
pub fn vrp(iv_atm: f64, rv: f64) -> Option<f64> {
    (iv_atm.is_finite() && rv.is_finite()).then(|| iv_atm - rv)
}

/// One expiry slice: ascending `(strike, mark_iv, open_interest)` plus call /
/// put delta points for the smile interpolation.
struct ExpirySlice<'a> {
    rows: Vec<(f64, f64, f64)>, // (strike, iv, oi)
    call_pts: Vec<(f64, f64)>,  // (|delta|, iv)
    put_pts: Vec<(f64, f64)>,
    _key_min_expiry: std::marker::PhantomData<&'a ()>,
}

fn iv_ok(iv: f64) -> bool {
    iv.is_finite() && iv > 0.0
}

impl<'a> ExpirySlice<'a> {
    /// Contracts of one expiry with usable rows (finite positive IV; OI kept
    /// as-is so zero-OI rows exist for skew but are excluded by the index).
    fn build(chain: &'a BTreeMap<ContractKey, TickerSnap>, expiry: i64) -> Self {
        let mut rows = Vec::new();
        let mut call_pts = Vec::new();
        let mut put_pts = Vec::new();
        for (key, snap) in chain {
            if key.expiry_ts_ns != expiry || !snap.usable_oi() || !iv_ok(snap.mark_iv) {
                continue;
            }
            rows.push((key.strike(), snap.mark_iv, snap.open_interest));
            if let Some(g) = snap.greeks {
                if g.delta.is_finite() {
                    match key.kind() {
                        OptionKind::Call if g.delta > 0.0 && g.delta < 1.0 => {
                            call_pts.push((g.delta, snap.mark_iv))
                        }
                        OptionKind::Put if g.delta < 0.0 && g.delta > -1.0 => {
                            put_pts.push(((-g.delta), snap.mark_iv))
                        }
                        _ => {}
                    }
                }
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        call_pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        put_pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self {
            rows,
            call_pts,
            put_pts,
            _key_min_expiry: std::marker::PhantomData,
        }
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Linear interpolation of IV at moneyness = 1.0 (i.e. strike == spot),
    /// falling back to the nearest strike when only one side brackets (IVS-2).
    fn atm_iv(&self, spot: f64) -> Option<f64> {
        if self.rows.is_empty() || !spot.is_finite() || spot <= 0.0 {
            return None;
        }
        let idx = self.rows.partition_point(|r| r.0 < spot);
        match (idx.checked_sub(1), self.rows.get(idx)) {
            // Bracketed: lower = rows[i], upper = rows[idx].
            (Some(i), Some(_)) => {
                let (k1, iv1, _) = self.rows[i];
                let (k2, iv2, _) = self.rows[idx];
                if k2 == k1 {
                    return Some(iv1);
                }
                // spot may equal k1 exactly (partition point semantics).
                let w = ((spot - k1) / (k2 - k1)).clamp(0.0, 1.0);
                Some(iv1 + w * (iv2 - iv1))
            }
            (Some(i), None) => self.rows.get(i).map(|r| r.1),
            (None, Some(&(_, iv, _))) => Some(iv),
            (None, None) => None,
        }
    }

    /// Linear interpolation of IV at |delta| = target (IVS-4); `None` when the
    /// smile does not bracket the target.
    fn iv_at_delta(pts: &[(f64, f64)], target: f64) -> Option<f64> {
        // Exact hits at either end of the smile must resolve (a 25Δ quote IS
        // the interpolation point).
        if let Some(&(d, iv)) = pts.first() {
            if d == target {
                return Some(iv);
            }
        }
        if let Some(&(d, iv)) = pts.last() {
            if d == target {
                return Some(iv);
            }
        }
        let idx = pts.partition_point(|p| p.0 < target);
        match (idx.checked_sub(1), pts.get(idx)) {
            (Some(_i), Some(&(d1, iv1))) => {
                let (d2, iv2) = pts[idx];
                if d2 == d1 {
                    return Some(iv1);
                }
                let w = ((target - d1) / (d2 - d1)).clamp(0.0, 1.0);
                Some(iv1 + w * (iv2 - iv1))
            }
            _ => None, // target outside the quoted |delta| range — no extrapolation
        }
    }
}

/// Pure IV-surface computation over the recorded chain (spec 038 accessors).
#[derive(Debug, Default)]
pub struct IvSurfaceAggregator {
    chains: ChainMap,
}

impl IvSurfaceAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn chains(&self) -> &ChainMap {
        &self.chains
    }

    pub fn on_ticker(&mut self, ev: &EventEnvelope, underlying: &str) -> bool {
        let Some((leg, snap)) = TickerSnap::from_event(ev) else {
            return false;
        };
        if !leg.underlying.eq_ignore_ascii_case(underlying) {
            return false;
        }
        let mut canon = leg.clone();
        canon.underlying = underlying.to_string();
        self.chains.insert(&canon, snap, ev.recv_ts_ns);
        true
    }

    fn chain(&self, u: &str) -> Option<&BTreeMap<ContractKey, TickerSnap>> {
        self.chains.chain(u)
    }

    fn slice_at(&self, u: &str, expiry: i64) -> Option<ExpirySlice<'_>> {
        let chain = self.chain(u)?;
        let slice = ExpirySlice::build(chain, expiry);
        (!slice.is_empty()).then_some(slice)
    }

    /// Expiry minimizing |T − target_years| among usable rows (IVS-3: nearest
    /// available expiry; never synthesized).
    fn expiry_nearest_tenor(
        &self,
        u: &str,
        target_years: f64,
        now_ns: i64,
    ) -> Option<i64> {
        let chain = self.chain(u)?;
        let mut best = (f64::INFINITY, None);
        for key in chain.keys() {
            let t = (key.expiry_ts_ns - now_ns) as f64 / NS_PER_DAY / 365.25;
            if t <= 0.0 {
                continue; // expired or same-midnight: not a live tenor
            }
            let dist = (t - target_years).abs();
            if dist < best.0 {
                best = (dist, Some(key.expiry_ts_ns));
            }
        }
        best.1
    }

    fn nearest_expiry(&self, u: &str, now_ns: i64) -> Option<i64> {
        self.expiry_nearest_tenor(u, 0.0, now_ns)
    }

    /// ATM IV at the nearest expiry (IVS-2): linear interpolation at
    /// moneyness = 1.0 using the ticker's own underlying price.
    pub fn atm_iv_at(&self, u: &str, now_ns: i64) -> Option<f64> {
        let expiry = self.nearest_expiry(u, now_ns)?;
        let spot = self.chains.spot(u)?;
        self.slice_at(u, expiry)?.atm_iv(spot)
    }

    /// ATM IV for a target tenor in years, nearest-available-expiry fallback
    /// (IVS-3). `None` when no non-expired tenor is close enough to be
    /// meaningful (caller decides the policy; the feature passes through).
    pub fn term_iv(&self, u: &str, tenor_years: f64, now_ns: i64) -> Option<f64> {
        let expiry = self.expiry_nearest_tenor(u, tenor_years, now_ns)?;
        let spot = self.chains.spot(u)?;
        self.slice_at(u, expiry)?.atm_iv(spot)
    }

    /// 25Δ risk reversal and wing richness at the nearest expiry (IVS-4):
    /// `RR = IV_25Δcall − IV_25Δput` (negative = put skew / fear),
    /// `WR = IV_25Δput / IV_25Δcall`.
    pub fn skew_25(&self, u: &str, now_ns: i64) -> Option<(f64, f64)> {
        let expiry = self.nearest_expiry(u, now_ns)?;
        let slice = self.slice_at(u, expiry)?;
        let c25 = ExpirySlice::iv_at_delta(&slice.call_pts, 0.25)?;
        let p25 = ExpirySlice::iv_at_delta(&slice.put_pts, 0.25)?;
        if c25 <= 0.0 {
            return None;
        }
        Some((c25 - p25, p25 / c25))
    }

    /// OI-weighted composite vol index at the nearest expiry (IVS-7): the
    /// weighted mean of VARIANCE — `sqrt(Σ wᵢ·IVᵢ² / Σ wᵢ)`. Zero-OI strikes
    /// do not contribute; positive + finite or suppressed.
    pub fn vol_index(&self, u: &str, now_ns: i64) -> Option<f64> {
        let expiry = self.nearest_expiry(u, now_ns)?;
        let slice = self.slice_at(u, expiry)?;
        let mut wsum = 0.0;
        let mut vsum = 0.0;
        for &(_k, iv, oi) in &slice.rows {
            if !oi.is_finite() || oi <= 0.0 {
                continue; // IVS-7: zero-OI strikes MUST NOT contribute
            }
            wsum += oi;
            vsum += oi * iv * iv;
        }
        if wsum <= 0.0 {
            return None;
        }
        let var = vsum / wsum;
        (var.is_finite() && var > 0.0).then(|| var.sqrt())
    }
}

// ---------------------------------------------------------------------------
// Catalog feature adapters
// ---------------------------------------------------------------------------

fn ticker_for(
    agg: &mut IvSurfaceAggregator,
    ev: &EventEnvelope,
    underlying: &str,
) -> bool {
    agg.on_ticker(ev, underlying)
}

/// `iv.atm.{u}` — ATM IV at the nearest expiry, interpolated at moneyness 1.0.
#[derive(Debug)]
pub struct IvAtm {
    underlying: String,
    agg: IvSurfaceAggregator,
}

impl IvAtm {
    pub fn new(underlying: &str) -> Self {
        Self { underlying: underlying.to_ascii_lowercase(), agg: IvSurfaceAggregator::new() }
    }
    pub fn aggregator(&self) -> &IvSurfaceAggregator {
        &self.agg
    }
}

impl crate::engine::TickFeature for IvAtm {
    fn id(&self) -> String {
        format!("iv.atm.{}", self.underlying)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if !ticker_for(&mut self.agg, ev, &self.underlying) {
            return None;
        }
        self.agg.atm_iv_at(&self.underlying, ev.recv_ts_ns)
    }
}

/// `iv.term.{u}.{tenor}` — ATM IV for a target tenor with nearest-available-
/// expiry fallback (IVS-3).
#[derive(Debug)]
pub struct IvTerm {
    tenor_name: String,
    tenor_years: f64,
    underlying: String,
    agg: IvSurfaceAggregator,
}

impl IvTerm {
    pub fn new(underlying: &str, tenor_name: &str, tenor_days: f64) -> Self {
        Self {
            tenor_name: tenor_name.to_owned(),
            tenor_years: tenor_days / 365.25,
            underlying: underlying.to_ascii_lowercase(),
            agg: IvSurfaceAggregator::new(),
        }
    }
    pub fn aggregator(&self) -> &IvSurfaceAggregator {
        &self.agg
    }
}

impl crate::engine::TickFeature for IvTerm {
    fn id(&self) -> String {
        format!("iv.term.{}.{}", self.underlying, self.tenor_name)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if !ticker_for(&mut self.agg, ev, &self.underlying) {
            return None;
        }
        self.agg.term_iv(&self.underlying, self.tenor_years, ev.recv_ts_ns)
    }
}

/// `iv.skew.{u}.rr25` / `iv.skew.{u}.wing25` — 25Δ risk reversal and wing
/// richness (IVS-4). `kind` selects which of the pair this instance emits.
#[derive(Debug)]
pub struct IvSkew {
    want_wing: bool,
    underlying: String,
    agg: IvSurfaceAggregator,
}

impl IvSkew {
    pub fn risk_reversal(underlying: &str) -> Self {
        Self::new(false, underlying)
    }
    pub fn wing_richness(underlying: &str) -> Self {
        Self::new(true, underlying)
    }
    fn new(want_wing: bool, underlying: &str) -> Self {
        Self { want_wing, underlying: underlying.to_ascii_lowercase(), agg: IvSurfaceAggregator::new() }
    }
    pub fn aggregator(&self) -> &IvSurfaceAggregator {
        &self.agg
    }
}

impl crate::engine::TickFeature for IvSkew {
    fn id(&self) -> String {
        let tail = if self.want_wing { "wing25" } else { "rr25" };
        format!("iv.skew.{}.{}", self.underlying, tail)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if !ticker_for(&mut self.agg, ev, &self.underlying) {
            return None;
        }
        let (rr, wing) = self.agg.skew_25(&self.underlying, ev.recv_ts_ns)?;
        if self.want_wing {
            Some(wing)
        } else {
            Some(rr)
        }
    }
}

/// `iv.index.{u}` — OI-weighted DVOL-style composite vol index (IVS-7).
#[derive(Debug)]
pub struct IvIndex {
    underlying: String,
    agg: IvSurfaceAggregator,
}

impl IvIndex {
    pub fn new(underlying: &str) -> Self {
        Self { underlying: underlying.to_ascii_lowercase(), agg: IvSurfaceAggregator::new() }
    }
    pub fn aggregator(&self) -> &IvSurfaceAggregator {
        &self.agg
    }
}

impl crate::engine::TickFeature for IvIndex {
    fn id(&self) -> String {
        format!("iv.index.{}", self.underlying)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if !ticker_for(&mut self.agg, ev, &self.underlying) {
            return None;
        }
        self.agg.vol_index(&self.underlying, ev.recv_ts_ns)
    }
}

/// Mid-rank percentile of the current value against past observations:
/// `(#past < v + 0.5·(#past == v)) / n_past` — deterministic, exact (IVS-6
/// sort-based method), and ties land at 0.5 rather than saturating at 1.0.
fn midrank_percentile(past: &[f64], current: f64) -> f64 {
    let mut below = 0usize;
    let mut equal = 0usize;
    for &p in past {
        if p < current {
            below += 1;
        } else if p == current {
            equal += 1;
        }
    }
    let n = past.len() as f64;
    (below as f64 + 0.5 * equal as f64) / n
}

/// Vol regime encoding — REVERSE-ORDINAL, per the spec 038 warning:
/// 0 = Rich, 1 = Fair, 2 = Cheap. Do not flip after materialization (CONV-20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolRegime {
    Rich = 0,
    Fair = 1,
    Cheap = 2,
}

impl VolRegime {
    pub fn from_percentile(pct: f64) -> Self {
        // Thresholds per spec 038: ≥66% Rich, ≤33% Cheap, else Fair.
        if pct >= 0.66 {
            VolRegime::Rich
        } else if pct <= 0.33 {
            VolRegime::Cheap
        } else {
            VolRegime::Fair
        }
    }
    pub fn encode(&self) -> f64 {
        *self as u8 as f64
    }
}

/// `iv.percentile.{u}` / `iv.regime.{u}` — rolling percentile rank of ATM IV
/// over a trailing window of OBSERVED days (IVS-11: collector-downtime gaps
/// contribute nothing to the denominator because only days with a recorded
/// sample are stored). Both features share this struct; `want_regime`
/// selects the emission (`kind`).
#[derive(Debug)]
pub struct IvPercentileFeature {
    want_regime: bool,
    window_days: i64,
    underlying: String,
    agg: IvSurfaceAggregator,
    /// UTC-day bucket → last ATM IV observed that day (BTreeMap: sorted,
    /// CONV-10; missing days simply absent — IVS-11).
    daily: BTreeMap<i64, f64>,
}

const DAY_NS: i64 = 86_400_000_000_000;

impl IvPercentileFeature {
    pub fn percentile(underlying: &str, window_days: i64) -> Self {
        Self::new(false, underlying, window_days)
    }
    pub fn regime(underlying: &str, window_days: i64) -> Self {
        Self::new(true, underlying, window_days)
    }
    fn new(want_regime: bool, underlying: &str, window_days: i64) -> Self {
        Self {
            want_regime,
            window_days: window_days.max(1),
            underlying: underlying.to_ascii_lowercase(),
            agg: IvSurfaceAggregator::new(),
            daily: BTreeMap::new(),
        }
    }

    /// Exposed for tests: percentile of the latest sample vs prior days.
    pub fn current_percentile(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let iv = self.update_daily(ev)?;
        let today = ev.recv_ts_ns.div_euclid(DAY_NS);
        let cutoff = today - self.window_days;
        let past: Vec<f64> = self
            .daily
            .range(..today)
            .filter(|(d, _)| **d >= cutoff && **d < today)
            .map(|(_, v)| *v)
            .collect();
        (!past.is_empty()).then(|| midrank_percentile(&past, iv))
    }

    fn update_daily(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if !ticker_for(&mut self.agg, ev, &self.underlying) {
            return None;
        }
        let iv = self.agg.atm_iv_at(&self.underlying, ev.recv_ts_ns)?;
        let today = ev.recv_ts_ns.div_euclid(DAY_NS);
        self.daily.insert(today, iv); // replace same-day sample (last wins)
        Some(iv)
    }
}

impl crate::engine::TickFeature for IvPercentileFeature {
    fn id(&self) -> String {
        if self.want_regime {
            format!("iv.regime.{}", self.underlying)
        } else {
            format!("iv.percentile.{}", self.underlying)
        }
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let pct = self.current_percentile(ev)?;
        if self.want_regime {
            Some(VolRegime::from_percentile(pct).encode())
        } else {
            Some(pct)
        }
    }
}



