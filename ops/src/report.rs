//! Monthly report generator (OPS-6): the fund-of-one scoreboard, rendered from
//! journals/tracker numbers only — no hand-entered values, no invented cells.
//! An LLM may draft prose *around* these tables (spec 010), but every number
//! here comes from the input struct. Rendering is pure; the caller supplies the
//! already-computed figures (grounding contract).

/// One strategy's row in the expectancy / equity tables.
#[derive(Debug, Clone)]
pub struct StrategyRow {
    pub strategy: String,
    /// Net return over the month, fraction (after all costs).
    pub net_return: f64,
    pub max_drawdown: f64,
    /// Expectancy per trade in R multiples, after costs.
    pub expectancy_r: f64,
    pub trades: u32,
    pub win_rate: f64,
}

/// Live-vs-paper-vs-backtest tracking error for one strategy (SIM-9 spirit).
#[derive(Debug, Clone)]
pub struct TrackingRow {
    pub strategy: String,
    pub live_return: f64,
    pub paper_return: f64,
    pub backtest_return: f64,
    /// Live − backtest, the number that says whether the edge survived contact.
    pub tracking_error: f64,
}

/// Cost breakdown for the month (all in quote currency).
#[derive(Debug, Clone, Default)]
pub struct CostBreakdown {
    pub fees: f64,
    /// Realized slippage minus modeled slippage — model honesty (PD-5).
    pub slippage_vs_model: f64,
    pub funding: f64,
    pub infra: f64,
}

/// A funnel transition or kill during the month (STR-/EXE- events).
#[derive(Debug, Clone)]
pub struct FunnelEvent {
    pub strategy: String,
    pub from_stage: String,
    pub to_stage: String,
    /// True if this was a demotion / kill rather than a promotion.
    pub demotion: bool,
}

/// The benchmark row (REQUIRED, OPS-6): the book vs passive alternatives.
#[derive(Debug, Clone)]
pub struct Benchmark {
    pub book_return: f64,
    pub btc_hold_return: f64,
    pub tbill_return: f64,
}

/// All inputs to the monthly report. Everything here is sourced from journals
/// and the run tracker; the renderer never computes or invents figures.
#[derive(Debug, Clone)]
pub struct MonthlyReport {
    /// `YYYY-MM`.
    pub month: String,
    pub blended_return: f64,
    pub blended_max_drawdown: f64,
    pub strategies: Vec<StrategyRow>,
    pub tracking: Vec<TrackingRow>,
    pub costs: CostBreakdown,
    pub funnel: Vec<FunnelEvent>,
    pub benchmark: Benchmark,
}

fn pct(x: f64) -> String {
    format!("{:+.2}%", x * 100.0)
}

impl MonthlyReport {
    /// Render the §13 scoreboard to markdown. All six sections plus the
    /// benchmark row are always present (OPS-6); empty inputs render an
    /// explicit "no data" line, never a blank or a fabricated value (RES-5
    /// spirit).
    pub fn render_markdown(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("# Monthly Report — {}\n\n", self.month));
        s.push_str(&format!(
            "Blended net return **{}**, max drawdown **{}**.\n\n",
            pct(self.blended_return),
            pct(self.blended_max_drawdown)
        ));

        // 1. Equity & Drawdown (per strategy).
        s.push_str("## Equity & Drawdown\n\n");
        s.push_str("| Strategy | Net return | Max DD |\n|---|---|---|\n");
        if self.strategies.is_empty() {
            s.push_str("| _no data_ | — | — |\n");
        }
        for r in &self.strategies {
            s.push_str(&format!(
                "| {} | {} | {} |\n",
                r.strategy,
                pct(r.net_return),
                pct(r.max_drawdown)
            ));
        }

        // 2. Expectancy after costs.
        s.push_str("\n## Expectancy (after costs)\n\n");
        s.push_str("| Strategy | Expectancy (R) | Trades | Win rate |\n|---|---|---|---|\n");
        if self.strategies.is_empty() {
            s.push_str("| _no data_ | — | — | — |\n");
        }
        for r in &self.strategies {
            s.push_str(&format!(
                "| {} | {:+.3} | {} | {} |\n",
                r.strategy,
                r.expectancy_r,
                r.trades,
                pct(r.win_rate)
            ));
        }

        // 3. Tracking error (live vs paper vs backtest).
        s.push_str("\n## Tracking Error (live vs paper vs backtest)\n\n");
        s.push_str(
            "| Strategy | Live | Paper | Backtest | Tracking err |\n|---|---|---|---|---|\n",
        );
        if self.tracking.is_empty() {
            s.push_str("| _no data_ | — | — | — | — |\n");
        }
        for t in &self.tracking {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                t.strategy,
                pct(t.live_return),
                pct(t.paper_return),
                pct(t.backtest_return),
                pct(t.tracking_error)
            ));
        }

        // 4. Cost breakdown.
        s.push_str("\n## Cost Breakdown\n\n");
        s.push_str(&format!(
            "| Fees | Slippage vs model | Funding | Infra |\n|---|---|---|---|\n| {:.2} | {:.2} | {:.2} | {:.2} |\n",
            self.costs.fees, self.costs.slippage_vs_model, self.costs.funding, self.costs.infra
        ));

        // 5. Funnel transitions & kills.
        s.push_str("\n## Funnel Transitions & Kills\n\n");
        if self.funnel.is_empty() {
            s.push_str("_No transitions this month._\n");
        }
        for f in &self.funnel {
            let arrow = if f.demotion {
                "⏬ kill/demote"
            } else {
                "⏫ promote"
            };
            s.push_str(&format!(
                "- {} {}: {} → {}\n",
                arrow, f.strategy, f.from_stage, f.to_stage
            ));
        }

        // 6. Benchmark row (REQUIRED).
        s.push_str("\n## Benchmark\n\n");
        s.push_str("| Book | BTC hold | T-bill |\n|---|---|---|\n");
        s.push_str(&format!(
            "| {} | {} | {} |\n",
            pct(self.benchmark.book_return),
            pct(self.benchmark.btc_hold_return),
            pct(self.benchmark.tbill_return)
        ));

        s
    }
}
