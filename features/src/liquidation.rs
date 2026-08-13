//! Cross-venue liquidation aggregation + estimated cascade bands (spec 029,
//! LIQ-1..LIQ-9). Pure functions of (Liquidation, OpenInterest, MarkPrice)
//! events — NO new event variant (LIQ-5), deterministic (LIQ-3), no network
//! (LIQ-9).
//!
//! - `liq.agg`       — merges the throttled per-venue liq feeds: near-
//!   simultaneous reports of the same liquidation from different venues are
//!   de-duplicated within `dedup_window_ns`, and the rolling signed notional
//!   over `agg_window_ns` is emitted per liquidation. `sampled()` is always
//!   true — venues throttle (Binance ~1/s), so this is a best-effort merged
//!   tape, NOT a census.
//! - `liq.est_bands` — estimates liquidation cascade levels from OI + mark +
//!   a configurable leverage-tier distribution. Emits the fractional distance
//!   from mark to the nearest estimated long-side liq level; `estimated()` is
//!   always true — a model, not ground truth.
//! - [`WhaleBandStudy`] — the RES-4 offline study (LIQ-6): replays recorded
//!   Hyperliquid mark/OI into `liq.est_bands` state and pairs every spec 028
//!   `WhalePosition` real liq price with the sign-aware model estimate, then
//!   reports [`BandAccuracy`]. Evidence only — bands stay out of strategies
//!   until this study clears (WHL-5).

use crate::config::{LeverageTier, LiqEstBandsParams};
use crate::engine::{Locality, TickFeature};
use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};
use std::collections::{BTreeMap, VecDeque};

/// `liq.agg` — cross-venue de-sampled aggregate liquidation notional (LIQ-1).
pub struct LiqAgg {
    dedup_window_ns: i64,
    agg_window_ns: i64,
    buf: VecDeque<(i64, f64)>, // (recv_ts_ns, signed_notional)
    sum: f64,
    /// venue → (last_ts, last_price) for cross-venue dedup.
    last_venue_tick: BTreeMap<Venue, (i64, f64)>,
}

impl LiqAgg {
    pub fn new(dedup_window_ns: i64, agg_window_ns: i64) -> Self {
        Self {
            dedup_window_ns,
            agg_window_ns,
            buf: VecDeque::new(),
            sum: 0.0,
            last_venue_tick: BTreeMap::new(),
        }
    }
    /// The merged tape is a SAMPLE, not a census (venues throttle) — LIQ-1.
    pub fn sampled(&self) -> bool {
        true
    }
}

impl TickFeature for LiqAgg {
    fn id(&self) -> String {
        "liq.agg".into()
    }
    /// Online + offline (LIQ-4; Decision: the de-sampled tape is also replayed
    /// for feature-store materialization / backtests — only uses recorded
    /// Liquidation events, so it is safe to replay).
    fn locality(&self) -> Locality {
        Locality::Both
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let MarketEvent::Liquidation { price, qty, side } = ev.body else {
            return None;
        };
        let notional = price * qty;
        let signed = match side {
            Side::Buy => notional,
            Side::Sell => -notional,
        };
        let ts = ev.recv_ts_ns;
        // Cross-venue dedup: a liquidation at the same price reported on
        // ANOTHER venue within dedup_window_ns is one event, not two.
        let dup = self.last_venue_tick.iter().any(|(v, (pts, pprice))| {
            *v != ev.venue
                && ts - *pts <= self.dedup_window_ns
                && (pprice - price).abs() <= price.abs() * 1e-6
        });
        if dup {
            return None;
        }
        self.last_venue_tick.insert(ev.venue, (ts, price));
        self.buf.push_back((ts, signed));
        self.sum += signed;
        while let Some(&(t, v)) = self.buf.front() {
            if ts - t > self.agg_window_ns {
                self.sum -= v;
                self.buf.pop_front();
            } else {
                break;
            }
        }
        Some(self.sum)
    }
}
/// One rolling window per (venue, side) — the per-venue liquidation sums the
/// cross-venue divergence reads. Venue-level by design: the merged event
/// stream re-interns symbols per `(venue, venue_symbol)` (EVT-8), so a
/// feature cannot match "the same coin" across venues by `SymbolId` — the
/// honest v1 aggregates all symbols per venue (same convention as `liq.agg`).
/// Per-symbol cross-venue identity needs name-keyed events (engine change,
/// spec 004 decision note) and is v2.
#[derive(Default)]
struct SideWindow {
    buf: VecDeque<(i64, f64)>,
    sum: f64,
}
impl SideWindow {
    /// Evict entries older than `window_ns` relative to `ts`. Called at BOTH
    /// push and read: a (venue, side) window with no new events must still
    /// decay, or the divergence would serve stale sums forever (liq_delta_3
    /// regression — the cross-venue feature reads ALL four windows at every
    /// event, including ones the event did not touch).
    fn prune(&mut self, ts: i64, window_ns: i64) {
        while let Some(&(t, x)) = self.buf.front() {
            if ts - t > window_ns {
                self.sum -= x;
                self.buf.pop_front();
            } else {
                break;
            }
        }
    }
    /// Push (ts, notional) after pruning; returns the new rolling sum.
    fn push(&mut self, ts: i64, v: f64, window_ns: i64) -> f64 {
        self.prune(ts, window_ns);
        self.buf.push_back((ts, v));
        self.sum += v;
        self.sum
    }
    /// Prune to `ts` and return the as-of rolling sum.
    fn sum_at(&mut self, ts: i64, window_ns: i64) -> f64 {
        self.prune(ts, window_ns);
        self.sum
    }
}

