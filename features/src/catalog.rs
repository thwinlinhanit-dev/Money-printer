//! v1 feature catalog (spec 004 §Feature catalog). Ids and formulas are
//! normative. Each feature is a pure function of the events it has seen.

use crate::bar::Bar;
use crate::engine::{BarFeature, TickFeature};
use mp_core::{BookMirror, EventEnvelope, MarketEvent, Side, Venue};
use std::collections::VecDeque;

// ---- order flow -------------------------------------------------------------

/// Renders a footprint tf into an id-safe label (`60s`, `5m`).
pub fn tf_label(tf_ns: i64) -> String {
    let s = tf_ns.max(1) / 1_000_000_000;
    if s % 60 == 0 && s >= 300 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// Shared per-bar, per-size-bucket accumulator behind the `footprint.*`
/// features (spec 004 §Order flow). Trades are bucketed by USD notional
/// (price × qty); the closed bar's totals are released when a trade lands in a
/// later time bucket — the same rollover rule as `BarBuilder`, so
/// `footprint.delta.{tf}.{bucket}` agrees with `delta.bar.{tf}` when summed
/// across buckets. Pure function of events: no wall clock (PD-3/FEA-2).
#[derive(Debug, Clone)]
pub struct FootprintAccumulator {
    tf_ns: i64,
    min_usd: f64,
    max_usd: f64,
    bar_start: Option<i64>,
    buy_vol: f64,
    sell_vol: f64,
}

impl FootprintAccumulator {
    pub fn new(tf_ns: i64, min_usd: f64, max_usd: f64) -> Self {
        Self {
            tf_ns: tf_ns.max(1),
            min_usd,
            max_usd,
            bar_start: None,
            buy_vol: 0.0,
            sell_vol: 0.0,
        }
    }

    /// Returns `Some((signed_delta, buy, sell))` of the just-closed bar when a
    /// trade rolls the bucket, else `None` (in-bucket accumulation). A trade
    /// priced out of this bucket's range still rolls the bucket (the closed
    /// bar is real), it just does not contribute to the new one.
    pub fn on_trade(
        &mut self,
        ts_ns: i64,
        price: f64,
        qty: f64,
        side: Side,
    ) -> Option<(f64, f64, f64)> {
        let notional = price * qty;
        let in_bucket = notional >= self.min_usd && notional < self.max_usd;
        let bucket = ts_ns.div_euclid(self.tf_ns) * self.tf_ns;
        let mut closed = None;
        if let Some(cur) = self.bar_start {
            if cur != bucket {
                closed = Some((self.buy_vol - self.sell_vol, self.buy_vol, self.sell_vol));
                self.bar_start = None;
                self.buy_vol = 0.0;
                self.sell_vol = 0.0;
            }
        }
        if !in_bucket {
            return closed;
        }
        if self.bar_start.is_none() {
            self.bar_start = Some(bucket);
        }
        match side {
            Side::Buy => self.buy_vol += qty,
            Side::Sell => self.sell_vol += qty,
        }
        closed
    }
}

/// `footprint.delta.{tf}.{bucket}` — per-bar signed orderflow delta
/// (buy qty − sell qty) restricted to one size bucket (notional USD). Emitted
/// at bucket rollover, exactly like a bar-close feature (no intra-bar repaint).
pub struct FootprintDelta {
    tf_ns: i64,
    bucket: String,
    acc: FootprintAccumulator,
}
impl FootprintDelta {
    pub fn new(tf_ns: i64, bucket: &str, min_usd: f64, max_usd: f64) -> Self {
        Self {
            tf_ns,
            bucket: bucket.to_owned(),
            acc: FootprintAccumulator::new(tf_ns, min_usd, max_usd),
        }
    }
}
impl TickFeature for FootprintDelta {
    fn id(&self) -> String {
        format!("footprint.delta.{}.{}", tf_label(self.tf_ns), self.bucket)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let Some((price, qty, side, _, _)) = ev.body.trade_view() {
            self.acc
                .on_trade(ev.recv_ts_ns, price, qty, side)
                .map(|(d, _, _)| d)
        } else {
            None
        }
    }
}

/// `footprint.imb.{tf}.{bucket}` — per-bar imbalance (buy − sell)/(buy + sell)
/// of one size bucket. Ranges [−1, 1]; 1 = the bucket's entire bar flow was
/// buys. Silent while the closed bar had no bucket flow (no 0/0 emission).
pub struct FootprintImbalance {
    tf_ns: i64,
    bucket: String,
    acc: FootprintAccumulator,
}
impl FootprintImbalance {
    pub fn new(tf_ns: i64, bucket: &str, min_usd: f64, max_usd: f64) -> Self {
        Self {
            tf_ns,
            bucket: bucket.to_owned(),
            acc: FootprintAccumulator::new(tf_ns, min_usd, max_usd),
        }
    }
}
impl TickFeature for FootprintImbalance {
    fn id(&self) -> String {
        format!("footprint.imb.{}.{}", tf_label(self.tf_ns), self.bucket)
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let Some((price, qty, side, _, _)) = ev.body.trade_view() {
            self.acc
                .on_trade(ev.recv_ts_ns, price, qty, side)
                .and_then(|(_, b, s)| {
                    if b + s > 0.0 {
                        Some((b - s) / (b + s))
                    } else {
                        None
                    }
                })
        } else {
            None
        }
    }
}

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
        if let Some((_, qty, side, _, _)) = ev.body.trade_view() {
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

/// `funding.rate` — passthrough of funding rate events (any venue).
pub struct FundingRate;
impl FundingRate {
    pub fn new() -> Self {
        Self
    }
}
impl Default for FundingRate {
    fn default() -> Self {
        Self::new()
    }
}
impl TickFeature for FundingRate {
    fn id(&self) -> String {
        "funding.rate".into()
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let MarketEvent::Funding { rate, .. } = ev.body {
            Some(rate)
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
        if let Some((price, qty, side, _, _)) = ev.body.trade_view() {
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

// ---- liquidation flow (COL-29 real liq source, spec 004 §Liquidation flow) -

/// `liq.vol_buy` / `liq.vol_sell` — rolling Σ liquidation notional for one
/// side within `window_ns`. Emits the rolling sum each time a liquidation of
/// that side arrives (a buy-side liquidation is the venue buying back the
/// liquidated short — volume flow into the book; sell-side is longs being
/// dumped). Mirrors the `LiqCluster` window mechanics but per-side and
/// absolute, so the two sides can diverge instead of netting.
pub struct LiqVol {
    window_ns: i64,
    side: Side,
    buf: VecDeque<(i64, f64)>,
    sum: f64,
}
impl LiqVol {
    pub fn new(window_ns: i64, side: Side) -> Self {
        Self {
            window_ns,
            side,
            buf: VecDeque::new(),
            sum: 0.0,
        }
    }
}
impl TickFeature for LiqVol {
    fn id(&self) -> String {
        match self.side {
            Side::Buy => "liq.vol_buy".into(),
            Side::Sell => "liq.vol_sell".into(),
        }
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let MarketEvent::Liquidation { price, qty, side } = ev.body {
            if side == self.side {
                let notional = price * qty;
                self.buf.push_back((ev.recv_ts_ns, notional));
                self.sum += notional;
                while let Some(&(ts, v)) = self.buf.front() {
                    if ev.recv_ts_ns - ts > self.window_ns {
                        self.sum -= v;
                        self.buf.pop_front();
                    } else {
                        break;
                    }
                }
                return Some(self.sum);
            }
        }
        None
    }
}

/// `liq.rate` — rolling liquidation event rate (liquidations/second) within
/// `window_ns`. Emits on each liquidation; the intensity reading complementing
/// the notional sums (`liq.vol_*`) — many small liquidations vs few big ones.
pub struct LiqRate {
    window_ns: i64,
    ts: VecDeque<i64>,
}
impl LiqRate {
    pub fn new(window_ns: i64) -> Self {
        Self {
            window_ns,
            ts: VecDeque::new(),
        }
    }
}
impl TickFeature for LiqRate {
    fn id(&self) -> String {
        "liq.rate".into()
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if matches!(ev.body, MarketEvent::Liquidation { .. }) {
            self.ts.push_back(ev.recv_ts_ns);
            while let Some(&t) = self.ts.front() {
                if ev.recv_ts_ns - t > self.window_ns {
                    self.ts.pop_front();
                } else {
                    break;
                }
            }
            let secs = self.window_ns as f64 / 1_000_000_000.0;
            return Some(self.ts.len() as f64 / secs);
        }
        None
    }
}

/// `liq.dist` — liquidation price-distance from mid, in bps, at the moment
/// the liquidation prints: `|liq_price − mid| / mid × 10_000`. How far from
/// fair value the cascade is hitting (a wide gap = the book got pushed, or
/// the venue's marking is far from the touch). Silent while the book is stale
/// or one-sided (FEA-8).
pub struct LiqDist {
    book: BookMirror,
}
impl LiqDist {
    pub fn new() -> Self {
        Self {
            book: BookMirror::new(),
        }
    }
}
impl Default for LiqDist {
    fn default() -> Self {
        Self::new()
    }
}
impl TickFeature for LiqDist {
    fn id(&self) -> String {
        "liq.dist".into()
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        self.book.apply(&ev.body);
        if let MarketEvent::Liquidation { price, .. } = ev.body {
            if self.book.is_stale() {
                return None; // FEA-8: never read a gapped book
            }
            let mid = self.book.mid()?;
            if mid > 0.0 {
                return Some((price - mid).abs() / mid * 10_000.0);
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

/// Which [`BookDepth`] statistic a band computes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BookDepthKind {
    /// `(Σbids − Σasks)/(Σbids + Σasks)` within the band — the depth gauge.
    Gauge,
    /// `Σbids + Σasks` within the band — the liquidity thickness.
    Total,
}

/// `book.depth.{pct}` / `book.depth_total.{pct}` — liquidity within `pct` of
/// mid (Cryexc/OpenMarket liquidity-band stats, spec 004 §Liquidity). Sums
/// resting notional on each side for levels within `mid × pct`; Gauge emits
/// the signed imbalance, Total emits the combined thickness. Silent while the
/// book is stale or one-sided (FEA-8).
pub struct BookDepth {
    book: BookMirror,
    pct: f64,
    kind: BookDepthKind,
}
impl BookDepth {
    pub fn new(pct: f64, kind: BookDepthKind) -> Self {
        Self {
            book: BookMirror::new(),
            pct,
            kind,
        }
    }

    /// Band label in percent (0.005 → "0.5", 0.02 → "2", 0.1 → "10").
    fn label(pct: f64) -> String {
        format!("{}", pct * 100.0)
    }
}
impl TickFeature for BookDepth {
    fn id(&self) -> String {
        let p = Self::label(self.pct);
        match self.kind {
            BookDepthKind::Gauge => format!("book.depth.{p}"),
            BookDepthKind::Total => format!("book.depth_total.{p}"),
        }
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if !self.book.apply(&ev.body) {
            return None;
        }
        if self.book.is_stale() {
            return None; // FEA-8: never read a gapped book
        }
        let mid = self.book.mid()?;
        let band = mid * self.pct;
        let (mut bsum, mut asum) = (0.0, 0.0);
        for (p, q) in self.book.bids() {
            if p >= mid - band {
                bsum += p * q;
            }
        }
        for (p, q) in self.book.asks() {
            if p <= mid + band {
                asum += p * q;
            }
        }
        match self.kind {
            BookDepthKind::Gauge => {
                let t = bsum + asum;
                if t > 0.0 {
                    Some((bsum - asum) / t)
                } else {
                    None
                }
            }
            BookDepthKind::Total => Some(bsum + asum),
        }
    }
}

/// `microprice.{venue}` — microprice of the top of book
/// `(bid_qty × ask_price + ask_qty × bid_price) / (bid_qty + ask_qty)`: the
/// qty-weighted mid the next trade is expected to land on (best-ask fills at
/// bid price when ask qty dominates, and vice versa). Emits when the book
/// changes; silent while the book is stale or one-sided (FEA-8).
pub struct Microprice {
    book: BookMirror,
    venue: Venue,
}
impl Microprice {
    pub fn for_venue(venue: Venue) -> Self {
        Self {
            book: BookMirror::new(),
            venue,
        }
    }
}
impl TickFeature for Microprice {
    fn id(&self) -> String {
        format!("microprice.{}", self.venue.slug())
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if ev.venue != self.venue || !self.book.apply(&ev.body) {
            return None;
        }
        if self.book.is_stale() {
            return None; // FEA-8: never read a gapped book
        }
        let (bid_px, bid_qty) = self.book.best_bid()?;
        let (ask_px, ask_qty) = self.book.best_ask()?;
        let total = bid_qty + ask_qty;
        if total > 0.0 {
            Some((bid_qty * ask_px + ask_qty * bid_px) / total)
        } else {
            None
        }
    }
}

/// `spread.bp.{venue}` — top-of-book quoted spread in basis points of mid:
/// `(ask − bid) / mid × 10_000`. Wide-spread regimes destroy short-horizon
/// predictability (microstructure research, arXiv 2602.00776) — the regime
/// feature below flags them for gating. Emits when the book changes; silent
/// while stale or one-sided (FEA-8).
pub struct SpreadBp {
    book: BookMirror,
    venue: Venue,
}
impl SpreadBp {
    pub fn for_venue(venue: Venue) -> Self {
        Self {
            book: BookMirror::new(),
            venue,
        }
    }
}
impl TickFeature for SpreadBp {
    fn id(&self) -> String {
        format!("spread.bp.{}", self.venue.slug())
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if ev.venue != self.venue || !self.book.apply(&ev.body) {
            return None;
        }
        if self.book.is_stale() {
            return None; // FEA-8: never read a gapped book
        }
        let (bid_px, _) = self.book.best_bid()?;
        let (ask_px, _) = self.book.best_ask()?;
        let mid = self.book.mid()?;
        if mid > 0.0 {
            Some((ask_px - bid_px) / mid * 10_000.0)
        } else {
            None
        }
    }
}

/// `spread.regime.{venue}` — wide-spread regime flag: 1.0 when the quoted
/// spread (in bps of mid) is ≥ `wide_bps`, else 0.0. Pure feature-regime
/// gate for strategies (microstructure predictability collapses in wide
/// regimes); threshold is config (`microstructure.wide_spread_bps`).
/// Emits when the book changes; silent while stale or one-sided (FEA-8).
pub struct SpreadRegime {
    book: BookMirror,
    venue: Venue,
    wide_bps: f64,
}
impl SpreadRegime {
    pub fn for_venue(venue: Venue, wide_bps: f64) -> Self {
        Self {
            book: BookMirror::new(),
            venue,
            wide_bps: wide_bps.max(0.0),
        }
    }
}
impl TickFeature for SpreadRegime {
    fn id(&self) -> String {
        format!("spread.regime.{}", self.venue.slug())
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if ev.venue != self.venue || !self.book.apply(&ev.body) {
            return None;
        }
        if self.book.is_stale() {
            return None; // FEA-8: never read a gapped book
        }
        let (bid_px, _) = self.book.best_bid()?;
        let (ask_px, _) = self.book.best_ask()?;
        let mid = self.book.mid()?;
        if mid > 0.0 {
            let bp = (ask_px - bid_px) / mid * 10_000.0;
            Some(if bp >= self.wide_bps { 1.0 } else { 0.0 })
        } else {
            None
        }
    }
}

/// `tape.bps_delta` — per-trade price change vs the previous trade on the
/// symbol, in basis points. Emitted only when |Δ| ≥ `min_bps` (default 0.5) —
/// OpenMarket's tape hides sub-half-bps ticks as noise; first trade of a
/// symbol emits nothing (no prior price).
pub struct TapeBpsDelta {
    last: Option<f64>,
    min_bps: f64,
}
impl TapeBpsDelta {
    pub fn new(min_bps: f64) -> Self {
        Self {
            last: None,
            min_bps,
        }
    }
}
impl Default for TapeBpsDelta {
    fn default() -> Self {
        Self::new(0.5)
    }
}
impl TickFeature for TapeBpsDelta {
    fn id(&self) -> String {
        "tape.bps_delta".into()
    }
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        if let Some((price, _, _, _, _)) = ev.body.trade_view() {
            let d = match self.last {
                Some(p) if p > 0.0 => (price - p) / p * 10_000.0,
                _ => 0.0,
            };
            self.last = Some(price);
            if d.abs() >= self.min_bps {
                Some(d)
            } else {
                None
            }
        } else {
            None
        }
    }
}

/// `tape.tps.{tf}` — trades per second for a closed bar (OpenMarket TPS).
pub struct TapeTps {
    tf: String,
    tf_secs: f64,
}
impl TapeTps {
    pub fn new(tf: &str, tf_secs: f64) -> Self {
        Self {
            tf: tf.to_owned(),
            tf_secs,
        }
    }
}
impl BarFeature for TapeTps {
    fn id(&self) -> String {
        format!("tape.tps.{}", self.tf)
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        Some(bar.n_trades as f64 / self.tf_secs.max(1e-9))
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

// ---- footprint signal catalog (spec 049) -----------------------------------

/// `footprint.volume.bubble.{tf}` — volume percentile rank over a rolling
/// window. High values (>80) indicate volume climaxes; low values (<20)
/// indicate volume dry-ups. Emits on bar close.
pub struct VolumeBubble {
    tf: String,
    window: usize,
    volumes: VecDeque<f64>,
}
impl VolumeBubble {
    pub fn new(tf: &str, window: usize) -> Self {
        Self {
            tf: tf.to_owned(),
            window,
            volumes: VecDeque::new(),
        }
    }
}
impl BarFeature for VolumeBubble {
    fn id(&self) -> String {
        format!("footprint.volume.bubble.{}", self.tf)
    }
    fn warm(&self) -> bool {
        self.volumes.len() >= self.window
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.volumes.push_back(bar.vol);
        while self.volumes.len() > self.window {
            self.volumes.pop_front();
        }
        if !self.warm() {
            return None;
        }
        // Count how many volumes in window are less than current
        let current = bar.vol;
        let rank = self.volumes.iter().filter(|&&v| v < current).count() as f64;
        Some(rank / self.window as f64 * 100.0)
    }
}

/// `footprint.market.profile.poc.{tf}` — Point of Control: price level with
/// highest volume over a rolling window. Emits on bar close.
pub struct MarketProfilePoc {
    tf: String,
    window: usize,
    bars: VecDeque<Bar>,
    bucket_width: f64,
}
impl MarketProfilePoc {
    pub fn new(tf: &str, window: usize, bucket_width: f64) -> Self {
        Self {
            tf: tf.to_owned(),
            window,
            bars: VecDeque::new(),
            bucket_width,
        }
    }
}
impl BarFeature for MarketProfilePoc {
    fn id(&self) -> String {
        format!("footprint.market.profile.poc.{}", self.tf)
    }
    fn warm(&self) -> bool {
        self.bars.len() >= self.window
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.bars.push_back(bar.clone());
        while self.bars.len() > self.window {
            self.bars.pop_front();
        }
        if !self.warm() {
            return None;
        }
        // Build volume profile: distribute volume across price buckets
        let mut profile: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
        let bucket_size = self.bucket_width;
        for b in self.bars.iter() {
            let mid = (b.high + b.low) / 2.0;
            let bucket = (mid / bucket_size).floor() as i64;
            *profile.entry(bucket).or_insert(0.0) += b.vol;
        }
        // Find POC (bucket with highest volume). DETERMINISTIC tie-break
        // (PD-3, A-5 audit 2026-09-02): a HashMap iteration order is
        // per-process randomized, so `.max_by` on ties picked a different POC
        // price run-to-run for identical input. Sort buckets ascending and
        // scan with a strict `>` so an equal-volume tie always resolves to
        // the LOWEST bucket — stable across runs, matching the VAH/VAL path
        // (which sorts into a Vec first).
        let mut buckets: Vec<(i64, f64)> = profile.into_iter().collect();
        buckets.sort_unstable_by_key(|&(bucket, _)| bucket);
        let mut poc = buckets[0];
        for &(bucket, vol) in buckets.iter().skip(1) {
            if vol > poc.1 {
                poc = (bucket, vol);
            }
        }
        Some(poc.0 as f64 * bucket_size + bucket_size / 2.0)
    }
}

/// `footprint.market.profile.vah.{tf}` — Value Area High: upper boundary of
/// 70% volume concentration. Emits on bar close.
pub struct MarketProfileVah {
    tf: String,
    window: usize,
    bars: VecDeque<Bar>,
    bucket_width: f64,
}
impl MarketProfileVah {
    pub fn new(tf: &str, window: usize, bucket_width: f64) -> Self {
        Self {
            tf: tf.to_owned(),
            window,
            bars: VecDeque::new(),
            bucket_width,
        }
    }
}
impl BarFeature for MarketProfileVah {
    fn id(&self) -> String {
        format!("footprint.market.profile.vah.{}", self.tf)
    }
    fn warm(&self) -> bool {
        self.bars.len() >= self.window
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.bars.push_back(bar.clone());
        while self.bars.len() > self.window {
            self.bars.pop_front();
        }
        if !self.warm() {
            return None;
        }
        // Build volume profile
        let mut profile: Vec<(f64, f64)> = Vec::new();
        let bucket_size = self.bucket_width;
        for b in self.bars.iter() {
            let mid = (b.high + b.low) / 2.0;
            let bucket = (mid / bucket_size).floor() as f64 * bucket_size + bucket_size / 2.0;
            if let Some(entry) = profile.iter_mut().find(|(k, _)| (*k - bucket).abs() < 1e-9) {
                entry.1 += b.vol;
            } else {
                profile.push((bucket, b.vol));
            }
        }
        // Sort by price
        profile.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        // Find POC and expand to 70% volume
        let total_vol: f64 = profile.iter().map(|(_, v)| v).sum();
        let target_vol = total_vol * 0.7;
        let poc_idx = profile.iter().enumerate().max_by(|a, b| a.1.1.partial_cmp(&b.1.1).unwrap_or(std::cmp::Ordering::Equal)).map(|(i, _)| i).unwrap_or(0);
        let mut vol_sum = profile[poc_idx].1;
        let mut high_idx = poc_idx;
        let mut low_idx = poc_idx;
        while vol_sum < target_vol && (high_idx < profile.len() - 1 || low_idx > 0) {
            let up_vol = if high_idx < profile.len() - 1 { profile[high_idx + 1].1 } else { 0.0 };
            let dn_vol = if low_idx > 0 { profile[low_idx - 1].1 } else { 0.0 };
            if up_vol >= dn_vol && high_idx < profile.len() - 1 {
                high_idx += 1;
                vol_sum += profile[high_idx].1;
            } else if low_idx > 0 {
                low_idx -= 1;
                vol_sum += profile[low_idx].1;
            } else {
                break;
            }
        }
        Some(profile[high_idx].0)
    }
}

/// `footprint.market.profile.val.{tf}` — Value Area Low: lower boundary of
/// 70% volume concentration. Emits on bar close.
pub struct MarketProfileVal {
    tf: String,
    window: usize,
    bars: VecDeque<Bar>,
    bucket_width: f64,
}
impl MarketProfileVal {
    pub fn new(tf: &str, window: usize, bucket_width: f64) -> Self {
        Self {
            tf: tf.to_owned(),
            window,
            bars: VecDeque::new(),
            bucket_width,
        }
    }
}
impl BarFeature for MarketProfileVal {
    fn id(&self) -> String {
        format!("footprint.market.profile.val.{}", self.tf)
    }
    fn warm(&self) -> bool {
        self.bars.len() >= self.window
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.bars.push_back(bar.clone());
        while self.bars.len() > self.window {
            self.bars.pop_front();
        }
        if !self.warm() {
            return None;
        }
        // Build volume profile
        let mut profile: Vec<(f64, f64)> = Vec::new();
        let bucket_size = self.bucket_width;
        for b in self.bars.iter() {
            let mid = (b.high + b.low) / 2.0;
            let bucket = (mid / bucket_size).floor() as f64 * bucket_size + bucket_size / 2.0;
            if let Some(entry) = profile.iter_mut().find(|(k, _)| (*k - bucket).abs() < 1e-9) {
                entry.1 += b.vol;
            } else {
                profile.push((bucket, b.vol));
            }
        }
        // Sort by price
        profile.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        // Find POC and expand to 70% volume
        let total_vol: f64 = profile.iter().map(|(_, v)| v).sum();
        let target_vol = total_vol * 0.7;
        let poc_idx = profile.iter().enumerate().max_by(|a, b| a.1.1.partial_cmp(&b.1.1).unwrap_or(std::cmp::Ordering::Equal)).map(|(i, _)| i).unwrap_or(0);
        let mut vol_sum = profile[poc_idx].1;
        let mut high_idx = poc_idx;
        let mut low_idx = poc_idx;
        while vol_sum < target_vol && (high_idx < profile.len() - 1 || low_idx > 0) {
            let up_vol = if high_idx < profile.len() - 1 { profile[high_idx + 1].1 } else { 0.0 };
            let dn_vol = if low_idx > 0 { profile[low_idx - 1].1 } else { 0.0 };
            if up_vol >= dn_vol && high_idx < profile.len() - 1 {
                high_idx += 1;
                vol_sum += profile[high_idx].1;
            } else if low_idx > 0 {
                low_idx -= 1;
                vol_sum += profile[low_idx].1;
            } else {
                break;
            }
        }
        Some(profile[low_idx].0)
    }
}
