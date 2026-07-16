//! Backtest metrics (spec 005 §Metrics). Expectancy is the only score that
//! matters; win rate is vanity. Drawdown from the equity curve.
//!
//! SIM-8: every report includes a 2×-costs stress column. SIM-12: optimistic
//! maker fills and optimistic tape fallbacks are tracked in separate buckets
//! so G1 can refuse edges that only exist under model optimism.

use crate::fills::FillOptimism;

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