/// `liq.delta.{a}_{b}` — cross-venue liquidation-pressure divergence
/// (COL-29 cascade detection, spec 004 §Liquidation flow):
/// `(Σbuy_a − Σsell_a) − (Σbuy_b − Σsell_b)` over the rolling window, all
/// symbols per venue. Large |delta| = the two venues DISAGREE on who is being
/// forced out — venue a squeezing shorts while venue b dumps longs (positive)
/// or the mirror (negative). Near zero = the venues are in sync (both dumping
/// longs, or both quiet) — no divergence to trade. The "buy-side on one venue
/// vs sell-side on another" reading the user asked for is the special case
/// where each venue is one-sided. Emits on every liquidation event from
/// either venue of the pair.
pub struct LiqDelta {
    window_ns: i64,
    a: Venue,
    b: Venue,
    buy: BTreeMap<Venue, SideWindow>,
    sell: BTreeMap<Venue, SideWindow>,
}
impl LiqDelta {
    pub fn new(window_ns: i64, a: Venue, b: Venue) -> Self {
        Self {
            window_ns,
            a,
            b,
            buy: BTreeMap::new(),
            sell: BTreeMap::new(),
        }
    }
}
impl TickFeature for LiqDelta {
    fn id(&self) -> String {
        // Underscore between venues — matches the spec's pairwise convention
        // (`px.divergence.{a}_{b}`, `basis.{a}_{b}`, `leadlag.{a}_{b}.{w}`)
        // so downstream prefix matching is uniform.
        format!("liq.delta.{}_{}", self.a.slug(), self.b.slug())
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let MarketEvent::Liquidation { price, qty, side } = ev.body else {
            return None;
        };
        if ev.venue != self.a && ev.venue != self.b {
            return None;
        }
        let notional = price * qty;
        let w = self.window_ns;
        let ts = ev.recv_ts_ns;
        match side {
            Side::Buy => {
                self.buy.entry(ev.venue).or_default().push(ts, notional, w);
            }
            Side::Sell => {
                self.sell.entry(ev.venue).or_default().push(ts, notional, w);
            }
        }
        // As-of divergence: prune EVERY window to this event's time so a
        // (venue, side) with no new events decays too (liq_delta_3).
        let (ba, sa) = (
            self.buy
                .get_mut(&self.a)
                .map(|sw| sw.sum_at(ts, w))
                .unwrap_or(0.0),
            self.sell
                .get_mut(&self.a)
                .map(|sw| sw.sum_at(ts, w))
                .unwrap_or(0.0),
        );
        let (bb, sb) = (
            self.buy
                .get_mut(&self.b)
                .map(|sw| sw.sum_at(ts, w))
                .unwrap_or(0.0),
            self.sell
                .get_mut(&self.b)
                .map(|sw| sw.sum_at(ts, w))
                .unwrap_or(0.0),
        );
        Some((ba - sa) - (bb - sb))
    }
}

