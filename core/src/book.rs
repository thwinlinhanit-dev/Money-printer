//! Order-book reconstruction from snapshots + deltas (EVT-9).
//!
//! Validates sequence continuity. On any gap the mirror marks itself **stale**
//! and refuses reads until the next snapshot — a book with a silent hole is
//! worse than no book (spec 001/002). Downstream features must not read a
//! stale book (FEA-8).

use crate::event::{Level, MarketEvent};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Total-ordered price key (handles the `f64` ordering; prices are never NaN in
/// a valid book, but `total_cmp` is defined regardless).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Px(f64);
impl Eq for Px {}
impl Ord for Px {
    fn cmp(&self, o: &Self) -> Ordering {
        self.0.total_cmp(&o.0)
    }
}
impl PartialOrd for Px {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Reconstructed L2 book for one symbol on one venue.
#[derive(Debug, Clone, Default)]
pub struct BookMirror {
    bids: BTreeMap<Px, f64>,
    asks: BTreeMap<Px, f64>,
    last_seq: u64,
    initialized: bool,
    stale: bool,
}

impl BookMirror {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the book is currently unusable (gap seen, awaiting snapshot).
    pub fn is_stale(&self) -> bool {
        self.stale || !self.initialized
    }

    /// Last applied sequence number.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Apply a market event if it is a book snapshot or delta; other event
    /// kinds are ignored. Returns `true` if the book changed state.
    pub fn apply(&mut self, ev: &MarketEvent) -> bool {
        match ev {
            MarketEvent::BookSnapshot {
                bids, asks, seq, ..
            } => {
                self.apply_snapshot(*seq, bids, asks);
                true
            }
            MarketEvent::BookDelta {
                bids,
                asks,
                first_seq,
                last_seq,
            } => self.apply_delta(*first_seq, *last_seq, bids, asks),
            _ => false,
        }
    }

    /// Reset the book from a snapshot (clears staleness).
    pub fn apply_snapshot(&mut self, seq: u64, bids: &[Level], asks: &[Level]) {
        self.bids.clear();
        self.asks.clear();
        for &(p, q) in bids {
            if q > 0.0 {
                self.bids.insert(Px(p), q);
            }
        }
        for &(p, q) in asks {
            if q > 0.0 {
                self.asks.insert(Px(p), q);
            }
        }
        self.last_seq = seq;
        self.initialized = true;
        self.stale = false;
    }

    /// Apply a book delta with `[first_seq, last_seq]` continuity checking
    /// (EVT-9). Returns `false` and marks the book stale on a gap.
    ///
    /// Rules:
    /// - Fully-old update (`last_seq <= self.last_seq`): already applied, ignore.
    /// - Contiguous (`first_seq <= self.last_seq + 1 <= last_seq + 1`): apply.
    /// - Gap (`first_seq > self.last_seq + 1`): mark stale, drop until snapshot.
    pub fn apply_delta(
        &mut self,
        first_seq: u64,
        last_seq: u64,
        bids: &[Level],
        asks: &[Level],
    ) -> bool {
        if !self.initialized || self.stale {
            // No trusted base to apply onto; wait for a snapshot.
            self.stale = true;
            return false;
        }
        if last_seq <= self.last_seq {
            return false; // stale/duplicate, harmless
        }
        if first_seq > self.last_seq + 1 {
            self.stale = true; // gap
            return false;
        }
        for &(p, q) in bids {
            if q > 0.0 {
                self.bids.insert(Px(p), q);
            } else {
                self.bids.remove(&Px(p));
            }
        }
        for &(p, q) in asks {
            if q > 0.0 {
                self.asks.insert(Px(p), q);
            } else {
                self.asks.remove(&Px(p));
            }
        }
        self.last_seq = last_seq;
        true
    }

    /// Best bid `(price, qty)` — `None` if stale or empty (EVT-9 refuses reads).
    pub fn best_bid(&self) -> Option<(f64, f64)> {
        if self.is_stale() {
            return None;
        }
        self.bids.iter().next_back().map(|(p, q)| (p.0, *q))
    }

    /// Best ask `(price, qty)` — `None` if stale or empty.
    pub fn best_ask(&self) -> Option<(f64, f64)> {
        if self.is_stale() {
            return None;
        }
        self.asks.iter().next().map(|(p, q)| (p.0, *q))
    }

    /// Mid price — `None` if stale or a side is empty.
    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some((b, _)), Some((a, _))) => Some((b + a) / 2.0),
            _ => None,
        }
    }

    /// Number of resting levels per side — `None` if stale.
    pub fn depth_levels(&self) -> Option<(usize, usize)> {
        if self.is_stale() {
            return None;
        }
        Some((self.bids.len(), self.asks.len()))
    }

    /// Walk the ask side consuming up to `qty`, best price first, removing
    /// displayed size as it is taken (a market buy paying impact — SIM-2 L2).
    /// The book recovers naturally from subsequent deltas. Returns
    /// `(filled_qty, notional)`; `None` if stale (no trusted book to walk).
    pub fn walk_ask(&mut self, qty: f64) -> Option<(f64, f64)> {
        self.walk(qty, true)
    }

    /// Walk the bid side consuming up to `qty`, best price first (a market
    /// sell). See [`Self::walk_ask`].
    pub fn walk_bid(&mut self, qty: f64) -> Option<(f64, f64)> {
        self.walk(qty, false)
    }

    fn walk(&mut self, qty: f64, asks: bool) -> Option<(f64, f64)> {
        if self.is_stale() {
            return None;
        }
        let mut remaining = qty;
        let mut notional = 0.0;
        let mut drained: Vec<Px> = Vec::new();
        let side = if asks { &mut self.asks } else { &mut self.bids };
        // Asks: ascending (lowest first). Bids: descending (highest first).
        let prices: Vec<Px> = if asks {
            side.keys().copied().collect()
        } else {
            side.keys().rev().copied().collect()
        };
        for px in prices {
            if remaining <= 0.0 {
                break;
            }
            let level_qty = *side.get(&px).unwrap();
            let take = remaining.min(level_qty);
            notional += take * px.0;
            remaining -= take;
            if take >= level_qty {
                drained.push(px);
            } else {
                side.insert(px, level_qty - take);
            }
        }
        for px in drained {
            side.remove(&px);
        }
        Some((qty - remaining, notional))
    }
}
