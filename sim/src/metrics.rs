//! Backtest metrics (spec 005 §Metrics). Expectancy is the only score that
//! matters; win rate is vanity. Drawdown from the equity curve.
//!
//! SIM-8: every report includes a 2×-costs stress column — the same trades,
//! fees doubled, so a strategy that only works at today's fee schedule is
//! visible before it is trusted. SIM-12: fills our L1/L2 fill models assumed
//! were resting-maker (not queue-modeled, so optimistic) are tracked
//! separately — their P&L is an upper bound, not a promise.

/// Per-run metrics, always reported after costs.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub trades: u64,
    pub wins: u64,
    pub gross_win: f64,
    pub gross_loss: f64,
    pub max_drawdown: f64,
    /// SIM-12: trade count/P&L that depended on an optimistic-maker fill.
    pub maker_trades: u64,
    pub maker_gross_win: f64,
    pub maker_gross_loss: f64,
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

    /// Record a realized trade outcome that closed against an
    /// `optimistic_maker` fill (SIM-12) — counted in both the main tally
    /// (it's still a real trade) and the maker-dependent sub-tally.
    pub fn record_maker_trade(&mut self, pnl: f64) {
        self.record_trade(pnl);
        if pnl == 0.0 {
            return;
        }
        self.maker_trades += 1;
        if pnl > 0.0 {
            self.maker_gross_win += pnl;
        } else {
            self.maker_gross_loss += -pnl;
        }
    }

    /// SIM-12: expectancy restricted to maker-dependent trades — the
    /// upper-bound number that disappears if queue position doesn't materialize.
    pub fn maker_expectancy(&self) -> f64 {
        if self.maker_trades == 0 {
            0.0
        } else {
            (self.maker_gross_win - self.maker_gross_loss) / self.maker_trades as f64
        }
    }

    /// SIM-8: expectancy under a `multiplier`×-costs stress (default caller
    /// uses 2.0). `total_fees` is the run's actual total fee spend
    /// (`Accountant::fees`); this re-prices the *extra* fee burden across the
    /// trade count without re-simulating fills under the higher cost (a full
    /// re-simulation is a documented follow-up, spec 005 Decisions).
    pub fn stress_expectancy(&self, total_fees: f64, multiplier: f64) -> f64 {
        if self.trades == 0 {
            return 0.0;
        }
        let extra_cost = total_fees * (multiplier - 1.0);
        (self.gross_win - self.gross_loss - extra_cost) / self.trades as f64
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
