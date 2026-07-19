//! v1 feature catalog (spec 004 §Feature catalog). Ids and formulas are
//! normative. Each feature is a pure function of the events it has seen.

use crate::bar::Bar;
use crate::engine::{BarFeature, TickFeature};
use mp_core::{BookMirror, EventEnvelope, MarketEvent, Side, Venue};
use std::collections::VecDeque;

// ---- order flow -------------------------------------------------------------

/// `cvd.{venue}` — cumulative signed trade quantity (buy +qty, sell −qty).
pub struct Cvd {
    venue: Venue,
    cvd: f64,
}
impl Cvd {
    pub fn new(venue: Venue) -> Self {
        Self { venue, cvd: 0.0 }
    }
}
impl TickFeature for Cvd {
    fn id(&self) -> String {
        format!("cvd.{}", self.venue.slug())
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let MarketEvent::Trade { qty, side, .. } = ev.body {
            self.cvd += match side {
                Side::Buy => qty,
                Side::Sell => -qty,
            };
            Some(self.cvd)
        } else {
            None
        }
    }
}

/// `whale_print.{venue}` — signed notional for trades ≥ `floor_usd` on one venue.
/// Default venue is Hyperliquid (large prints / liquidations edge on HL tape).
pub struct WhalePrint {
    floor_usd: f64,
    venue: Venue,
}
impl WhalePrint {
    /// Hyperliquid whale tracker (product default).
    pub fn new(floor_usd: f64) -> Self {
        Self::for_venue(floor_usd, Venue::Hyperliquid)
    }
    pub fn for_venue(floor_usd: f64, venue: Venue) -> Self {
        Self { floor_usd, venue }
    }
}
impl TickFeature for WhalePrint {
    fn id(&self) -> String {
        format!("whale_print.{}", self.venue.slug())
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if ev.venue != self.venue {
            return None;
        }
        if let MarketEvent::Trade {
            price, qty, side, ..
        } = ev.body
        {
            let notional = price * qty;
            if notional >= self.floor_usd {
                let signed = match side {
                    Side::Buy => notional,
                    Side::Sell => -notional,
                };
                return Some(signed);
            }
        }
        None
    }
}

/// `liq.cluster` — rolling Σ signed liquidation notional within
/// `window_ns`; emits when the window sum reaches `min_notional` in magnitude.
pub struct LiqCluster {
    window_ns: i64,
    min_notional: f64,
    buf: VecDeque<(i64, f64)>,
    sum: f64,
}
impl LiqCluster {
    pub fn new(window_ns: i64, min_notional: f64) -> Self {
        Self {
            window_ns,
            min_notional,
            buf: VecDeque::new(),
            sum: 0.0,
        }
    }
}
impl TickFeature for LiqCluster {
    fn id(&self) -> String {
        "liq.cluster".into()
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let MarketEvent::Liquidation { price, qty, side } = ev.body {
            let notional = price * qty;
            // Negative when longs are liquidated (Sell-side liquidation orders).
            let signed = match side {
                Side::Buy => notional,
                Side::Sell => -notional,
            };
            self.buf.push_back((ev.recv_ts_ns, signed));
            self.sum += signed;
            while let Some(&(ts, v)) = self.buf.front() {
                if ev.recv_ts_ns - ts > self.window_ns {
                    self.sum -= v;
                    self.buf.pop_front();
                } else {
                    break;
                }
            }
            if self.sum.abs() >= self.min_notional {
                return Some(self.sum);
            }
        }
        None
    }
}

// ---- derivatives passthrough ------------------------------------------------

/// `funding.{venue}` — passthrough of the funding rate.
pub struct FundingPassthrough {
    venue_slug: String,
}
impl FundingPassthrough {
    pub fn new(venue_slug: &str) -> Self {
        Self {
            venue_slug: venue_slug.to_owned(),
        }
    }
}
impl TickFeature for FundingPassthrough {
    fn id(&self) -> String {
        format!("funding.{}", self.venue_slug)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let MarketEvent::Funding { rate, .. } = ev.body {
            Some(rate)
        } else {
            None
        }
    }
}

