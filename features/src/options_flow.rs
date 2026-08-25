//! Cross-venue options flow aggregator (spec 039, OFI-1..13).
//!
//! Consumes `OptionTrade` events (plus OptionTicker for spot/delta
//! references) and emits windowed flow features: net premium flow, block
//! detection, delta-adjusted notional, kind-aware moneyness buckets,
//! per-expiry tenor decomposition, call-put ratio, dynamic-threshold whale
//! flow, and flow acceleration. Cross-venue divergence is wired but
//! suppressed (None) until a second venue appears (OFI-12).
//!
//! Windows close on event-time bucket rollover exactly like the footprint
//! accumulator (spec 004) — no wall clock (PD-3). The final partial window is
//! never emitted (same semantics as a bar close). Moneyness classification is
//! KIND-AWARE per the review fix in spec 039 (a 105k call with spot 100k is
//! OTM; the naive `|K/S − 1|` band misclassifies it).

use crate::options_greeks::{ContractKey, TickerSnap, NS_PER_DAY, YEAR_DAYS};
use mp_core::event::{EventEnvelope, MarketEvent, OptionKind, OptionLeg, Side, Venue};
use std::collections::{BTreeMap, BTreeSet};

/// Human label for a window length in ns (`1h`, `24h`, `90s`). Hours win over
/// days so a 24h window reads `24h` (the spec's cadence naming).
pub fn window_label(ns: i64) -> String {
    let s = ns.max(1) / 1_000_000_000;
    if s % 3600 == 0 && s >= 3600 {
        format!("{}h", s / 3600)
    } else if s % 86_400 == 0 && s >= 86_400 {
        format!("{}d", s / 86_400)
    } else {
        format!("{s}s")
    }
}

/// Kind-aware moneyness bucket (spec 039-MONEYNESS, review-fixed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoneynessBucket {
    Itm,
    Atm,
    Otm,
}

/// Classify one option trade. `None` = excluded (deep OTM beyond `otm_cap`,
/// or non-finite inputs — CONV-8 fail-closed).
pub fn moneyness_bucket(
    kind: OptionKind,
    strike: f64,
    spot: f64,
    atm_threshold: f64,
    itm_cap: f64,
    otm_cap: f64,
) -> Option<MoneynessBucket> {
    if !(strike.is_finite() && spot.is_finite() && spot > 0.0) {
        return None;
    }
    let rel = (strike / spot - 1.0).abs();
    if !rel.is_finite() {
        return None;
    }
    if rel < atm_threshold {
        return Some(MoneynessBucket::Atm);
    }
    let itm_side = match kind {
        OptionKind::Call => strike < spot,
        OptionKind::Put => strike > spot,
    };
    if itm_side {
        (rel <= itm_cap).then_some(MoneynessBucket::Itm)
    } else {
        (rel <= otm_cap).then_some(MoneynessBucket::Otm)
    }
}

/// Days-to-expiry tenor bucket (OFI-6; boundaries configurable, defaults
/// 7/45/180).
pub fn flow_tenor(
    days: f64,
    weekly_max: f64,
    monthly_max: f64,
    quarterly_max: f64,
) -> &'static str {
    if days <= weekly_max {
        "weekly"
    } else if days <= monthly_max {
        "monthly"
    } else if days <= quarterly_max {
        "quarterly"
    } else {
        "leap"
    }
}

/// Abramowitz–Stegun 7.1.26 rational approximation of the normal CDF — pure,
/// deterministic, no external math crate (spec 039 review decision).
fn norm_cdf(x: f64) -> f64 {
    const A1: f64 = 0.254_829_592;
    const A2: f64 = -0.284_496_736;
    const A3: f64 = 1.421_413_741;
    const A4: f64 = -1.453_152_027;
    const A5: f64 = 1.061_405_429;
    const P: f64 = 0.327_591_1;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let ax = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + P * ax);
    let poly = t * (A1 + t * (A2 + t * (A3 + t * (A4 + t * A5))));
    let erf = 1.0 - poly * (-ax * ax).exp();
    0.5 * (1.0 + sign * erf)
}

