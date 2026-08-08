//! Fill-model ladder + pending-order book (SIM-2).
//!
//! Isolated from the backtester hub so L0/L1/L2 logic and the single pending
//! drain live in one place — not three copy-pasted `mem::take` loops.

use mp_core::{BookMirror, IntentId, Liquidity, MarketEvent, Side, SymbolId, Venue};
use mp_features::BarBuilder;
use std::collections::BTreeMap;

/// How optimistic a fill is (SIM-12 + tape-fallback honesty).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillOptimism {
    /// No model optimism beyond explicit slip/fees.
    #[default]
    None,
    /// Resting limit assumed filled without queue position (SIM-12).
    Maker,
    /// L1/L2 market fill used tape/mark because the book was missing/stale.
    Tape,
}

impl FillOptimism {
    pub fn as_str(self) -> &'static str {
        match self {
            FillOptimism::None => "none",
            FillOptimism::Maker => "maker",
            FillOptimism::Tape => "tape",
        }
    }
}

/// Fill-model selection (SIM-2 ladder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillModel {
    /// Fill at the next bar's open ± slip (always complete).
    L0BarFill,
    /// Market: top-of-book × participation; limit: trade-print-through.
    #[default]
    L1TopOfBook,
    /// Market: book walk (impact); limit: trade volume × queue_share.
    L2DepthWalk,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PendingKind {
    Market,
    Limit(f64),
}

#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub symbol: SymbolId,
    pub venue: Venue,
    pub side: Side,
    pub qty: f64,
    pub kind: PendingKind,
    pub ready_ns: i64,
    pub intent_id: IntentId,
}

/// A produced fill before accounting.
#[derive(Debug, Clone)]
pub(crate) struct ProducedFill {
    pub symbol: SymbolId,
    pub venue: Venue,
    pub side: Side,
    pub price: f64,
    pub qty: f64,
    pub intent_id: IntentId,
    pub liquidity: Liquidity,
    pub optimism: FillOptimism,
}

/// Parameters the fill models need from SimConfig (no full config coupling).
#[derive(Debug, Clone, Copy)]
pub(crate) struct FillParams {
    pub model: FillModel,
    pub slip_frac: f64,
    pub participation: f64,
    pub queue_share: f64,
    pub bar_tf_ns: i64,
}

/// Pending orders + L0 bar state + L1/L2 book-relative fill logic.
#[derive(Debug, Default)]
pub(crate) struct PendingBook {
    pending: Vec<Pending>,
    /// L0 only: one bar builder per symbol (not shared with FeatureEngine —
    /// L0 needs open-of-next-bar, not feature bar closes).
    bars: BTreeMap<SymbolId, BarBuilder>,
}

impl PendingBook {
    pub fn push(&mut self, p: Pending) {
        self.pending.push(p);
    }

    /// Drain pending matching `keep` (returns true → stay pending).
    /// `on_fill` is called for each order that leaves the book as a fill.
    fn drain(
        &mut self,
        mut keep: impl FnMut(&Pending) -> bool,
        mut try_fill: impl FnMut(&Pending) -> Option<(f64, f64, Liquidity, FillOptimism)>,
    ) -> Vec<ProducedFill> {
        let ready = std::mem::take(&mut self.pending);
        let mut still = Vec::with_capacity(ready.len());
        let mut out = Vec::new();
        for p in ready {
            if keep(&p) {
                still.push(p);
                continue;
            }
            match try_fill(&p) {
                Some((px, qty, liq, opt)) if qty > 0.0 => {
                    out.push(ProducedFill {
                        symbol: p.symbol,
                        venue: p.venue,
                        side: p.side,
                        price: px,
                        qty,
                        intent_id: p.intent_id,
                        liquidity: liq,
                        optimism: opt,
                    });
                    if qty < p.qty {
                        still.push(Pending {
                            qty: p.qty - qty,
                            ..p
                        });
                    }
                }
                _ => still.push(p),
            }
        }
        self.pending = still;
        out
    }

    /// L0: on bar close, fill all ready pending for `symbol` at new open ± slip.
    pub fn on_trade_l0(
        &mut self,
        params: FillParams,
        symbol: SymbolId,
        price: f64,
        qty: f64,
        side: Side,
        now: i64,
    ) -> Vec<ProducedFill> {
        let bar = self
            .bars
            .entry(symbol)
            .or_insert_with(|| BarBuilder::new(params.bar_tf_ns));
        let closed = bar.on_event(
            now,
            &MarketEvent::Trade {
                price,
                qty,
                side,
                trade_id: 0,
            },
        );
        let Some(closed) = closed else {
            return Vec::new();
        };
        let new_open = self
            .bars
            .get(&symbol)
            .map(|b| b.current_open())
            .unwrap_or(price);
        let close_ts = closed.close_ts_ns;
        let slip = params.slip_frac;
        self.drain(
            |p| p.symbol != symbol || p.ready_ns > close_ts,
            |p| {
                let fill_px =
                    new_open * (1.0 + slip * if p.side == Side::Buy { 1.0 } else { -1.0 });
                Some((fill_px, p.qty, Liquidity::Maker, FillOptimism::None))
            },
        )
    }