/// `liq.est_bands` — estimated liquidation cascade bands (LIQ-2, LIQ-6).
pub struct LiqEstBands {
    maintenance_buffer: f64,
    leverage_tiers: Vec<LeverageTier>,
    last_mark: Option<f64>,
    last_oi: Option<f64>,
    long_level: f64,
    short_level: f64,
    notional_at_risk: f64,
}

impl LiqEstBands {
    pub fn new(maintenance_buffer: f64, leverage_tiers: Vec<LeverageTier>) -> Self {
        Self {
            maintenance_buffer,
            leverage_tiers,
            last_mark: None,
            last_oi: None,
            long_level: f64::NAN,
            short_level: f64::NAN,
            notional_at_risk: 0.0,
        }
    }
    /// The estimate is a MODEL (LIQ-2), never ground truth.
    pub fn estimated(&self) -> bool {
        true
    }
    /// Estimated long-side (long-position) liquidation level, price units.
    pub fn long_liq_level(&self) -> f64 {
        self.long_level
    }
    /// Estimated short-side (short-position) liquidation level, price units.
    pub fn short_liq_level(&self) -> f64 {
        self.short_level
    }
    /// Open-interest notional held in the "imminent" (high-leverage) cohort.
    pub fn notional_at_risk(&self) -> f64 {
        self.notional_at_risk
    }
    fn recompute(&mut self) -> Option<f64> {
        let mark = self.last_mark?;
        let oi = self.last_oi?;
        if !mark.is_finite() || mark <= 0.0 || !oi.is_finite() || oi <= 0.0 {
            return None; // no mark/OI yet ⇒ silent, never NaN (fail-closed)
        }
        // Deterministic descending order (total_cmp is a total order, even for
        // NaN) — CONV-10/CONV-11.
        let mut tiers: Vec<&LeverageTier> = self.leverage_tiers.iter().collect();
        tiers.sort_by(|a, b| b.leverage.total_cmp(&a.leverage));
        let mut nearest_level = f64::NAN;
        let mut nearest_dist = f64::MAX;
        let mut at_risk = 0.0;
        for t in tiers {
            // Maintenance margin fraction, `mmr = 1/(leverage × buffer)`.
            // A long entered at ~mark is liquidated after a `drop` fraction
            // decline: `drop = (1 − mmr)/leverage`; level ≈ mark·(1 − drop).
            let mmr = 1.0 / (t.leverage * self.maintenance_buffer.max(1e-9));
            let drop = (1.0 - mmr) / t.leverage;
            let level = mark * (1.0 - drop);
            let dist = mark - level;
            if dist >= 0.0 && dist < nearest_dist {
                nearest_dist = dist;
                nearest_level = level;
            }
            // "Imminent cascade" cohort = leverage ≥ 10× (documented assumption).
            if t.leverage >= 10.0 {
                at_risk += oi * t.weight;
            }
        }
        self.long_level = nearest_level;
        self.short_level = mark + nearest_dist; // symmetric upper estimate
        self.notional_at_risk = at_risk;
        if nearest_level.is_finite() && mark > 0.0 {
            let frac = (mark - nearest_level) / mark;
            Some(frac.clamp(0.0, 1.0))
        } else {
            None
        }
    }
}

impl TickFeature for LiqEstBands {
    fn id(&self) -> String {
        "liq.est_bands".into()
    }
    /// Online + offline (LIQ-4): feeds the live screen and offline RES-4
    /// validation / feature-store materialization.
    fn locality(&self) -> Locality {
        Locality::Both
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        match ev.body {
            MarketEvent::MarkPrice { mark, .. } => {
                self.last_mark = Some(mark);
                None
            }
            MarketEvent::OpenInterest {
                oi_contracts,
                oi_notional,
            } => {
                self.last_oi = Some(if oi_notional.is_finite() && oi_notional > 0.0 {
                    oi_notional
                } else {
                    oi_contracts
                });
                self.recompute() // emitted at OI-update cadence (LIQ-2)
            }
            _ => None,
        }
    }
}