/// Black-Scholes delta fallback (OFI-4) when no ticker delta matches.
/// `None` when inputs are non-finite, σ ≤ 0, or T ≤ 0 (fail-closed).
pub fn bs_delta(spot: f64, strike: f64, t_years: f64, iv: f64, kind: OptionKind) -> Option<f64> {
    if !(spot.is_finite() && strike.is_finite() && t_years.is_finite() && iv.is_finite())
        || spot <= 0.0
        || strike <= 0.0
        || iv <= 0.0
        || t_years <= 0.0
    {
        return None;
    }
    let sqrt_t = t_years.sqrt();
    let d1 = ((spot / strike).ln() + 0.5 * iv * iv * t_years) / (iv * sqrt_t);
    if !d1.is_finite() {
        return None;
    }
    Some(match kind {
        OptionKind::Call => norm_cdf(d1),
        OptionKind::Put => norm_cdf(d1) - 1.0,
    })
}

/// Tunable flow parameters (mirrors `[options_flow]` config; kept as a plain
/// struct so features stay config-agnostic).
#[derive(Debug, Clone)]
pub struct FlowParams {
    pub contract_multiplier: f64,
    pub block_threshold_usd: f64,
    pub whale_floor_usd: f64,
    pub k_whale: f64,
    pub atm_threshold: f64,
    pub itm_cap: f64,
    pub otm_cap: f64,
    /// Tenor boundaries in days (weekly ≤, monthly ≤, quarterly ≤).
    pub tenor_bounds: [f64; 3],
}

impl Default for FlowParams {
    fn default() -> Self {
        Self {
            contract_multiplier: 1.0,
            block_threshold_usd: 100_000.0,
            whale_floor_usd: 500_000.0,
            k_whale: 3.0,
            atm_threshold: 0.02,
            itm_cap: 0.10,
            otm_cap: 0.50,
            tenor_bounds: [7.0, 45.0, 180.0],
        }
    }
}

/// Everything one closed flow window aggregates (OFI-2..8).
#[derive(Debug, Default)]
pub(crate) struct WindowCore {
    bucket: Option<i64>,
    npf: f64,
    block_signed: f64,
    call_vol: f64,
    put_vol: f64,
    net_delta: f64,
    itm: f64,
    atm_flow: f64,
    otm: f64,
    tenors: [f64; 4], // weekly, monthly, quarterly, leap
    notionals: Vec<f64>,
}

impl WindowCore {
    fn reset(&mut self) {
        *self = WindowCore::default();
    }
}

/// Shared state + window machinery behind all flow feature instances for one
/// (underlying, window) pair.
#[derive(Debug)]
pub(crate) struct FlowWindow {
    pub underlying: String,
    pub window_ns: i64,
    params: FlowParams,
    /// Latest spot + per-contract delta/IV from OptionTicker events.
    spot: Option<f64>,
    deltas: BTreeMap<ContractKey, (f64, f64)>, // (delta, mark_iv)
    /// Contracts whose ticker delta was present but INVALID (|δ|>1 or
    /// non-finite): OFI-4 requires suppression, not a BS fallback that would
    /// paper over a bad venue print.
    invalid: BTreeSet<ContractKey>,
    venues: BTreeSet<Venue>,
    venue_npf: BTreeMap<Venue, f64>,
    core: WindowCore,
}

impl FlowWindow {
    pub(crate) fn new(underlying: &str, window_ns: i64, params: FlowParams) -> Self {
        Self {
            underlying: underlying.to_ascii_lowercase(),
            window_ns,
            params,
            spot: None,
            deltas: BTreeMap::new(),
            invalid: BTreeSet::new(),
            venues: BTreeSet::new(),
            venue_npf: BTreeMap::new(),
            core: WindowCore::default(),
        }
    }

