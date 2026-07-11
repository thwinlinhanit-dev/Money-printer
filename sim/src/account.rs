//! Portfolio accounting (SIM-13). Average-cost positions with an exact identity
//! asserted at every step: `equity == start_cash + realized + unrealized − fees`.
//!
//! `realized` stays GROSS of fees (the identity above needs the separate fee
//! term); per-trade NET P&L for metrics comes from [`FillOutcome`], which
//! attributes each closing fill's own fee plus the pro-rata share of the entry
//! fees it releases — so expectancy is genuinely after costs (spec 005).

use mp_core::SymbolId;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default)]
struct Position {
    qty: f64, // signed: long +, short −
    avg: f64,
    /// Fees paid opening the currently-held quantity (released pro-rata as the
    /// position reduces, so closed-trade P&L can be reported net of ALL costs).
    fees_open: f64,
}

/// What one fill did, for metrics attribution.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FillOutcome {
    /// Realized P&L from the position reduction, GROSS of fees.
    pub realized_gross: f64,
    /// Quantity closed by this fill (0.0 for a pure open/add).
    pub closed_qty: f64,
    /// Costs attributable to the closed quantity: the closing portion of this
    /// fill's fee + the pro-rata entry fees released from the position.
    pub attributed_fees: f64,
}

/// Tracks cash, positions, realized/unrealized P&L, and fees.
#[derive(Debug, Clone)]
pub struct Accountant {
    start_cash: f64,
    cash: f64,
    realized: f64,
    fees: f64,
    funding_paid: f64,
    positions: BTreeMap<SymbolId, Position>,
    marks: BTreeMap<SymbolId, f64>,
}

impl Accountant {
    pub fn new(start_cash: f64) -> Self {
        Self {
            start_cash,
            cash: start_cash,
            realized: 0.0,
            fees: 0.0,
            funding_paid: 0.0,
            positions: BTreeMap::new(),
            marks: BTreeMap::new(),
        }
    }

    /// Accrue funding on a held perp position (SIM-4). Longs pay when `rate > 0`.
    /// Uses the current mark for notional.
    pub fn accrue_funding(&mut self, symbol: SymbolId, rate: f64) {
        let qty = self.position(symbol);
        if qty == 0.0 || !rate.is_finite() {
            return;
        }
        let mark = self.marks.get(&symbol).copied().unwrap_or(0.0);
        let pay = rate * qty * mark; // long (qty>0) pays when rate>0
        self.cash -= pay;
        self.funding_paid += pay;
    }

    pub fn funding_paid(&self) -> f64 {
        self.funding_paid
    }

    /// Update the mark price used for unrealized/equity.
    pub fn mark(&mut self, symbol: SymbolId, price: f64) {
        if price.is_finite() && price > 0.0 {
            self.marks.insert(symbol, price);
        }
    }

    /// Apply a fill. `signed_qty` is +qty for a buy, −qty for a sell. Returns
    /// the [`FillOutcome`] so callers can report NET per-trade P&L
    /// (gross − attributed entry+exit fees) — expectancy after costs.
    pub fn apply_fill(
        &mut self,
        symbol: SymbolId,
        signed_qty: f64,
        price: f64,
        fee: f64,
    ) -> FillOutcome {
        self.cash -= price * signed_qty;
        self.cash -= fee;
        self.fees += fee;
        self.mark(symbol, price);

        let p = self.positions.entry(symbol).or_default();
        if p.qty == 0.0 {
            p.qty = signed_qty;
            p.avg = price;
            p.fees_open = fee;
            return FillOutcome::default();
        }
        if p.qty.signum() == signed_qty.signum() {
            // Increase same side: weighted average; entry fee accrues.
            let nq = p.qty + signed_qty;
            p.avg = (p.avg * p.qty + price * signed_qty) / nq;
            p.qty = nq;
            p.fees_open += fee;
            return FillOutcome::default();
        }
        // Reduce/close/flip.
        let reduce = signed_qty.abs().min(p.qty.abs());
        let realized_gross = reduce * (price - p.avg) * p.qty.signum();
        self.realized += realized_gross;
        // Pro-rata entry fees released by this reduction + the closing share
        // of this fill's own fee (a flip's opening share stays with the new
        // position).
        let released_entry = p.fees_open * (reduce / p.qty.abs());
        let closing_fee = fee * (reduce / signed_qty.abs());
        let nq = p.qty + signed_qty;
        if nq == 0.0 {
            *p = Position::default();
        } else if nq.signum() == p.qty.signum() {
            p.qty = nq; // partial reduce, avg unchanged
            p.fees_open -= released_entry;
        } else {
            p.qty = nq; // flipped: remaining fee opens the new side
            p.avg = price;
            p.fees_open = fee - closing_fee;
        }
        FillOutcome {
            realized_gross,
            closed_qty: reduce,
            attributed_fees: released_entry + closing_fee,
        }
    }

    pub fn position(&self, symbol: SymbolId) -> f64 {
        self.positions.get(&symbol).map(|p| p.qty).unwrap_or(0.0)
    }

    /// Average entry price of the current position (0.0 if flat).
    pub fn avg_cost(&self, symbol: SymbolId) -> f64 {
        self.positions.get(&symbol).map(|p| p.avg).unwrap_or(0.0)
    }

    pub fn positions(&self) -> BTreeMap<SymbolId, f64> {
        self.positions.iter().map(|(k, v)| (*k, v.qty)).collect()
    }

    pub fn realized(&self) -> f64 {
        self.realized
    }

    pub fn fees(&self) -> f64 {
        self.fees
    }

    /// Σ over positions of `qty·(mark − avg)`.
    pub fn unrealized(&self) -> f64 {
        self.positions
            .iter()
            .map(|(sym, p)| {
                let mark = self.marks.get(sym).copied().unwrap_or(p.avg);
                p.qty * (mark - p.avg)
            })
            .sum()
    }

    /// Equity = cash + Σ qty·mark.
    pub fn equity(&self) -> f64 {
        let pos_val: f64 = self
            .positions
            .iter()
            .map(|(sym, p)| {
                let mark = self.marks.get(sym).copied().unwrap_or(p.avg);
                p.qty * mark
            })
            .sum();
        self.cash + pos_val
    }

    /// SIM-13 identity residual: should be ~0 at all times.
    /// `equity == start_cash + realized + unrealized − fees − funding_paid`.
    pub fn identity_residual(&self) -> f64 {
        self.equity()
            - (self.start_cash + self.realized + self.unrealized() - self.fees - self.funding_paid)
    }
}
