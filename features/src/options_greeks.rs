//! Options Greeks aggregation engine (spec 037, GRE-1..12).
//!
//! Consumes `OptionTicker` events (spec 031) and produces aggregate
//! portfolio-level Greeks: net GEX, max pain, implied probability, net
//! delta/vega/theta per underlying — plus finite-difference higher-order
//! Greeks (vanna/volga/charm) after warmup. Pure function of events: no wall
//! clock, no I/O, no unseeded randomness (PD-3/CONV-9). All iteration is in
//! sorted key order (CONV-10); non-finite inputs are skipped fail-closed
//! (CONV-8); a contract update REPLACES its snapshot (GRE-2), never
//! accumulates.
//!
//! The full per-strike profile / implied-probability distribution /
//! per-expiry decomposition live on [`GreeksAggregator`] accessors (GRE-11);
//! registered catalog features are the chain-level scalars (spec 037 v1
//! emission decision). Chain plumbing is shared with spec 038/039 modules.

use mp_core::event::{EventEnvelope, MarketEvent, OptionGreeks, OptionKind, OptionLeg};
use std::collections::BTreeMap;

pub(crate) const NS_PER_DAY: f64 = 86_400_000_000_000.0;
pub(crate) const YEAR_DAYS: f64 = 365.25;

/// Canonical chain key: (expiry, strike, kind). Strike stored as
/// `f64::to_bits()` of the always-positive strike — IEEE-754 bit patterns of
/// non-negative floats are monotone with value, so BTreeMap iteration is
/// strike-ascending without an OrderedFloat newtype (CONV-10 determinism).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContractKey {
    pub expiry_ts_ns: i64,
    strike_bits: u64,
    is_put: bool,
}

impl ContractKey {
    pub fn new(expiry_ts_ns: i64, strike: f64, kind: OptionKind) -> Self {
        Self {
            expiry_ts_ns,
            strike_bits: strike.to_bits(),
            is_put: matches!(kind, OptionKind::Put),
        }
    }
    pub fn strike(&self) -> f64 {
        f64::from_bits(self.strike_bits)
    }
    pub fn kind(&self) -> OptionKind {
        if self.is_put {
            OptionKind::Put
        } else {
            OptionKind::Call
        }
    }
}

/// Latest ticker snapshot for one contract (replaced on every update,
/// GRE-2 replace semantics).
#[derive(Debug, Clone, Copy)]
pub struct TickerSnap {
    pub mark_iv: f64,
    #[allow(dead_code)] // recorded verbatim; consumed by offline materialization
    pub mark_price: f64,
    /// The OptionTicker's own `underlying_price` (GRE-10 — never an external
    /// spot feed).
    pub spot: f64,
    pub open_interest: f64,
    pub greeks: Option<OptionGreeks>,
}

impl TickerSnap {
    /// Extract from an event body; `None` for non-ticker events.
    pub fn from_event(ev: &EventEnvelope) -> Option<(&OptionLeg, Self)> {
        let MarketEvent::OptionTicker {
            leg,
            mark_iv,
            mark_price,
            underlying_price,
            open_interest,
            greeks,
        } = &ev.body
        else {
            return None;
        };
        Some((
            leg,
            Self {
                mark_iv: *mark_iv,
                mark_price: *mark_price,
                spot: *underlying_price,
                open_interest: *open_interest,
                greeks: *greeks,
            },
        ))
    }

    /// Fail-closed row validity for OI-weighted math (CONV-8): spot positive,
    /// OI non-negative finite.
    pub fn usable_oi(&self) -> bool {
        self.spot.is_finite()
            && self.spot > 0.0
            && self.open_interest.is_finite()
            && self.open_interest >= 0.0
    }
}

/// Underlying → contract map. Shared by the Greeks (037), IV surface (038),
/// and flow (039) engines.
#[derive(Debug, Default)]
pub struct ChainMap {
    chains: BTreeMap<String, BTreeMap<ContractKey, TickerSnap>>,
    /// Most recent chain spot per underlying, stamped with the tick time —
    /// individual contracts re-tick at different moments, so "any row's
    /// spot" can be stale; the LATEST ticker batch is the chain index price
    /// (GRE-10/IVS-2).
    spots: BTreeMap<String, (i64, f64)>,
}