    /// Feed one event. Returns `Some(ClosedWindow)` when the event rolled the
    /// bucket (the closed window's aggregates, before reset).
    pub(crate) fn on_event(&mut self, ev: &EventEnvelope) -> Option<ClosedWindow> {
        let mut rolled = None;
        match &ev.body {
            MarketEvent::OptionTicker { leg, .. } => {
                if leg.underlying.eq_ignore_ascii_case(&self.underlying) {
                    if let Some((leg, snap)) = TickerSnap::from_event(ev) {
                        if snap.spot.is_finite() && snap.spot > 0.0 {
                            self.spot = Some(snap.spot);
                        }
                        if let Some(g) = snap.greeks {
                            let key = ContractKey::new(leg.expiry_ts_ns, leg.strike, leg.kind);
                            if g.delta.is_finite() && g.delta.abs() <= 1.0 {
                                self.deltas.insert(key, (g.delta, snap.mark_iv));
                            } else {
                                // Ticker matched but delta unusable: mark the
                                // contract so trades on it are SUPPRESSED
                                // (OFI-4), never silently BS-substituted.
                                self.deltas.remove(&key);
                                self.invalid.insert(key);
                            }
                        }
                    }
                }
            }
            MarketEvent::OptionTrade {
                leg,
                price,
                qty,
                side,
                ..
            } => {
                if !leg.underlying.eq_ignore_ascii_case(&self.underlying) {
                    return rolled;
                }
                if price.is_finite() && qty.is_finite() && *qty > 0.0 {
                    rolled = self.on_trade(ev.recv_ts_ns, leg, *price, *qty, *side, ev.venue);
                }
            }
            _ => {}
        }
        rolled
    }

    fn on_trade(
        &mut self,
        ts_ns: i64,
        leg: &OptionLeg,
        price: f64,
        qty: f64,
        side: Side,
        venue: Venue,
    ) -> Option<ClosedWindow> {
        let bucket = ts_ns.div_euclid(self.window_ns) * self.window_ns;
        let mut closed = None;
        if let Some(cur) = self.core.bucket {
            if cur != bucket {
                closed = Some(self.close());
            }
        }
        let p = &self.params;
        let notional = price * qty * p.contract_multiplier;
        if !notional.is_finite() {
            return closed;
        }
        let sign = match side {
            Side::Buy => 1.0,
            Side::Sell => -1.0,
        };
        if self.core.bucket.is_none() {
            self.core.bucket = Some(bucket);
        }
        // Net premium flow (OFI-3): signed by aggressor side.
        self.core.npf += sign * notional;
        *self.venue_npf.entry(venue).or_default() += sign * notional;
        self.venues.insert(venue);
        // Block detection (OFI-2): USD notional threshold.
        if notional >= p.block_threshold_usd {
            self.core.block_signed += sign * notional;
        }
        match leg.kind {
            OptionKind::Call => self.core.call_vol += notional.abs(),
            OptionKind::Put => self.core.put_vol += notional.abs(),
        }

        // Delta-adjusted flow (OFI-4): ticker delta matched by
        // (strike, expiry, kind); BS fallback from (mark_iv, spot, T);
        // |delta| > 1 suppressed (CONV-8).
        let key = ContractKey::new(leg.expiry_ts_ns, leg.strike, leg.kind);
        let delta = if self.invalid.contains(&key) {
            None // ticker said |δ|>1: suppress (OFI-4/CONV-8), no fallback
        } else {
            match self.deltas.get(&key) {
                Some(&(d, _)) => Some(d),
                None => {
                    let iv = self
                        .deltas
                        .values()
                        .map(|&(_, iv)| iv)
                        .find(|iv| iv.is_finite() && *iv > 0.0);
                    let t = (leg.expiry_ts_ns - ts_ns).max(0) as f64 / (YEAR_DAYS * NS_PER_DAY);
                    match (self.spot, iv) {
                        (Some(s), Some(iv)) => bs_delta(s, leg.strike, t, iv, leg.kind),
                        _ => None,
                    }
                }
            }
        };
        if let Some(d) = delta.filter(|d| d.is_finite() && d.abs() <= 1.0) {
            self.core.net_delta += sign * d * notional;
        }
        // Moneyness (OFI-5, kind-aware): fail-closed without a spot reference;
        // deep OTM excluded (noise guard).
        if let Some(spot) = self.spot {
            if let Some(b) = moneyness_bucket(
                leg.kind,
                leg.strike,
                spot,
                p.atm_threshold,
                p.itm_cap,
                p.otm_cap,
            ) {
                match b {
                    MoneynessBucket::Itm => self.core.itm += sign * notional,
                    MoneynessBucket::Atm => self.core.atm_flow += sign * notional,
                    MoneynessBucket::Otm => self.core.otm += sign * notional,
                }
            }
        }
        // Per-expiry tenor decomposition (OFI-6).
        let days = (leg.expiry_ts_ns - ts_ns).max(0) as f64 / NS_PER_DAY;
        let idx = match flow_tenor(
            days,
            p.tenor_bounds[0],
            p.tenor_bounds[1],
            p.tenor_bounds[2],
        ) {
            "weekly" => 0,
            "monthly" => 1,
            "quarterly" => 2,
            _ => 3,
        };
        self.core.tenors[idx] += sign * notional;
        self.core.notionals.push(notional);
        closed
    }

