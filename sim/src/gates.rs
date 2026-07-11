//! Gate G1 evaluation (spec 006 §Gates, bridged here because it reads the
//! backtest [`Metrics`] which live in this crate). G1 is the first honest
//! filter — Backtest → WalkForward:
//!
//! - expectancy > 0 in the **2×-cost column** (SIM-8), not the rosy base case;
//! - at least `min_trades` trades (default 100) — no edge claims off 5 fills;
//! - max drawdown ≤ the declared budget;
//! - the edge must NOT be optimistic-maker-dependent (SIM-12): if removing the
//!   maker-tagged P&L flips the 2×-cost expectancy non-positive, the "edge" is
//!   an artifact of queue-position optimism and G1 fails.
//!
//! Agents prepare this evidence; only a human clicks promote (STR-3).

use crate::metrics::Metrics;

/// G1 thresholds (spec 006 gate table).
#[derive(Debug, Clone, Copy)]
pub struct G1Params {
    pub min_trades: u64,
    /// Max drawdown budget in equity currency (magnitude).
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

/// The outcome of a G1 check: pass/fail plus every failing reason (a strategy
/// that fails three ways should see all three — honest evidence, PD-5).
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
    // Maker-dependence: strip the optimistic-maker P&L from the net and see if
    // the base expectancy survives. If the whole edge lived in maker fills, the
    // remaining per-trade net is non-positive ⇒ fail (SIM-12 upper-bound honesty).
    let net = metrics.gross_win - metrics.gross_loss;
    let maker_net = metrics.maker_gross_win - metrics.maker_gross_loss;
    let non_maker_net = net - maker_net;
    if metrics.maker_trades > 0 && maker_net > 0.0 && non_maker_net <= 0.0 {
        reasons.push(
            "edge is optimistic-maker-dependent: non-maker P&L is non-positive (SIM-12)".into(),
        );
    }

    G1Result {
        pass: reasons.is_empty(),
        reasons,
    }
}