impl ChainMap {
    pub fn insert(&mut self, leg: &OptionLeg, snap: TickerSnap, ts_ns: i64) {
        let u = leg.underlying.clone();
        self.chains.entry(u.clone()).or_default().insert(
            ContractKey::new(leg.expiry_ts_ns, leg.strike, leg.kind),
            snap,
        );
        // Keep the freshest spot (>= so equal-timestamp batches within one
        // event loop still overwrite deterministically).
        match self.spots.get(&u) {
            Some(&(prev_ts, _)) if prev_ts > ts_ns => {}
            _ => {
                self.spots.insert(u, (ts_ns, snap.spot));
            }
        }
    }

    pub fn chain(&self, underlying: &str) -> Option<&BTreeMap<ContractKey, TickerSnap>> {
        self.chains.get(underlying)
    }

    /// Latest recorded chain spot for an underlying (freshest ticker batch).
    pub fn spot(&self, underlying: &str) -> Option<f64> {
        let (ts, spot) = *self.spots.get(underlying)?;
        let _ = ts;
        (spot.is_finite() && spot > 0.0).then_some(spot)
    }
}

/// GEX at one contract (GRE-2): `γ × OI × spot² × multiplier`, calls
/// positive / puts negative. `None` when any input is non-finite (CONV-8).
pub fn gex_at(snap: &TickerSnap, is_put: bool, multiplier: f64) -> Option<f64> {
    let g = snap.greeks?;
    if !snap.usable_oi() || !g.gamma.is_finite() {
        return None;
    }
    let sign = if is_put { -1.0 } else { 1.0 };
    Some(sign * g.gamma * snap.open_interest * snap.spot * snap.spot * multiplier)
}

// ---------------------------------------------------------------------------
// GreeksAggregator — the pure computation core (accessors + FD higher-order)
// ---------------------------------------------------------------------------

/// Aggregate first-order readings at one instant (emission source and FD base).
#[derive(Debug, Clone, Copy, Default)]
struct Aggregates {
    net_delta: f64,
    net_vega: f64,
    oi_weighted_iv_num: f64,
    oi_weighted_iv_den: f64,
    any_row: bool,
}

impl Aggregates {
    fn mean_iv(&self) -> Option<f64> {
        if self.oi_weighted_iv_den > 0.0 && self.oi_weighted_iv_num.is_finite() {
            Some(self.oi_weighted_iv_num / self.oi_weighted_iv_den)
        } else {
            None
        }
    }
}

/// Pure options-chain aggregator for one underlying. Each feature instance
/// owns its own copy — chains are small (hundreds of rows) and flat ownership
/// avoids shared mutable state.
#[derive(Debug)]
pub struct GreeksAggregator {
    chains: ChainMap,
    multiplier: f64,
    /// Previous snapshot's aggregates for finite-difference higher-order
    /// Greeks (GRE-5). Suppressed until two snapshots exist.
    prev: Option<(i64, Aggregates)>,
}

impl GreeksAggregator {
    pub fn new(multiplier: f64) -> Self {
        Self {
            chains: ChainMap::default(),
            multiplier,
            prev: None,
        }
    }

    pub fn chains(&self) -> &ChainMap {
        &self.chains
    }

    /// Record one OptionTicker event; false when it isn't a ticker for
    /// `underlying`. Matching is case-insensitive; the chain is keyed by the
    /// caller's canonical casing.
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

    fn aggregates(&self, underlying: &str) -> Option<Aggregates> {
        let chain = self.chains.chain(underlying)?;
        let mut agg = Aggregates::default();
        for (_key, snap) in chain.iter() {
            if !snap.usable_oi() {
                continue;
            }
            let Some(g) = snap.greeks else { continue };
            if !g.delta.is_finite() || !g.vega.is_finite() {
                continue;
            }
            agg.net_delta += g.delta * snap.open_interest * self.multiplier;
            agg.net_vega += g.vega * snap.open_interest * self.multiplier;
            if snap.mark_iv.is_finite() && snap.mark_iv > 0.0 {
                agg.oi_weighted_iv_num += snap.mark_iv * snap.open_interest;
                agg.oi_weighted_iv_den += snap.open_interest;
            }
            agg.any_row = true;
        }
        agg.any_row.then_some(agg)
    }