    fn close(&mut self) -> ClosedWindow {
        // Whale threshold (OFI-8): max(floor, k × p95) over THIS window's
        // notionals (exact sort — spec 039 review decision; self-inclusion
        // bias is conservative and documented).
        let mut sorted = std::mem::take(&mut self.core.notionals);
        sorted.sort_by(|a, b| a.total_cmp(b));
        let p95 = if sorted.is_empty() {
            0.0
        } else {
            let idx = ((0.95 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
            sorted[idx]
        };
        let whale_threshold = self.params.whale_floor_usd.max(self.params.k_whale * p95);
        let whale_count = sorted.iter().filter(|n| **n >= whale_threshold).count() as f64;
        let whale_net: f64 = sorted.iter().filter(|n| **n >= whale_threshold).sum();
        let closed = ClosedWindow {
            npf: self.core.npf,
            block_signed: self.core.block_signed,
            net_delta: self.core.net_delta,
            itm: self.core.itm,
            atm_flow: self.core.atm_flow,
            otm: self.core.otm,
            call_put_ratio: (self.core.put_vol > 0.0)
                .then(|| self.core.call_vol / self.core.put_vol),
            tenors: self.core.tenors,
            whale_count,
            whale_net,
            venues_seen: self.venues.len(),
            venue_npf_pair: {
                let mut it = self.venue_npf.values();
                match (it.next(), it.next()) {
                    (Some(a), Some(b)) => Some(a - b),
                    _ => None,
                }
            },
        };
        self.core.reset();
        self.venues.clear();
        self.venue_npf.clear();
        closed
    }
}

/// Aggregates of one CLOSED window, handed to the emitting feature.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClosedWindow {
    pub npf: f64,
    pub block_signed: f64,
    pub net_delta: f64,
    pub itm: f64,
    pub atm_flow: f64,
    pub otm: f64,
    pub call_put_ratio: Option<f64>,
    pub tenors: [f64; 4],
    pub whale_count: f64,
    pub whale_net: f64,
    pub venues_seen: usize,
    pub venue_npf_pair: Option<f64>,
}

// ---------------------------------------------------------------------------
// Catalog feature adapters
// ---------------------------------------------------------------------------

/// Which flow metric this adapter emits from its closed windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowMetric {
    NetPremium,
    Block,
    NetDelta,
    ItmFlow,
    AtmFlow,
    OtmFlow,
    CallPutRatio,
    WeeklyFlow,
    MonthlyFlow,
    QuarterlyFlow,
    LeapFlow,
    WhaleCount,
    WhaleNet,
    Acceleration,
    CrossVenueDivergence,
}