/// `oi.delta` — change in open interest vs the previous reading.
#[derive(Default)]
pub struct OiDelta {
    last: Option<f64>,
}
impl OiDelta {
    pub fn new() -> Self {
        Self::default()
    }
}
impl TickFeature for OiDelta {
    fn id(&self) -> String {
        "oi.delta".into()
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let MarketEvent::OpenInterest { oi_contracts, .. } = ev.body {
            let d = self.last.map(|p| oi_contracts - p);
            self.last = Some(oi_contracts);
            return d; // None on the first reading (no delta yet)
        }
        None
    }
}

// ---- liquidity (book) -------------------------------------------------------

/// `imbalance.top` — top-of-book imbalance `(bid−ask)/(bid+ask)`. Silent while
/// the book is stale (FEA-8).
#[derive(Default)]
pub struct BookImbalance {
    book: BookMirror,
}
impl BookImbalance {
    pub fn new() -> Self {
        Self::default()
    }
}
impl TickFeature for BookImbalance {
    fn id(&self) -> String {
        "imbalance.top".into()
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let changed = self.book.apply(&ev.body);
        if !changed {
            return None;
        }
        match (self.book.best_bid(), self.book.best_ask()) {
            (Some((_, bq)), Some((_, aq))) if bq + aq > 0.0 => Some((bq - aq) / (bq + aq)),
            _ => None, // stale or one-sided ⇒ silent
        }
    }
}

// ---- bar features -----------------------------------------------------------

/// `delta.bar.{tf}` — per-bar buy_vol − sell_vol.
pub struct BarDelta {
    tf: String,
}
impl BarDelta {
    pub fn new(tf: &str) -> Self {
        Self { tf: tf.to_owned() }
    }
}
impl BarFeature for BarDelta {
    fn id(&self) -> String {
        format!("delta.bar.{}", self.tf)
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        Some(bar.buy_vol - bar.sell_vol)
    }
}

/// `vol.rv.{tf}.{w}` — realized vol: √(Σ r² over the last `w` bar returns).
pub struct RealizedVol {
    tf: String,
    w: usize,
    last_close: Option<f64>,
    rets: VecDeque<f64>,
}
impl RealizedVol {
    pub fn new(tf: &str, w: usize) -> Self {
        Self {
            tf: tf.to_owned(),
            w,
            last_close: None,
            rets: VecDeque::new(),
        }
    }
}
impl BarFeature for RealizedVol {
    fn id(&self) -> String {
        format!("vol.rv.{}.{}", self.tf, self.w)
    }
    fn warm(&self) -> bool {
        self.rets.len() >= self.w
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        if let Some(prev) = self.last_close {
            if prev > 0.0 && bar.close > 0.0 {
                let r = (bar.close / prev).ln();
                self.rets.push_back(r);
                while self.rets.len() > self.w {
                    self.rets.pop_front();
                }
            }
        }
        self.last_close = Some(bar.close);
        if self.warm() {
            let ss: f64 = self.rets.iter().map(|r| r * r).sum();
            Some(ss.sqrt())
        } else {
            None
        }
    }
}

/// `breakout.{n}` — Donchian: +1 if close exceeds the prior `n`-bar high, −1 if
/// below the prior `n`-bar low, else 0. Warm after `n` bars.
pub struct DonchianBreakout {
    n: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
}
impl DonchianBreakout {
    pub fn new(n: usize) -> Self {
        Self {
            n,
            highs: VecDeque::new(),
            lows: VecDeque::new(),
        }
    }
}
impl BarFeature for DonchianBreakout {
    fn id(&self) -> String {
        format!("breakout.{}", self.n)
    }
    fn warm(&self) -> bool {
        self.highs.len() >= self.n
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        let signal = if self.highs.len() >= self.n {
            let hi = self.highs.iter().cloned().fold(f64::MIN, f64::max);
            let lo = self.lows.iter().cloned().fold(f64::MAX, f64::min);
            if bar.close > hi {
                1.0
            } else if bar.close < lo {
                -1.0
            } else {
                0.0
            }
        } else {
            0.0
        };
        self.highs.push_back(bar.high);
        self.lows.push_back(bar.low);
        while self.highs.len() > self.n {
            self.highs.pop_front();
            self.lows.pop_front();
        }
        if self.warm() {
            Some(signal)
        } else {
            None
        }
    }
}