    /// Net signed GEX across the whole chain (calls +, puts −). Positive =
    /// dealers LONG gamma (moves dampened); negative = dealers SHORT gamma
    /// (moves amplified) — spec 037 interpretation (review-corrected).
    pub fn net_gex(&self, underlying: &str) -> Option<f64> {
        let chain = self.chains.chain(underlying)?;
        let mut any = false;
        let mut total = 0.0;
        for (key, snap) in chain {
            if let Some(v) = gex_at(snap, key.is_put, self.multiplier) {
                total += v;
                any = true;
            }
        }
        any.then_some(total)
    }

    /// Per-strike GEX profile aggregated across expiries (GRE-2 accessor),
    /// ascending strike order.
    pub fn gex_profile(&self, underlying: &str) -> Vec<(f64, f64)> {
        let Some(chain) = self.chains.chain(underlying) else {
            return Vec::new();
        };
        // bits ordering == value ordering for positive strikes.
        let mut by_strike: BTreeMap<u64, (f64, f64)> = BTreeMap::new();
        for (key, snap) in chain {
            if let Some(v) = gex_at(snap, key.is_put, self.multiplier) {
                let e = by_strike
                    .entry(key.strike_bits)
                    .or_insert((key.strike(), 0.0));
                e.1 += v;
            }
        }
        by_strike.into_values().collect()
    }

