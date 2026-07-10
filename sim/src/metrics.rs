//! Backtest metrics (spec 005 §Metrics). Expectancy is the only score that
//! matters; win rate is vanity. Drawdown from the equity curve.

/// Per-run metrics, always reported after costs.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub trades: u64,
    pub wins: u64,
    pub gross_win: f64,
    pub gross_loss: f64,
    pub max_drawdown: f64,
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

    /// Sample the equity curve to track drawdown.
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

    /// `E = mean P&L per trade` after costs.
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
