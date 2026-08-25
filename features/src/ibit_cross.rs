//! IBIT ↔ Deribit cross-market features (spec 040, IBI-5/IBI-6).
//!
//! GLOBAL tick features (FEA-20): one instance sees every event, which is the
//! only way to correlate two venues' option chains. v1 alignment is DAILY
//! (IBI-6): each UTC day's closing aggregate per venue — OI-weighted mean IV
//! and net delta (Σ delta·OI·multiplier over tickers with usable OI) — feeds
//! the next day's divergence/correlation emission. A venue missing its close
//! suppresses that day entirely (single-venue suppression, IBI-5).
//!
//! Determinism: every instance keeps its own identical state (the
//! SwingSweep duplicate-instance precedent) — no shared cells, same events ⇒
//! same emissions.

use crate::engine::TickFeature;
use mp_core::Venue;
use std::collections::VecDeque;

const DAY_NS: i64 = 86_400_000_000_000;

/// Which output an [`IbitDerivCross`] instance emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossField {
    /// `IV_atm_ibit − IV_atm_deribit` at the daily close.
    IvDivergence,
    /// Pearson correlation of the daily net-delta series at `lag` days.
    FlowCorr(u8),
}

/// One UTC day's closing aggregates for one venue.
#[derive(Debug, Clone, Copy, Default)]
struct DayClose {
    iv_num: f64,
    iv_wt: f64,
    net_delta: f64,
}

impl DayClose {
    fn iv(&self) -> Option<f64> {
        if self.iv_wt > 0.0 {
            let v = self.iv_num / self.iv_wt;
            v.is_finite().then_some(v)
        } else {
            None
        }
    }
}

/// Params (config section `[ibit_cross]`). Disabled unless
/// `deriv_underlying` is set.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct IbitCrossParams {
    /// Deribit (crypto-native) underlying, e.g. "BTC". Empty ⇒ family off.
    #[serde(default)]
    pub deriv_underlying: String,
    /// CBOE ETF underlying (spec 040 default "IBIT").
    #[serde(default = "default_ibit_underlying")]
    pub ibit_underlying: String,
    /// Minimum paired days before flow correlations emit (fail-closed vs a
    /// two-point "correlation").
    #[serde(default = "default_min_overlap")]
    pub min_overlap_days: usize,
    /// IBIT contract multiplier (IBI-2).
    #[serde(default = "default_ibit_multiplier")]
    pub ibit_multiplier: f64,
    /// History length for the correlation window (days).
    #[serde(default = "default_hist_days")]
    pub hist_days: usize,
}

fn default_ibit_underlying() -> String {
    "IBIT".to_owned()
}
fn default_min_overlap() -> usize {
    5
}
fn default_ibit_multiplier() -> f64 {
    100.0
}
fn default_hist_days() -> usize {
    90
}

impl Default for IbitCrossParams {
    fn default() -> Self {
        Self {
            deriv_underlying: String::new(),
            ibit_underlying: default_ibit_underlying(),
            min_overlap_days: default_min_overlap(),
            ibit_multiplier: default_ibit_multiplier(),
            hist_days: default_hist_days(),
        }
    }
}

#[derive(Debug)]
struct CrossCore {
    params: IbitCrossParams,
    cur_date: i64,
    /// Current-day accumulators per side (Cboe, Deribit).
    acc: [DayClose; 2],
    /// Whether each side printed at least one ticker TODAY.
    seen: [bool; 2],
    /// Previous COMPLETED day's close — set ONLY when both sides printed on
    /// that day (strict same-day pairing; a one-sided day pairs with
    /// nothing and carries nothing forward).
    prev: Option<(i64, DayClose, DayClose)>,
    /// Completed paired history (date, ibit close, deriv close).
    hist: VecDeque<(i64, DayClose, DayClose)>,
    /// Set when the LAST consumed event archived a paired day (public
    /// builder consumes this to yield the row).
    just_archived: bool,
}

impl CrossCore {
    fn new(params: IbitCrossParams) -> Self {
        Self {
            params,
            cur_date: i64::MIN,
            acc: [DayClose::default(); 2],
            seen: [false; 2],
            just_archived: false,
            prev: None,
            hist: VecDeque::new(),
        }
    }