    /// Max pain of the NEAREST expiry with OI data (GRE-3): the candidate
    /// settlement strike minimizing total OI-weighted payout. Spot-independent
    /// (pure function of OI and strikes).
    pub fn max_pain(&self, underlying: &str) -> Option<f64> {
        let chain = self.chains.chain(underlying)?;
        let nearest = Self::nearest_expiry(chain)?;
        let mut rows: Vec<(f64, f64, f64)> = Vec::new(); // (strike, call_oi, put_oi)
        for (key, snap) in chain {
            if key.expiry_ts_ns != nearest || !snap.usable_oi() {
                continue;
            }
            match key.kind() {
                OptionKind::Call => rows.push((key.strike(), snap.open_interest, 0.0)),
                OptionKind::Put => rows.push((key.strike(), 0.0, snap.open_interest)),
            }
        }
        if rows.is_empty() {
            return None;
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let candidates: Vec<f64> = rows.iter().map(|r| r.0).collect();
        let mut best = (f64::INFINITY, None);
        for &settle in &candidates {
            let mut payout = 0.0;
            for &(k, call_oi, put_oi) in &rows {
                payout += call_oi * (settle - k).max(0.0) + put_oi * (k - settle).max(0.0);
            }
            if payout < best.0 {
                best = (payout, Some(settle));
            }
        }
        best.1
    }

    pub(crate) fn nearest_expiry(chain: &BTreeMap<ContractKey, TickerSnap>) -> Option<i64> {
        chain
            .iter()
            .filter(|(_, s)| s.usable_oi())
            .map(|(k, _)| k.expiry_ts_ns)
            .min()
    }

    /// Market-implied risk-neutral distribution from CALL deltas (GRE-4):
    /// P(S>K) ≈ delta_call. Buckets `(strike_upper_bound, density)` ascending:
    /// tail below lowest strike, inter-strike masses, then the tail above the
    /// highest (`f64::INFINITY` upper bound). Total mass is exactly 1 up to
    /// float error on complete chains.
    pub fn implied_prob_distribution(&self, underlying: &str) -> Option<Vec<(f64, f64)>> {
        let chain = self.chains.chain(underlying)?;
        let mut pts: Vec<(f64, f64)> = chain
            .iter()
            .filter_map(|(key, snap)| {
                if key.is_put || !snap.usable_oi() {
                    return None;
                }
                let d = snap.greeks?.delta;
                (d.is_finite() && (0.0..=1.0).contains(&d)).then_some((key.strike(), d))
            })
            .collect();
        if pts.len() < 2 {
            return None;
        }
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        pts.dedup_by(|a, b| a.0 == b.0);
        let p_above: Vec<f64> = pts.iter().map(|(_, d)| d.clamp(0.0, 1.0)).collect();
        let mut out = Vec::with_capacity(pts.len() + 1);
        out.push((pts[0].0, 1.0 - p_above[0]));
        for i in 1..pts.len() {
            out.push((pts[i].0, p_above[i - 1] - p_above[i]));
        }
        out.push((f64::INFINITY, *p_above.last().unwrap_or(&0.0)));
        Some(out)
    }

    /// Per-expiry decomposition `[net_delta, net_vega, net_theta]` (GRE-11):
    /// component sums equal the scalar aggregates.
    pub fn net_greeks_by_expiry(&self, underlying: &str) -> Vec<(i64, [f64; 3])> {
        let Some(chain) = self.chains.chain(underlying) else {
            return Vec::new();
        };
        let mut by_expiry: BTreeMap<i64, [f64; 3]> = BTreeMap::new();
        for (key, snap) in chain {
            if !snap.usable_oi() {
                continue;
            }
            let Some(g) = snap.greeks else { continue };
            if !(g.delta.is_finite() && g.vega.is_finite() && g.theta.is_finite()) {
                continue;
            }
            let e = by_expiry.entry(key.expiry_ts_ns).or_insert([0.0; 3]);
            e[0] += g.delta * snap.open_interest * self.multiplier;
            e[1] += g.vega * snap.open_interest * self.multiplier;
            e[2] += g.theta * snap.open_interest * self.multiplier;
        }
        by_expiry.into_iter().collect()
    }

    /// Advance the FD base with current aggregates. Returns `(vanna, volga,
    /// charm)` once ≥2 snapshots exist (GRE-5 warmup), else `None` after
    /// storing the first.
    ///
    /// KNOWN CONTAMINATION (spec 037 Decisions, review entry): consecutive
    /// snapshots differ in spot AND time AND IV together — these are regime
    /// indicators, not pricing-grade partial derivatives.
    pub fn advance_fd(
        &mut self,
        underlying: &str,
        ts_ns: i64,
    ) -> Option<(Option<f64>, Option<f64>, Option<f64>)> {
        let agg = self.aggregates(underlying)?;
        let Some((prev_ts, prev)) = self.prev else {
            self.prev = Some((ts_ns, agg));
            return None;
        };
        let dt_years = ((ts_ns - prev_ts).max(1) as f64) / (YEAR_DAYS * NS_PER_DAY);
        let out = match (agg.mean_iv(), prev.mean_iv()) {
            (Some(ivn), Some(ivp)) if (ivn - ivp).abs() > 1e-8 => (
                Some((agg.net_delta - prev.net_delta) / (ivn - ivp)),
                Some((agg.net_vega - prev.net_vega) / (ivn - ivp)),
                Some((agg.net_delta - prev.net_delta) / dt_years),
            ),
            _ => (None, None, None),
        };
        self.prev = Some((ts_ns, agg));
        Some(out)
    }

    /// Net delta exposure: Σ OI × delta × multiplier.
    pub fn net_delta(&self, underlying: &str) -> Option<f64> {
        self.aggregates(underlying).map(|a| a.net_delta)
    }

    /// Net vega exposure: Σ OI × vega × multiplier.
    pub fn net_vega(&self, underlying: &str) -> Option<f64> {
        self.aggregates(underlying).map(|a| a.net_vega)
    }

    /// Net theta exposure: Σ OI × theta × multiplier (daily cost of carry).
    pub fn net_theta(&self, underlying: &str) -> Option<f64> {
        let chain = self.chains.chain(underlying)?;
        let mut any = false;
        let mut total = 0.0;
        for snap in chain.values() {
            if !snap.usable_oi() {
                continue;
            }
            let Some(g) = snap.greeks else { continue };
            if !g.theta.is_finite() {
                continue;
            }
            total += g.theta * snap.open_interest * self.multiplier;
            any = true;
        }
        any.then_some(total)
    }
}

// ---------------------------------------------------------------------------
// Catalog feature adapters (global tick features — FEA-20 pattern)
// ---------------------------------------------------------------------------

/// Which chain-level scalar this adapter emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainScalar {
    GexNet,
    MaxPain,
    NetDelta,
    NetVega,
    NetTheta,
}

