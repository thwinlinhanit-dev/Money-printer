//! Time-bar builder (spec 004 §Bars). Bars close on the first trade that lands
//! in a later time bucket; bar-derived features update ONLY on bar close (no
//! intra-bar repaint — repainting features are banned). Bucketing uses
//! `recv_ts_ns` for consistency with the event stream's ordering.

use mp_core::{MarketEvent, Side};

/// A closed time bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub vol: f64,
    pub buy_vol: f64,
    pub sell_vol: f64,
    pub vwap: f64,
    pub n_trades: u64,
    pub first_ts_ns: i64,
    pub last_ts_ns: i64,
    /// Bucket end (exclusive), i.e. `bucket_start + tf_ns`.
    pub close_ts_ns: i64,
}

/// Accumulates trades into fixed time buckets.
#[derive(Debug, Clone)]
pub struct BarBuilder {
    tf_ns: i64,
    bucket_start: Option<i64>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    vol: f64,
    buy_vol: f64,
    sell_vol: f64,
    pv: f64, // Σ price·qty for vwap
    n: u64,
    first_ts: i64,
    last_ts: i64,
}

impl BarBuilder {
    /// Open price of the bucket currently accumulating (the most recent bar's
    /// open once a new bucket has started). `0.0` before the first trade.
    pub fn current_open(&self) -> f64 {
        self.open
    }

    pub fn new(tf_ns: i64) -> Self {
        Self {
            tf_ns: tf_ns.max(1),
            bucket_start: None,
            open: 0.0,
            high: f64::MIN,
            low: f64::MAX,
            close: 0.0,
            vol: 0.0,
            buy_vol: 0.0,
            sell_vol: 0.0,
            pv: 0.0,
            n: 0,
            first_ts: 0,
            last_ts: 0,
        }
    }

    fn bucket_of(&self, ts: i64) -> i64 {
        ts.div_euclid(self.tf_ns) * self.tf_ns
    }

    fn finish(&self) -> Bar {
        let vwap = if self.vol > 0.0 {
            self.pv / self.vol
        } else {
            self.close
        };
        Bar {
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            vol: self.vol,
            buy_vol: self.buy_vol,
            sell_vol: self.sell_vol,
            vwap,
            n_trades: self.n,
            first_ts_ns: self.first_ts,
            last_ts_ns: self.last_ts,
            close_ts_ns: self.bucket_start.unwrap_or(0) + self.tf_ns,
        }
    }

    fn reset_with(&mut self, bucket: i64, price: f64, qty: f64, side: Side, ts: i64) {
        self.bucket_start = Some(bucket);
        self.open = price;
        self.high = price;
        self.low = price;
        self.close = price;
        self.vol = qty;
        self.buy_vol = 0.0;
        self.sell_vol = 0.0;
        match side {
            Side::Buy => self.buy_vol = qty,
            Side::Sell => self.sell_vol = qty,
        }
        self.pv = price * qty;
        self.n = 1;
        self.first_ts = ts;
        self.last_ts = ts;
    }

    /// Feed an event; returns a closed [`Bar`] when this event opens a new
    /// bucket (only trades participate).
    pub fn on_event(&mut self, ts_ns: i64, body: &MarketEvent) -> Option<Bar> {
        let (price, qty, side) = match body {
            MarketEvent::Trade {
                price, qty, side, ..
            } => (*price, *qty, *side),
            _ => return None,
        };
        let bucket = self.bucket_of(ts_ns);
        match self.bucket_start {
            None => {
                self.reset_with(bucket, price, qty, side, ts_ns);
                None
            }
            Some(cur) if bucket == cur => {
                self.high = self.high.max(price);
                self.low = self.low.min(price);
                self.close = price;
                self.vol += qty;
                match side {
                    Side::Buy => self.buy_vol += qty,
                    Side::Sell => self.sell_vol += qty,
                }
                self.pv += price * qty;
                self.n += 1;
                self.last_ts = ts_ns;
                None
            }
            Some(_) => {
                let closed = self.finish();
                self.reset_with(bucket, price, qty, side, ts_ns);
                Some(closed)
            }
        }
    }
}
