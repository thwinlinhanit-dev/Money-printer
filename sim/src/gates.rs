//! Gate G1 evaluation (spec 006 §Gates). G1 is the first honest filter —
//! Backtest → WalkForward:
//!
//! - expectancy > 0 in the **2×-cost column** (SIM-8);
//! - at least `min_trades` trades;
//! - max drawdown ≤ the declared budget;
//! - the edge must NOT be optimistic-maker-dependent (SIM-12);
//! - the edge must NOT be optimistic-tape-dependent (L1/L2 book fallback).

use crate::metrics::Metrics;

/// G1 thresholds (spec 006 gate table).
#[derive(Debug, Clone, Copy)]
pub struct G1Params {
    pub min_trades: u64,
    pub dd_budget: f64,
}

impl Default for G1Params {
    fn default() -> Self {
        G1Params {
            min_trades: 100,
            dd_budget: f64::INFINITY,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct G1Result {
    pub pass: bool,
    pub reasons: Vec<String>,
}

/// Evaluate G1 against a completed run's metrics and its 2×-cost stress
/// expectancy (from [`crate::Backtester::stress_expectancy_2x`]).
pub fn evaluate_g1(metrics: &Metrics, stress_expectancy_2x: f64, p: &G1Params) -> G1Result {
    let mut reasons = Vec::new();

    if stress_expectancy_2x <= 0.0 {
        reasons.push(format!(
            "expectancy in 2x-cost column is {stress_expectancy_2x:+.6} (must be > 0)"
        ));
    }
    if metrics.trades < p.min_trades {
        reasons.push(format!(
            "only {} trades (need >= {})",
            metrics.trades, p.min_trades
        ));
    }
    if metrics.max_drawdown > p.dd_budget {
        reasons.push(format!(
            "max drawdown {:.2} exceeds budget {:.2}",
            metrics.max_drawdown, p.dd_budget
        ));
    }

    let net = metrics.gross_win - metrics.gross_loss;
    let maker_net = metrics.maker_gross_win - metrics.maker_gross_loss;
    let tape_net = metrics.tape_gross_win - metrics.tape_gross_loss;

    // Maker-dependence: strip optimistic-maker P&L; if the remainder is
    // non-positive while maker profit was positive, the edge is queue optimism.
    let non_maker_net = net - maker_net;
    if metrics.maker_trades > 0 && maker_net > 0.0 && non_maker_net <= 0.0 {
        reasons.push(
            "edge is optimistic-maker-dependent: non-maker P&L is non-positive (SIM-12)".into(),
        );
    }

    // Tape-dependence: same honesty for L1/L2 book-missing full fills.
    let non_tape_net = net - tape_net;
    if metrics.tape_trades > 0 && tape_net > 0.0 && non_tape_net <= 0.0 {
        reasons.push(
            "edge is optimistic-tape-dependent: non-tape P&L is non-positive (book fallback)"
                .into(),
        );
    }

    G1Result {
        pass: reasons.is_empty(),
        reasons,
    }
}
