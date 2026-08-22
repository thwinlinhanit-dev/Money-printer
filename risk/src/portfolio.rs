//! Portfolio-level risk math (spec 035 SWG-6, amending spec 008). Pure
//! functions over plain inputs — no venue, no wall clock (PD-3/PD-4).
//!
//! Three capabilities the per-trade sizer does NOT have (a swing portfolio
//! holds several positions at once):
//! - `correlation_adjusted_exposure` — one number for the whole book that
//!   punishes stacking correlated legs, feeding the RG-13 portfolio cap.
//! - `cumulative_funding_cost` / `expected_return_net_of_funding` — expected
//!   return must be net of funding over the strategy's declared holding period
//!   (`holding_period_bars`), not just spot P&L.
//!
//! Fail-closed (CONV-8): corrupt/ragged inputs never produce a too-small
//! number — a matrix mismatch or an out-of-range correlation yields `+inf`
//! (a cap that always rejects), and non-finite funding inputs yield `NaN`
//! (a return that cannot be compared optimistically).

use mp_core::Side;

/// Correlation-adjusted portfolio exposure: `sqrt(wᵀ ρ w)` where `w` = position
/// notionals and `ρ` = pairwise correlation matrix.
///
/// Degenerate cases that must hold:
/// - empty book → 0.0 (nothing to cap);
/// - perfectly correlated (`ρ = 1`) book → the gross sum (RG-5's number);
/// - uncorrelated book (identity `ρ`) → sqrt of sum of squares (less than
///   gross — diversification is allowed to earn a bigger book, that is the
///   point of the cap);
/// - ragged matrix, non-finite `ρ`, or `ρ ∉ [-1, 1]` → `+inf` (fail-closed:
///   an uncomputable correlation never under-reports exposure).
pub fn correlation_adjusted_exposure(notionals: &[f64], corr: &[Vec<f64>]) -> f64 {
    if notionals.is_empty() {
        return 0.0;
    }
    let n = notionals.len();
    if corr.len() != n || corr.iter().any(|row| row.len() != n) {
        return f64::INFINITY;
    }
    let mut quad = 0.0;
    for (i, w_i) in notionals.iter().enumerate() {
        for (j, w_j) in notionals.iter().enumerate() {
            let rho = corr[i][j];
            if !rho.is_finite() || !(-1.0..=1.0).contains(&rho) {
                return f64::INFINITY;
            }
            quad += rho * w_i * w_j;
        }
    }
    if !quad.is_finite() || quad < 0.0 {
        // A real correlation matrix is positive semi-definite (quad >= 0);
        // anything else is data corruption — fail closed.
        return f64::INFINITY;
    }
    quad.sqrt()
}

/// Cumulative funding cost over a holding period (spec 035 SWG-6): the
/// strategy's `holding_period_bars` × bar-interval funding rate × notional.
///
/// Sign convention: a LONG pays the prevailing rate (`rate > 0` ⇒ cost > 0);
/// a SHORT receives it. Negative rates (inverted funding) flip the sign —
/// the position earns funding. Returns `NaN` on non-finite inputs so a
/// corrupted rate can never silently zero the drag.
pub fn cumulative_funding_cost(
    notional: f64,
    funding_rate_per_interval: f64,
    intervals_held: u32,
    side: Side,
) -> f64 {
    if !notional.is_finite() || !funding_rate_per_interval.is_finite() {
        return f64::NAN;
    }
    let side_sign = match side {
        Side::Buy => 1.0,   // long pays the positive rate
        Side::Sell => -1.0, // short receives the positive rate
    };
    side_sign * funding_rate_per_interval * intervals_held as f64 * notional.abs()
}

/// Expected return net of cumulative funding over the expected holding period
/// (spec 035 SWG-6). `gross_pnl` is the spot P&L the strategy expects; the
/// funding drag is subtracted so a positive-spot, funding-negative position is
/// correctly unattractive. `NaN` on invalid inputs — never an optimistic number.
pub fn expected_return_net_of_funding(
    gross_pnl: f64,
    notional: f64,
    funding_rate_per_interval: f64,
    intervals_held: u32,
    side: Side,
) -> f64 {
    if !gross_pnl.is_finite() {
        return f64::NAN;
    }
    let cost = cumulative_funding_cost(notional, funding_rate_per_interval, intervals_held, side);
    if cost.is_nan() {
        return f64::NAN;
    }
    gross_pnl - cost
}