    fn observe(
        &mut self,
        venue: Venue,
        underlying: &str,
        mark_iv: f64,
        open_interest: f64,
        delta: Option<f64>,
        recv_ts_ns: i64,
    ) -> Option<()> {
        let p = &self.params;
        let side = match (venue, underlying) {
            (Venue::Cboe, u) if u == p.ibit_underlying => 0usize,
            (Venue::Deribit, u) if u == p.deriv_underlying => 1usize,
            _ => return None,
        };
        let date = recv_ts_ns.div_euclid(DAY_NS);
        if self.cur_date == i64::MIN {
            self.cur_date = date;
        }
        self.just_archived = false;
        if date > self.cur_date {
            // Rollover: if BOTH sides printed on the completed day, archive
            // the aligned close; otherwise the day pairs with nothing.
            if self.seen[0] && self.seen[1] {
                let d = self.cur_date;
                let closed = self.acc;
                self.prev = Some((d, closed[0], closed[1]));
                self.just_archived = true;
                self.hist.push_back((d, closed[0], closed[1]));
                while self.hist.len() > self.params.hist_days {
                    self.hist.pop_front();
                }
            } else {
                self.prev = None;
            }
            self.acc = [DayClose::default(); 2];
            self.seen = [false; 2];
            self.cur_date = date;
        }
        self.seen[side] = true;
        let acc = &mut self.acc[side];
        if mark_iv.is_finite() && open_interest.is_finite() && open_interest > 0.0 {
            acc.iv_num += mark_iv * open_interest;
            acc.iv_wt += open_interest;
        }
        if let Some(d) = delta.filter(|d| d.is_finite()) {
            if open_interest.is_finite() && open_interest > 0.0 {
                let mult = if side == 0 {
                    self.params.ibit_multiplier
                } else {
                    1.0 // Deribit: 1 BTC/ETH per contract
                };
                acc.net_delta += d * open_interest * mult;
            }
        }
        Some(()) // consumed: the (venue, underlying) pair matched
    }

    /// Closing-snapshot read: both venues must have completed the SAME day
    /// (IBI-5 suppression + IBI-6 alignment).
    fn divergence(&self) -> Option<f64> {
        let (_, a, b) = self.prev.as_ref()?;
        let iv_a = a.iv()?;
        let iv_b = b.iv()?;
        Some(iv_a - iv_b)
    }

    /// Pearson correlation of ibit_net_delta[t] vs deribit_net_delta[t−lag]
    /// over overlapping paired dates; requires ≥ `min_overlap_days` pairs.
    fn flow_corr(&self, lag: u8) -> Option<f64> {
        let n = self.hist.len();
        let lag = lag as usize;
        if n < self.params.min_overlap_days || lag >= n {
            return None;
        }
        let items: Vec<(f64, f64)> = (lag..n)
            .map(|t| (self.hist[t].1.net_delta, self.hist[t - lag].2.net_delta))
            .collect();
        let m = items.len();
        if m < self.params.min_overlap_days {
            return None;
        }
        let (mut sa, mut sb) = (0.0, 0.0);
        for (a, b) in &items {
            sa += a;
            sb += b;
        }
        let (ma, mb) = (sa / m as f64, sb / m as f64);
        let mut cov = 0.0;
        let mut va = 0.0;
        let mut vb = 0.0;
        for (a, b) in &items {
            cov += (a - ma) * (b - mb);
            va += (a - ma) * (a - ma);
            vb += (b - mb) * (b - mb);
        }
        let den = (va.sqrt() * vb.sqrt()).max(f64::EPSILON);
        let r = cov / den;
        r.is_finite().then_some(r.clamp(-1.0, 1.0))
    }
}

/// `ibit.cross.iv_divergence` / `ibit.cross.flow_corr.lag{k}` — global tick
/// features (IBI-5). Registered ×4 (one per field). Emission happens ONLY at
/// a UTC-date rollover using the PREVIOUS day's closing aggregates (IBI-6).
pub struct IbitDerivCross {
    core: CrossCore,
    field: CrossField,
}

impl IbitDerivCross {
    pub fn new(params: IbitCrossParams, field: CrossField) -> Self {
        Self {
            core: CrossCore::new(params),
            field,
        }
    }
}

