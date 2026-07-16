//! Fractional-Kelly ceiling (RSK-3). Estimated from LIVE trades only (backtests
//! flatter); quarter-Kelly by default because full Kelly assumes you know your
//! edge and you don't. Below a minimum live-trade count the cap is pinned to a
//! small floor — LiveSmall exists to gather that sample.

/// Live trade-outcome summary feeding the Kelly estimate.
#[derive(Debug, Clone, Copy)]
pub struct KellyStats {
    /// Number of LIVE trades (never backtest — see spec).
    pub trades: u32,
    /// Win rate `p ∈ [0, 1]`.
    pub p: f64,
    /// Odds `b = avg_win / avg_loss` (> 0).
    pub b: f64,
}

/// Kelly ceiling parameters.
#[derive(Debug, Clone, Copy)]
pub struct KellyParams {
    /// Fraction of full Kelly to allow (default 0.25).
    pub kelly_fraction: f64,
    /// Minimum live trades before trusting the estimate.
    pub min_trades: u32,
    /// Allocation floor used while under `min_trades`.
    pub alloc_floor: f64,
}

impl Default for KellyParams {
    fn default() -> Self {
        Self {
            kelly_fraction: 0.25,
            min_trades: 30,
            alloc_floor: 0.02,
        }
    }
}

/// Full-Kelly optimal fraction `f* = p − (1−p)/b`, clamped at 0 (never short
/// the edge).
pub fn full_kelly(stats: &KellyStats) -> f64 {
    if !(stats.p.is_finite() && stats.b.is_finite()) || stats.b <= 0.0 {
        return 0.0;
    }
    (stats.p - (1.0 - stats.p) / stats.b).max(0.0)
}

/// Allocation ceiling from the Kelly estimate (RSK-3). Pinned to `alloc_floor`
/// while there are fewer than `min_trades` live trades.
pub fn kelly_cap(params: &KellyParams, stats: &KellyStats) -> f64 {
    if stats.trades < params.min_trades {
        return params.alloc_floor.max(0.0);
    }
    (params.kelly_fraction.max(0.0) * full_kelly(stats)).max(0.0)
}

/// RSK-5: default drawdown budget for a strategy entering LiveSmall, sized
/// from the Monte-Carlo distribution (SIM-9): `p95(maxDD_mc) × 1.25`. The
/// margin covers the bootstrap's own uncertainty; the owner may only set a
/// TIGHTER budget than this default without a new decision (safety asymmetry).
pub fn dd_budget_from_mc(p95_max_dd: f64) -> f64 {
    p95_max_dd.max(0.0) * 1.25
}
