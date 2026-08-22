//! Backtest metrics (spec 005 §Metrics). Expectancy is the only score that
//! matters; win rate is vanity. Drawdown from the equity curve.
//!
//! SIM-8: every report includes a 2×-costs stress column. SIM-12: optimistic
//! maker fills and optimistic tape fallbacks are tracked in separate buckets
//! so G1 can refuse edges that only exist under model optimism. SWG-5 adds
//! bar-return Sharpe and the Deflated Sharpe Ratio (Bailey & López de Prado,
//! 2014) so a walk-forward that tried many grid combos reports a
//! multiple-testing-adjusted edge, not a curve-fit one.

use crate::fills::FillOptimism;

/// Number of bar returns needed before Sharpe is meaningful (FEA-3 warmup).
const MIN_BAR_RETURNS: usize = 2;

/// Bars per year for a bar timeframe in ns (365d @ 24h). Used to annualize the
/// bar-return Sharpe: daily = 365, 4h = 2190, 60s = 525600, 1ms = 31.5e12.
/// SWG-5 bar replay (daily/4h) reports a daily/4h Sharpe, not an event-count
/// one — the annualization must match the run's bar timeframe.
pub fn bars_per_year(bar_tf_ns: i64) -> f64 {
    31_557_600_000_000_000.0 / (bar_tf_ns as f64).max(1.0)
}

/// Per-run metrics, always reported after costs.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub trades: u64,
    pub wins: u64,
    pub gross_win: f64,
    pub gross_loss: f64,
    pub max_drawdown: f64,
    /// SIM-12: trades that closed against an optimistic-maker fill.
    pub maker_trades: u64,
    pub maker_gross_win: f64,
    pub maker_gross_loss: f64,
    /// L1/L2 tape-fallback fills (book missing) that contributed realized P&L.
    pub tape_trades: u64,
    pub tape_gross_win: f64,
    pub tape_gross_loss: f64,
    /// SWG-5: per-bar equity returns sampled at bar close (for Sharpe).
    bar_returns: Vec<f64>,
    last_equity: Option<f64>,
    peak_equity: f64,
    started: bool,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a realized trade outcome (one position reduction).
    pub fn record_trade(&mut self, pnl: f64) {
        if pnl == 0.0 {
            return;
        }
        self.trades += 1;
        if pnl > 0.0 {
            self.wins += 1;
            self.gross_win += pnl;
        } else {
            self.gross_loss += -pnl;
        }
    }

    /// Record P&L under a specific optimism tag (still counted in main tally).
    pub fn record_trade_with_optimism(&mut self, pnl: f64, opt: FillOptimism) {
        self.record_trade(pnl);
        if pnl == 0.0 {
            return;
        }
        match opt {
            FillOptimism::None => {}
            FillOptimism::Maker => {
                self.maker_trades += 1;
                if pnl > 0.0 {
                    self.maker_gross_win += pnl;
                } else {
                    self.maker_gross_loss += -pnl;
                }
            }
            FillOptimism::Tape => {
                self.tape_trades += 1;
                if pnl > 0.0 {
                    self.tape_gross_win += pnl;
                } else {
                    self.tape_gross_loss += -pnl;
                }
            }
        }
    }

    /// SIM-12: maker-tagged expectancy.
    pub fn maker_expectancy(&self) -> f64 {
        if self.maker_trades == 0 {
            0.0
        } else {
            (self.maker_gross_win - self.maker_gross_loss) / self.maker_trades as f64
        }
    }

    /// Tape-fallback expectancy (book-absent L1/L2 path).
    pub fn tape_expectancy(&self) -> f64 {
        if self.tape_trades == 0 {
            0.0
        } else {
            (self.tape_gross_win - self.tape_gross_loss) / self.tape_trades as f64
        }
    }

    /// Back-compat alias for tests that still call `record_maker_trade`.
    pub fn record_maker_trade(&mut self, pnl: f64) {
        self.record_trade_with_optimism(pnl, FillOptimism::Maker);
    }

    /// SIM-8: expectancy under a `multiplier`×-costs stress.
    pub fn stress_expectancy(&self, total_fees: f64, multiplier: f64) -> f64 {
        if self.trades == 0 {
            return 0.0;
        }
        let extra_cost = total_fees * (multiplier - 1.0);
        (self.gross_win - self.gross_loss - extra_cost) / self.trades as f64
    }

    /// SWG-5: record an equity sample at a bar close (bar-return Sharpe input).
    /// The first sample seeds the baseline; subsequent samples contribute log
    /// returns relative to the previous sample.
    pub fn record_bar_return(&mut self, equity: f64) {
        if !self.started {
            self.started = true;
            self.last_equity = Some(equity);
            return;
        }
        if let Some(prev) = self.last_equity {
            if prev > 0.0 && equity > 0.0 {
                self.bar_returns.push((equity / prev).ln());
            }
        }
        self.last_equity = Some(equity);
    }

    /// SWG-5: annualized Sharpe from the bar returns sampled at bar close.
    /// `bars_per_year` is the bar frequency (daily 365, 4h 2190, 60s 525600).
    /// None when too few returns to be meaningful (FEA-3 warmup).
    pub fn sharpe(&self, bars_per_year: f64) -> Option<f64> {
        if self.bar_returns.len() < MIN_BAR_RETURNS {
            return None;
        }
        let n = self.bar_returns.len() as f64;
        let mean = self.bar_returns.iter().sum::<f64>() / n;
        let var = self
            .bar_returns
            .iter()
            .map(|r| (r - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0);
        if var <= 0.0 || !var.is_finite() {
            return None;
        }
        Some(mean / var.sqrt() * bars_per_year.sqrt())
    }

    /// SWG-5: Deflated Sharpe Ratio (Bailey & López de Prado, 2014). Adjusts
    /// the observed Sharpe for the number of independent trials the strategy
    /// was selected from: the more combos tried, the higher the bar to call an
    /// edge real. None when the raw Sharpe is unavailable (FEA-3 warmup).
    pub fn deflated_sharpe(&self, bars_per_year: f64, n_trials: u64) -> Option<f64> {
        let sr = self.sharpe(bars_per_year)?;
        if n_trials <= 1 {
            return Some(sr); // a single trial needs no correction
        }
        // Expected maximum Sharpe under the null (pure noise) for n_trials
        // independent strategies (López de Prado, "Advances in Financial
        // Machine Learning" eq. 3.15):
        //   E[max(SR)] ≈ (1 - γ) * Φ^{-1}(1 - 1/n) + γ * Φ^{-1}(1 - 1/(e·n))
        // with γ the Euler–Mascheroni constant and Φ^{-1} the probit. The
        // probit uses Acklam's rational approximation (relative error < 1.15e-9).
        let euler_gamma = 0.577_215_664_901_532_9;
        let inv_n = 1.0 / n_trials as f64;
        let p1 = probit(1.0 - inv_n);
        let p2 = probit(1.0 - inv_n / std::f64::consts::E);
        let expected_max = (1.0 - euler_gamma) * p1 + euler_gamma * p2;
        Some(sr - expected_max)
    }

    pub fn sample_equity(&mut self, equity: f64) {
        if !self.started {
            self.peak_equity = equity;
            self.started = true;
        }
        self.peak_equity = self.peak_equity.max(equity);
        let dd = self.peak_equity - equity;
        self.max_drawdown = self.max_drawdown.max(dd);
    }

    pub fn hit_rate(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 / self.trades as f64
        }
    }

    pub fn expectancy(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            (self.gross_win - self.gross_loss) / self.trades as f64
        }
    }

    pub fn profit_factor(&self) -> f64 {
        if self.gross_loss == 0.0 {
            f64::INFINITY
        } else {
            self.gross_win / self.gross_loss
        }
    }
}