/// One completed UTC day of paired closes (spec 040 IBI-10 row source).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossDayClose {
    /// UTC day index (`recv_ts_ns / 86_400_000_000_000`).
    pub date: i64,
    /// OI-weighted mean mark IV per venue (NaN when no usable print).
    pub ibit_iv: f64,
    pub deriv_iv: f64,
    /// Σ delta·OI·multiplier per venue.
    pub ibit_net_delta: f64,
    pub deriv_net_delta: f64,
}

impl CrossDayClose {
    pub fn divergence(&self) -> f64 {
        self.ibit_iv - self.deriv_iv
    }
}

/// Offline daily-row builder (spec 040 IBI-10): feed events in canonical
/// order; every event that completes a PAIRED day yields exactly one
/// [`DailyClose`]. Correlations are snapshot from history at that moment via
/// [`IbitCrossDaily::corr`].
pub struct IbitCrossDaily {
    core: CrossCore,
}

impl IbitCrossDaily {
    pub fn new(params: IbitCrossParams) -> Self {
        Self {
            core: CrossCore::new(params),
        }
    }

    /// Consume one event; returns true when it belonged to the tracked pair.
    pub fn feed(&mut self, ev: &mp_core::EventEnvelope) -> bool {
        match &ev.body {
            mp_core::MarketEvent::OptionTicker {
                leg,
                mark_iv,
                open_interest,
                greeks,
                ..
            } => self
                .core
                .observe(
                    ev.venue,
                    &leg.underlying,
                    *mark_iv,
                    *open_interest,
                    greeks.as_ref().map(|g| g.delta),
                    ev.recv_ts_ns,
                )
                .is_some(),
            _ => false,
        }
    }

    /// The day just archived by the most recent [`feed`](Self::feed), if it
    /// was a paired completion. NaN IVs mean the day had prints but none were
    /// usable — callers decide whether to skip the row.
    pub fn next_completed(&mut self) -> Option<CrossDayClose> {
        if !self.core.just_archived {
            return None;
        }
        let (date, a, b) = self.core.hist.back().copied()?;
        self.core.just_archived = false;
        Some(CrossDayClose {
            date,
            ibit_iv: a.iv().unwrap_or(f64::NAN),
            deriv_iv: b.iv().unwrap_or(f64::NAN),
            ibit_net_delta: a.net_delta,
            deriv_net_delta: b.net_delta,
        })
    }

    /// Flow correlation at `lag`, snapshotted against current history.
    pub fn corr(&self, lag: u8) -> Option<f64> {
        self.core.flow_corr(lag)
    }
}