    /// L1/L2 market fills when ready and book/tape available.
    pub fn try_fill_market(
        &mut self,
        params: FillParams,
        books: &mut BTreeMap<SymbolId, BookMirror>,
        marks: &BTreeMap<SymbolId, f64>,
        symbol: SymbolId,
        now: i64,
    ) -> Vec<ProducedFill> {
        if matches!(params.model, FillModel::L0BarFill) {
            return Vec::new();
        }
        let model = params.model;
        let slip = params.slip_frac;
        let part = params.participation;
        self.drain(
            |p| p.symbol != symbol || p.ready_ns > now || p.kind != PendingKind::Market,
            |p| {
                match model {
                    FillModel::L1TopOfBook => {
                        fill_l1_market(books, marks, symbol, p.side, p.qty, slip, part)
                    }
                    FillModel::L2DepthWalk => {
                        fill_l2_market(books, marks, symbol, p.side, p.qty, slip)
                    }
                    FillModel::L0BarFill => None,
                }
                .map(|(px, q, tape)| {
                    (
                        px,
                        q,
                        Liquidity::Taker,
                        if tape {
                            FillOptimism::Tape
                        } else {
                            FillOptimism::None
                        },
                    )
                })
            },
        )
    }

    /// Trade-print rule for resting limits (SIM-2).
    pub fn try_fill_limit_trade_print(
        &mut self,
        params: FillParams,
        symbol: SymbolId,
        trade_px: f64,
        trade_qty: f64,
        aggr: Side,
        now: i64,
    ) -> Vec<ProducedFill> {
        let model = params.model;
        let qshare = params.queue_share;
        self.drain(
            |p| {
                let PendingKind::Limit(limit_px) = p.kind else {
                    return true; // keep non-limits
                };
                let crosses = match (p.side, aggr) {
                    (Side::Buy, Side::Sell) => trade_px < limit_px,
                    (Side::Sell, Side::Buy) => trade_px > limit_px,
                    _ => false,
                };
                p.symbol != symbol || p.ready_ns > now || !crosses
            },
            |p| {
                let PendingKind::Limit(limit_px) = p.kind else {
                    return None;
                };
                let fill_qty = match model {
                    FillModel::L2DepthWalk => p.qty.min(trade_qty * qshare),
                    _ => p.qty,
                };
                if fill_qty <= 0.0 {
                    return None;
                }
                Some((limit_px, fill_qty, Liquidity::Maker, FillOptimism::Maker))
            },
        )
    }
}

fn fallback_trade_price(
    marks: &BTreeMap<SymbolId, f64>,
    symbol: SymbolId,
    side: Side,
    qty: f64,
    slip: f64,
) -> Option<(f64, f64, bool)> {
    let mark = marks.get(&symbol).copied()?;
    let px = match side {
        Side::Buy => mark * (1.0 + slip),
        Side::Sell => mark * (1.0 - slip),
    };
    Some((px, qty, true))
}

fn fill_l1_market(
    books: &BTreeMap<SymbolId, BookMirror>,
    marks: &BTreeMap<SymbolId, f64>,
    symbol: SymbolId,
    side: Side,
    qty: f64,
    slip: f64,
    participation: f64,
) -> Option<(f64, f64, bool)> {
    let book = books.get(&symbol);
    let touch = match side {
        Side::Buy => book.and_then(|b| b.best_ask()),
        Side::Sell => book.and_then(|b| b.best_bid()),
    };
    if let Some((px, top_qty)) = touch {
        let cap = top_qty * participation;
        let fill_qty = qty.min(cap.max(0.0));
        if fill_qty <= 0.0 {
            return None;
        }
        let slipped = match side {
            Side::Buy => px * (1.0 + slip),
            Side::Sell => px * (1.0 - slip),
        };
        return Some((slipped, fill_qty, false));
    }
    fallback_trade_price(marks, symbol, side, qty, slip)
}

fn fill_l2_market(
    books: &mut BTreeMap<SymbolId, BookMirror>,
    marks: &BTreeMap<SymbolId, f64>,
    symbol: SymbolId,
    side: Side,
    qty: f64,
    slip: f64,
) -> Option<(f64, f64, bool)> {
    let walked = books.get_mut(&symbol).and_then(|b| match side {
        Side::Buy => b.walk_ask(qty),
        Side::Sell => b.walk_bid(qty),
    });
    if let Some((filled_qty, notional)) = walked {
        if filled_qty <= 0.0 {
            return fallback_trade_price(marks, symbol, side, qty, slip);
        }
        return Some((notional / filled_qty, filled_qty, false));
    }
    fallback_trade_price(marks, symbol, side, qty, slip)
}