/// Inverse standard-normal CDF (Peter J. Acklam's rational approximation,
/// relative error < 1.15e-9). Pure deterministic function of its input (PD-3).
pub(crate) fn probit(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e+01,
        2.209_460_984_245_205e+02,
        -2.759_285_104_469_687e+02,
        1.383_577_518_672_69e+02,
        -3.066_479_806_614_72e+01,
        2.506_628_277_459_24e+00,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e+01,
        1.615_858_368_322_409e+02,
        -1.556_989_798_598_866e+02,
        6.680_131_188_771_972e+01,
        -1.328_068_155_288_572e+01,
    ];
    const C: [f64; 6] = [
        -7.784_894_003_430_209e-03,
        -3.223_964_776_074_69e-1,
        -2.400_758_277_161_838e+00,
        -2.549_732_539_346_734e+00,
        4.374_664_141_464_968e+00,
        2.938_163_982_698_783e+00,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-03,
        3.224_671_290_700_195e-01,
        2.445_134_137_142_996e+00,
        3.754_408_661_908_416e+00,
    ];
    const P_LOW: f64 = 0.024_25;
    const P_HIGH: f64 = 1.0 - P_LOW;

    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= P_HIGH {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q
            + C[5] / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swg_5_sharpe_and_deflated_sharpe() {
        // A slightly noisy rising curve → positive Sharpe; a single trial's
        // Deflated Sharpe equals the raw one; more trials deflate it.
        let mut m = Metrics::new();
        m.record_bar_return(100.0);
        for i in 1..10 {
            m.record_bar_return(100.0 + (i as f64) * 0.5);
        }
        let s = m.sharpe(365.0).unwrap();
        assert!(s > 0.0, "rising curve must have positive sharpe");

        let m2 = m.clone();
        assert_eq!(m2.deflated_sharpe(365.0, 1), Some(s));

        let d = m.deflated_sharpe(365.0, 100).unwrap();
        assert!(d < s, "more trials must lower the deflated sharpe");

        // Warmup: too few returns → None, never NaN.
        let mut cold = Metrics::new();
        cold.record_bar_return(100.0);
        assert!(cold.sharpe(365.0).is_none());
        assert!(cold.deflated_sharpe(365.0, 10).is_none());
    }

    #[test]
    fn swg_5_probit_is_monotone_and_symmetric() {
        let p1 = probit(0.975);
        let p2 = probit(0.5);
        let p3 = probit(0.025);
        assert!(p1 > p2 && p2 > p3, "probit must be monotone");
        assert!((p1 + p3).abs() < 1e-6, "probit is antisymmetric about 0.5");
    }
}