/// RES-4 validation metric (LIQ-6): given `(estimated_long_level, realized_liq
/// price)` observations, return `(mean_relative_error, coverage)` where
/// `coverage` is the fraction with `estimated <= realized` (the estimate was
/// not dangerously above the actual liquidation price).
///
/// Offline-only (never strategy-consumed, LIQ-6); see [`WhaleBandStudy`] for
/// the harness that produces observations from recorded spec 028 Hyperliquid
/// positions.
pub fn band_accuracy(obs: &[(f64, f64)]) -> (f64, f64) {
    if obs.is_empty() {
        return (0.0, 0.0);
    }
    let mut sum_re = 0.0_f64;
    let mut cov = 0.0_f64;
    for (est, real) in obs {
        if real.is_finite() && *real > 0.0 {
            sum_re += (est - real).abs() / real;
            if est <= real {
                cov += 1.0;
            }
        }
    }
    let n = obs.len() as f64;
    (sum_re / n, cov / n)
}

/// One paired RES-4 observation: the model's estimated liq level at the
/// instant a real Hyperliquid position liquidation price was recorded
/// (spec 028 `WhalePosition` vs spec 029 `liq.est_bands`, LIQ-6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandObservation {
    pub symbol: SymbolId,
    /// `recv_ts_ns` of the WhalePosition event (COL-5).
    pub recv_ts_ns: i64,
    /// `true` = long position (`size > 0`): paired with the downside
    /// `long_liq_level` estimate. `false` = short: paired with the upside
    /// `short_liq_level` estimate.
    pub is_long: bool,
    /// Model estimate at that instant (a `LiqEstBands` recompute with the
    /// latest mark/OI). Always finite.
    pub estimate: f64,
    /// Real liquidation price from the position. Always finite and `> 0`.
    pub realized: f64,
}

/// RES-4 band-accuracy metric for one position side (LIQ-6).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BandAccuracy {
    pub n: u64,
    /// Mean relative error `Σ |est−real| / real / n` (ground-truth
    /// denominator; NaN/inf never enters — observations are pre-filtered).
    pub mean_relative_error: f64,
    /// Coverage: fraction of observations where the estimate was NOT on the
    /// dangerous side of the realized price — long: `est <= real` (estimate
    /// not dangerously above the actual), short: `est >= real` (mirrored).
    pub coverage: f64,
}

/// RES-4 event-study harness (LIQ-6): replays recorded Hyperliquid market
/// events (mark/OI) into per-symbol [`LiqEstBands`] state and, on every
/// recorded spec 028 `WhalePosition` with a real liq price, pairs it with the
/// sign-aware model estimate.
///
/// Sign awareness: a long position (`size > 0`) liquidates on the downside,
/// so it is paired with `long_liq_level` (mark − drop); a short position
/// liquidates on the upside and pairs with `short_liq_level` (mark + drop).
/// The coverage direction mirrors per side: `est <= real` is covered for
/// longs, `est >= real` for shorts — "the estimate was not dangerously on the
/// wrong side of the actual liquidation price".
///
/// Pure and deterministic (PD-3/CONV-9): a pure function of the event stream
/// plus the band params — no wall clock, no I/O, no randomness. Offline only;
/// output is research evidence, never a strategy input (WHL-5/PD-4).
pub struct WhaleBandStudy {
    maintenance_buffer: f64,
    leverage_tiers: Vec<LeverageTier>,
    /// Per-symbol band state, keyed by the interned symbol id. Only
    /// Hyperliquid mark/OI feed it, so the estimate is same-venue ground
    /// truth (apples-to-apples with the Hyperliquid positions).
    bands: BTreeMap<SymbolId, LiqEstBands>,
    observations: Vec<BandObservation>,
}

impl WhaleBandStudy {
    pub fn new(maintenance_buffer: f64, leverage_tiers: Vec<LeverageTier>) -> Self {
        Self {
            maintenance_buffer,
            leverage_tiers,
            bands: BTreeMap::new(),
            observations: Vec::new(),
        }
    }

    /// Build from the `liq.est_bands` TOML params (LIQ-7): the study uses the
    /// exact same leverage-tier assumptions the live feature does, so the
    /// offline grade is the live model's grade (one-code-path, FEA-4).
    pub fn from_params(p: &LiqEstBandsParams) -> Self {
        Self::new(p.maintenance_buffer, p.leverage_tiers.clone())
    }