impl TickFeature for IbitDerivCross {
    fn id(&self) -> String {
        match self.field {
            CrossField::IvDivergence => "ibit.cross.iv_divergence".into(),
            CrossField::FlowCorr(k) => format!("ibit.cross.flow_corr.lag{k}"),
        }
    }
    fn on_event(&mut self, ev: &mp_core::EventEnvelope) -> Option<f64> {
        // Only OUR two underlyings feed the state; everything else is inert
        // and can never trigger an emission (single-venue / single-pair
        // suppression, IBI-5).
        let matched = match &ev.body {
            mp_core::MarketEvent::OptionTicker {
                leg,
                mark_iv,
                open_interest,
                greeks,
                ..
            } => self
                .core
                .observe(
                    ev.venue,
                    &leg.underlying,
                    *mark_iv,
                    *open_interest,
                    greeks.as_ref().map(|g| g.delta),
                    ev.recv_ts_ns,
                )
                .is_some(),
            _ => false,
        };
        if !matched {
            return None;
        }
        match self.field {
            CrossField::IvDivergence => self.core.divergence(),
            CrossField::FlowCorr(k) => self.core.flow_corr(k),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{EventEnvelope, MarketEvent, OptionGreeks, OptionKind, OptionLeg};

    fn ticker(
        venue: Venue,
        underlying: &str,
        day: i64,
        iv: f64,
        oi: f64,
        delta: f64,
    ) -> EventEnvelope {
        let sym = mp_core::SymbolId(match venue {
            Venue::Cboe => 7,
            _ => 8,
        });
        let leg = OptionLeg {
            underlying: underlying.to_owned(),
            strike: 100.0,
            expiry_ts_ns: 0,
            kind: OptionKind::Call,
        };
        EventEnvelope::new(
            venue,
            sym,
            day * DAY_NS + 3_600_000_000_000,
            day * DAY_NS + 3_600_000_000_000,
            1,
            MarketEvent::OptionTicker {
                leg,
                mark_iv: iv,
                mark_price: 5.0,
                underlying_price: 100.0,
                open_interest: oi,
                greeks: Some(OptionGreeks {
                    delta,
                    gamma: 0.0,
                    theta: 0.0,
                    vega: 0.0,
                }),
            },
        )
    }

    fn params(min_overlap: usize) -> IbitCrossParams {
        IbitCrossParams {
            deriv_underlying: "BTC".into(),
            ibit_underlying: "IBIT".into(),
            min_overlap_days: min_overlap,
            ibit_multiplier: 100.0,
            hist_days: 64,
        }
    }

    fn feed_both_days(f: &mut IbitDerivCross, start: i64, days: i64, ramp: bool) {
        for d in start..start + days {
            // Ramps key on the ABSOLUTE day so split feeds stay globally
            // monotonic (a sawtooth would break lag-correlation exactness).
            let di = if ramp { d as f64 * 0.01 } else { 0.0 };
            let dd = if ramp { d as f64 * 0.002 } else { 0.0 };
            f.on_event(&ticker(Venue::Cboe, "IBIT", d, 0.40 + di, 1000.0, 0.5 + di));
            f.on_event(&ticker(
                Venue::Deribit,
                "BTC",
                d,
                0.55 + dd,
                100.0,
                0.4 + dd,
            ));
        }
    }

    #[test]
    fn ibi_5_suppressed_until_both_venues_share_a_completed_day() {
        let mut f = IbitDerivCross::new(params(2), CrossField::IvDivergence);
        // Only CBOE on days 1-2: one-sided days pair with nothing.
        for d in 1..=2 {
            f.on_event(&ticker(Venue::Cboe, "IBIT", d, 0.40, 1000.0, 0.5));
        }
        // Deribit joins on day 3 - the first SHARED day; nothing readable
        // while it is still open.
        f.on_event(&ticker(Venue::Deribit, "BTC", 3, 0.55, 100.0, 0.4));
        f.on_event(&ticker(Venue::Cboe, "IBIT", 3, 0.40, 1000.0, 0.5));
        assert!(f.core.divergence().is_none(), "day 3 still open");
        // Day-4 events complete day 3 -> its closes pair.
        f.on_event(&ticker(Venue::Cboe, "IBIT", 4, 9.99, 1.0, 0.5));
        f.on_event(&ticker(Venue::Deribit, "BTC", 4, 9.99, 1.0, 0.4));
        let div = f.core.divergence().expect("shared day completed");
        assert!((div - (0.40 - 0.55)).abs() < 1e-12);
    }

    #[test]
    fn ibi_6_divergence_is_previous_days_closing_aggregate() {
        let mut f = IbitDerivCross::new(params(2), CrossField::IvDivergence);
        feed_both_days(&mut f, 1, 2, false);
        // Day-3 event rolls day 2 over; divergence reads DAY-2 closes.
        f.on_event(&ticker(Venue::Cboe, "IBIT", 3, 9.99, 1.0, 0.5));
        let div = f.core.divergence().unwrap();
        assert!((div - (0.40 - 0.55)).abs() < 1e-12, "got {div}");
    }

    #[test]
    fn ibi_6_daily_iv_is_oi_weighted() {
        let mut f = IbitDerivCross::new(params(2), CrossField::IvDivergence);
        // Day-1 CBOE prints: (0.30 @ oi 300), (0.50 @ oi 100) -> weighted
        // mean = (90 + 50)/400 = 0.35, not the 0.40 arithmetic mean.
        f.on_event(&ticker(Venue::Cboe, "IBIT", 1, 0.30, 300.0, 0.5));
        f.on_event(&ticker(Venue::Cboe, "IBIT", 1, 0.50, 100.0, 0.5));
        f.on_event(&ticker(Venue::Deribit, "BTC", 1, 0.55, 100.0, 0.4));
        f.on_event(&ticker(Venue::Cboe, "IBIT", 2, 9.99, 1.0, 0.5));
        f.on_event(&ticker(Venue::Deribit, "BTC", 2, 9.99, 1.0, 0.4));
        let div = f.core.divergence().unwrap();
        assert!((div - (0.35 - 0.55)).abs() < 1e-12, "got {div}");
    }

    #[test]
    fn ibi_2_net_delta_uses_ibit_multiplier_100() {
        let mut f = IbitDerivCross::new(params(1), CrossField::FlowCorr(0));
        feed_both_days(&mut f, 1, 2, false);
        let (_, ibit_close, deriv_close) = f.core.hist[0];
        // IBIT: delta 0.5 x OI 1000 x mult 100 = 50_000 (NOT 500).
        assert_eq!(ibit_close.net_delta, 50_000.0);
        // Deribit: delta 0.4 x OI 100 x mult 1 = 40.
        assert_eq!(deriv_close.net_delta, 40.0);
    }

    #[test]
    fn ibi_5_flow_corr_gated_on_min_overlap_and_values_exact() {
        // Perfectly correlated ramps: Pearson = +1 at every lag once the
        // overlap minimum is met.
        let mut f = IbitDerivCross::new(params(5), CrossField::FlowCorr(0));
        feed_both_days(&mut f, 1, 3, true);
        assert!(f.core.flow_corr(0).is_none(), "3 pairs < min_overlap 5");
        feed_both_days(&mut f, 4, 2, true);
        assert!(f.core.flow_corr(0).is_none(), "4 pairs still < 5");
        // Days 6-9 of events complete days 5..8 -> hist holds 8 paired days.
        feed_both_days(&mut f, 6, 4, true);
        let r0 = f.core.flow_corr(0).expect("8 pairs now");
        assert!((r0 - 1.0).abs() < 1e-9);
        let r2 = f.core.flow_corr(2).expect("lag 2 with 6+ usable pairs");
        assert!((r2 - 1.0).abs() < 1e-9, "global ramps correlate at any lag");

        // Anti-correlated construction flips the sign exactly.
        let mut g = IbitDerivCross::new(params(3), CrossField::FlowCorr(0));
        for k in 0..3 {
            let d = 1 + k;
            let up = ticker(Venue::Cboe, "IBIT", d, 0.4, 1000.0, 0.5 + k as f64 * 0.01);
            let dn = ticker(
                Venue::Deribit,
                "BTC",
                d,
                0.55,
                100.0,
                0.4 - k as f64 * 0.002,
            );
            g.on_event(&up);
            g.on_event(&dn);
        }
        // Day-4 events complete day 3 -> hist holds the 3 paired days.
        g.on_event(&ticker(Venue::Cboe, "IBIT", 4, 0.4, 1000.0, 0.5));
        g.on_event(&ticker(Venue::Deribit, "BTC", 4, 0.55, 100.0, 0.4));
        let r = g.core.flow_corr(0).unwrap();
        assert!((r + 1.0).abs() < 1e-9, "anti-correlated series -> -1");
    }

    #[test]
    fn ibit_cross_ids_and_engine_registration() {
        let p = params(5);
        let f_up = IbitDerivCross::new(p.clone(), CrossField::IvDivergence);
        let f_l2 = IbitDerivCross::new(p.clone(), CrossField::FlowCorr(2));
        assert_eq!(f_up.id(), "ibit.cross.iv_divergence");
        assert_eq!(f_l2.id(), "ibit.cross.flow_corr.lag2");
        // Engine registration: ids exist only when deriv_underlying set.
        let on =
            crate::FeaturesConfig::from_toml("[ibit_cross]\nderiv_underlying = \"BTC\"\n").unwrap();
        let e = crate::engine_from_config(&on).unwrap();
        assert!(e.name_to_id("ibit.cross.iv_divergence").is_some());
        assert!(e.name_to_id("ibit.cross.flow_corr.lag0").is_some());
        assert!(e.name_to_id("ibit.cross.flow_corr.lag2").is_some());
        let off = crate::FeaturesConfig::from_toml("").unwrap();
        let e2 = crate::engine_from_config(&off).unwrap();
        assert!(e2.name_to_id("ibit.cross.iv_divergence").is_none());
    }
}
