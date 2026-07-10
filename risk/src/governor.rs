//! Drawdown governor (RSK-2). A strategy's allocation decays to zero as its
//! drawdown approaches its budget, so it bleeds out gracefully instead of
//! exploding. At `g = 0` the funnel auto-demotes it (G5).

/// Governor multiplier `g(dd) = clamp(1 − dd/budget, 0, 1)^gamma`.
/// `dd` and `budget` are positive drawdown magnitudes (fractions of equity).
pub fn dd_governor(dd: f64, dd_budget: f64, gamma: f64) -> f64 {
    if !(dd.is_finite() && dd_budget.is_finite()) || dd_budget <= 0.0 {
        return 0.0; // fail closed
    }
    let lin = (1.0 - dd / dd_budget).clamp(0.0, 1.0);
    let g = if gamma == 1.0 {
        lin
    } else {
        lin.powf(gamma.max(0.0))
    };
    g.clamp(0.0, 1.0)
}