impl ChainScalar {
    fn feature_prefix(&self) -> &'static str {
        match self {
            ChainScalar::GexNet => "gex.net",
            ChainScalar::MaxPain => "gex.max_pain",
            ChainScalar::NetDelta => "net.delta",
            ChainScalar::NetVega => "net.vega",
            ChainScalar::NetTheta => "net.theta",
        }
    }
}

/// Global tick feature emitting one chain-level scalar (spec 037 v1 emission
/// decision): `gex.net.{u}`, `gex.max_pain.{u}`, `net.{delta,vega,theta}.{u}`.
/// Emits on every OptionTicker batch for its underlying (replace semantics);
/// suppressed until at least one usable row exists.
#[derive(Debug)]
pub struct ChainScalarFeature {
    kind: ChainScalar,
    underlying: String,
    agg: GreeksAggregator,
}

impl ChainScalarFeature {
    pub fn new(kind: ChainScalar, underlying: &str, multiplier: f64) -> Self {
        Self {
            kind,
            underlying: underlying.to_ascii_lowercase(),
            agg: GreeksAggregator::new(multiplier),
        }
    }

    /// Read-only view for tests / offline materialization (GRE-11).
    pub fn aggregator(&self) -> &GreeksAggregator {
        &self.agg
    }
}

impl crate::engine::TickFeature for ChainScalarFeature {
    fn id(&self) -> String {
        format!("{}.{}", self.kind.feature_prefix(), self.underlying)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if !self.agg.on_ticker(ev, &self.underlying) {
            return None;
        }
        match self.kind {
            ChainScalar::GexNet => self.agg.net_gex(&self.underlying),
            ChainScalar::MaxPain => self.agg.max_pain(&self.underlying),
            ChainScalar::NetDelta => self.agg.net_delta(&self.underlying),
            ChainScalar::NetVega => self.agg.net_vega(&self.underlying),
            ChainScalar::NetTheta => self.agg.net_theta(&self.underlying),
        }
    }
}

/// One higher-order Greek via finite differences across ticker snapshots
/// (GRE-5). Emits `None` until the second snapshot (FEA-3 warmup); the FD
/// values are regime indicators — see the contamination note in spec 037
/// Decisions and on [`GreeksAggregator::advance_fd`].
#[derive(Debug)]
pub struct HigherOrderGreek {
    which: u8, // 0=vanna 1=volga 2=charm
    underlying: String,
    agg: GreeksAggregator,
}

impl HigherOrderGreek {
    pub fn vanna(underlying: &str, multiplier: f64) -> Self {
        Self::new(0, underlying, multiplier)
    }
    pub fn volga(underlying: &str, multiplier: f64) -> Self {
        Self::new(1, underlying, multiplier)
    }
    pub fn charm(underlying: &str, multiplier: f64) -> Self {
        Self::new(2, underlying, multiplier)
    }
    fn new(which: u8, underlying: &str, multiplier: f64) -> Self {
        Self {
            which,
            underlying: underlying.to_ascii_lowercase(),
            agg: GreeksAggregator::new(multiplier),
        }
    }

    pub fn aggregator(&self) -> &GreeksAggregator {
        &self.agg
    }
}

impl crate::engine::TickFeature for HigherOrderGreek {
    fn id(&self) -> String {
        let fam = match self.which {
            0 => "vanna",
            1 => "volga",
            _ => "charm",
        };
        format!("{fam}.{}", self.underlying)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if !self.agg.on_ticker(ev, &self.underlying) {
            return None;
        }
        let (vanna, volga, charm) = self.agg.advance_fd(&self.underlying, ev.recv_ts_ns)?;
        match self.which {
            0 => vanna,
            1 => volga,
            _ => charm,
        }
    }
}