impl FlowMetric {
    fn name(&self) -> &'static str {
        match self {
            FlowMetric::NetPremium => "flow.net_premium",
            FlowMetric::Block => "flow.block",
            FlowMetric::NetDelta => "flow.net_delta",
            FlowMetric::ItmFlow => "flow.by_moneyness.itm",
            FlowMetric::AtmFlow => "flow.by_moneyness.atm",
            FlowMetric::OtmFlow => "flow.by_moneyness.otm",
            FlowMetric::CallPutRatio => "flow.call_put_ratio",
            FlowMetric::WeeklyFlow => "flow.by_expiry.weekly",
            FlowMetric::MonthlyFlow => "flow.by_expiry.monthly",
            FlowMetric::QuarterlyFlow => "flow.by_expiry.quarterly",
            FlowMetric::LeapFlow => "flow.by_expiry.leap",
            FlowMetric::WhaleCount => "flow.whale.count",
            FlowMetric::WhaleNet => "flow.whale.net",
            FlowMetric::Acceleration => "flow.accl",
            FlowMetric::CrossVenueDivergence => "flow.xdiv",
        }
    }
}

/// Global tick feature emitting ONE flow metric per CLOSED window
/// (`{metric}.{underlying}.{window}`); suppressed until the first rollover.
/// The final partial window of a stream is never emitted (bar-close analogy).
#[derive(Debug)]
pub struct FlowFeature {
    metric: FlowMetric,
    win: FlowWindow,
    /// Previous closed NPF for acceleration (OFI-7 warmup: ≥2 windows).
    last_npf: Option<f64>,
}

impl FlowFeature {
    pub fn new(metric: FlowMetric, underlying: &str, window_ns: i64, params: FlowParams) -> Self {
        Self {
            metric,
            win: FlowWindow::new(underlying, window_ns, params),
            last_npf: None,
        }
    }

    pub(crate) fn closed_metric(&mut self, c: &ClosedWindow) -> Option<f64> {
        match self.metric {
            FlowMetric::NetPremium => Some(c.npf),
            FlowMetric::Block => Some(c.block_signed),
            FlowMetric::NetDelta => Some(c.net_delta),
            FlowMetric::ItmFlow => Some(c.itm),
            FlowMetric::AtmFlow => Some(c.atm_flow),
            FlowMetric::OtmFlow => Some(c.otm),
            FlowMetric::CallPutRatio => c.call_put_ratio,
            FlowMetric::WeeklyFlow => Some(c.tenors[0]),
            FlowMetric::MonthlyFlow => Some(c.tenors[1]),
            FlowMetric::QuarterlyFlow => Some(c.tenors[2]),
            FlowMetric::LeapFlow => Some(c.tenors[3]),
            FlowMetric::WhaleCount => Some(c.whale_count),
            FlowMetric::WhaleNet => Some(c.whale_net),
            FlowMetric::Acceleration => {
                let out = self.last_npf.map(|prev| c.npf - prev);
                self.last_npf = Some(c.npf);
                out // None until 2 complete windows exist (OFI-7)
            }
            // OFI-12: "no divergence" and "only one venue" are semantically
            // different — single venue emits NOTHING, never zero.
            FlowMetric::CrossVenueDivergence => (c.venues_seen >= 2).then_some(c.venue_npf_pair?),
        }
    }
}

impl crate::engine::TickFeature for FlowFeature {
    fn id(&self) -> String {
        format!(
            "{}.{}.{}",
            self.metric.name(),
            self.win.underlying,
            window_label(self.win.window_ns)
        )
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let closed = self.win.on_event(ev)?;
        self.closed_metric(&closed)
    }
}