    /// Feed one event. Returns the index of a newly recorded observation, or
    /// `None` when nothing was recorded (non-Hyperliquid data, no mark/OI yet,
    /// non-finite/zero-size position, or a venue that omits liq price).
    pub fn on_event(&mut self, ev: &EventEnvelope) -> Option<usize> {
        match &ev.body {
            // Band inputs: Hyperliquid mark/OI only, so the estimate state is
            // the same-venue market state the positions lived in.
            MarketEvent::MarkPrice { .. } | MarketEvent::OpenInterest { .. }
                if ev.venue == Venue::Hyperliquid =>
            {
                self.bands
                    .entry(ev.symbol)
                    .or_insert_with(|| {
                        LiqEstBands::new(self.maintenance_buffer, self.leverage_tiers.clone())
                    })
                    .on_event(ev);
                None
            }
            MarketEvent::WhalePosition {
                liq_price, size, ..
            } if ev.venue == Venue::Hyperliquid => {
                // Fail-closed (CONV-8): no real liq price ⇒ no observation;
                // a non-finite/zero size (corrupt event) is never classified
                // as a short and recorded.
                if !liq_price.is_finite() || *liq_price <= 0.0 || !size.is_finite() || *size == 0.0
                {
                    return None;
                }
                // No mark/OI seen for this symbol yet ⇒ no estimate ⇒ skip.
                let estimate = {
                    let bands = self.bands.get(&ev.symbol)?;
                    if *size > 0.0 {
                        bands.long_liq_level()
                    } else {
                        bands.short_liq_level()
                    }
                };
                if !estimate.is_finite() {
                    return None;
                }
                self.observations.push(BandObservation {
                    symbol: ev.symbol,
                    recv_ts_ns: ev.recv_ts_ns,
                    is_long: *size > 0.0,
                    estimate,
                    realized: *liq_price,
                });
                Some(self.observations.len() - 1)
            }
            _ => None,
        }
    }

    /// All recorded observations, in recv order.
    pub fn observations(&self) -> &[BandObservation] {
        &self.observations
    }

    /// Number of observations recorded so far.
    pub fn n_observations(&self) -> usize {
        self.observations.len()
    }

    /// Long-side accuracy. Definitionally identical to the standalone
    /// [`band_accuracy`] (same safety direction, `est <= real`) — reuse keeps
    /// one source of truth for the metric.
    pub fn long_accuracy(&self) -> BandAccuracy {
        let pairs: Vec<(f64, f64)> = self
            .observations
            .iter()
            .filter(|o| o.is_long)
            .map(|o| (o.estimate, o.realized))
            .collect();
        let (mre, cov) = band_accuracy(&pairs);
        BandAccuracy {
            n: pairs.len() as u64,
            mean_relative_error: mre,
            coverage: cov,
        }
    }

    /// Short-side accuracy. Mirrors the long-side safety direction: a short
    /// liquidates on the upside, so its estimate is covered when `est >= real`
    /// (the estimate was not dangerously BELOW the actual liq price).
    pub fn short_accuracy(&self) -> BandAccuracy {
        let mut n = 0u64;
        let mut sum_re = 0.0_f64;
        let mut cov = 0.0_f64;
        for o in &self.observations {
            if o.is_long {
                continue;
            }
            n += 1;
            sum_re += (o.estimate - o.realized).abs() / o.realized;
            if o.estimate >= o.realized {
                cov += 1.0;
            }
        }
        BandAccuracy {
            n,
            mean_relative_error: if n > 0 { sum_re / n as f64 } else { 0.0 },
            coverage: if n > 0 { cov / n as f64 } else { 0.0 },
        }
    }

    /// Combined accuracy across both sides: per-side safety direction, then
    /// merged. Never NaN (empty ⇒ zeros, CONV-8).
    pub fn accuracy(&self) -> BandAccuracy {
        let long = self.long_accuracy();
        let short = self.short_accuracy();
        let n = long.n + short.n;
        if n == 0 {
            return BandAccuracy::default();
        }
        BandAccuracy {
            n,
            mean_relative_error: (long.mean_relative_error * long.n as f64
                + short.mean_relative_error * short.n as f64)
                / n as f64,
            coverage: (long.coverage * long.n as f64 + short.coverage * short.n as f64) / n as f64,
        }
    }
}
