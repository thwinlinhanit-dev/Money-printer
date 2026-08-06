//! Monthly report generator (OPS-6): the fund-of-one scoreboard, rendered from
//! journals/tracker numbers only — no hand-entered values, no invented cells.
//! An LLM may draft prose *around* these tables (spec 010), but every number
//! here comes from the input struct. Rendering is pure; the caller supplies the
//! already-computed figures (grounding contract).
//!
//! The RES-4 whale band-accuracy section is grounded the same way: the caller
//! loads the weekly trend with [`load_band_accuracy_trend`] (reads
//! `research/band_accuracy/band_accuracy.jsonl`, the journal
//! `run_band_accuracy.py` appends — spec 029 LIQ-6) and hands the rows to
//! [`MonthlyReport`]. Parsing is fail-closed (CONV-8): a corrupt journal line
//! fails the load rather than silently dropping a graded week. The delivery-
//! accountability section grounds the same way on the quiet-hours Telegram
//! ledger pair via [`load_telegram_batch`] / [`load_telegram_delivered`]
//! (reads `journal/telegram/batch.jsonl` — pending, un-flushed P3 dispatches
//! — and `journal/telegram/delivered.jsonl` — the flushed records of what
//! WAS delivered; empty ledgers are the healthy states).

use crate::alert::{Alert, Severity};
use crate::telegram::{TelegramDeliveredRow, TelegramPendingRow};
use std::path::{Path, PathBuf};

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

/// One graded week of the RES-4 whale band-accuracy trend (spec 029 LIQ-6):
/// the `liq.est_bands` estimate graded against spec 028 Hyperliquid real liq
/// prices. Loaded from `band_accuracy.jsonl`; the report renders only what
/// the loader grounded on the journal.
#[derive(Debug, Clone)]
pub struct BandAccuracyRow {
    /// ISO year-week of the graded data, `2026-W31` (sortable lexically).
    pub week: String,
    /// Paired observations (spec 028 real liq prices) that week.
    pub observations: u64,
    /// Mean relative error `Σ|est−real| / real / n`, a fraction ≥ 0.
    pub mean_relative_error: f64,
    /// Coverage: fraction of observations where the estimate was NOT on the
    /// dangerous side of the realized liq price (fraction in [0, 1]).
    pub coverage: f64,
    /// The run_id echo the job writes on each trend line (`run_id` of the
    /// `whale_study` run that graded this week — the join key to that run's
    /// SIM-10 record in `runs/index.jsonl`). Optional: the parser accepts
    /// trend lines that predate the echo (and test fixtures that omit it).
    pub run_id: Option<String>,
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
    /// Weekly RES-4 band-accuracy trend (spec 029 LIQ-6), one row per graded
    /// ISO week, grounded on `band_accuracy.jsonl`. Empty = the study has not
    /// produced evidence yet ⇒ the section renders "no data" (RES-5).
    pub band_accuracy: Vec<BandAccuracyRow>,
    /// Pending (un-flushed) quiet-hours Telegram dispatches, grounded on
    /// `journal/telegram/batch.jsonl` via [`load_telegram_batch`]. Delivery
    /// accountability (OPS-6/OPS-9): a queued line is an alert that was
    /// batched but NOT yet delivered — anything still queued at month-end is
    /// a delivery gap the report surfaces. Empty = every batch was flushed
    /// (the healthy state).
    pub telegram_pending: Vec<TelegramPendingRow>,
    /// Delivered quiet-hours Telegram dispatches, grounded on
    /// `journal/telegram/delivered.jsonl` via [`load_telegram_delivered`] —
    /// the flushed records `telegram-flush` appends per successful send.
    /// Delivery accountability (OPS-6): what WAS delivered this month, the
    /// evidence the ledger worked, alongside the pending queue above. Empty
    /// = no flush has recorded a delivery (the healthy "nothing delivered
    /// yet" state).
    pub telegram_delivered: Vec<TelegramDeliveredRow>,
    pub benchmark: Benchmark,
}

fn pct(x: f64) -> String {
    format!("{:+.2}%", x * 100.0)
}

/// Relative delivery age for the delivery log: `today` for the current day,
/// `{n}d ago` otherwise — grounded on the loader's injected clock (PD-3).
fn delivered_age(days: u64) -> String {
    if days == 0 {
        "today".to_string()
    } else {
        format!("{days}d ago")
    }
}

/// Inline stylesheet for the self-contained HTML report (no external assets).
const REPORT_CSS: &str = r#"
body{font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;margin:2rem auto;max-width:60rem;padding:0 1rem;color:#1c1e21;line-height:1.5}
h1{font-size:1.6rem;border-bottom:2px solid #d0d7de;padding-bottom:.4rem}
h2{font-size:1.15rem;margin-top:1.8rem;color:#24292f}
h3{font-size:1rem;margin-top:1.4rem;color:#24292f}
table{border-collapse:collapse;width:100%;margin:.5rem 0 1rem}
th,td{border:1px solid #d0d7de;padding:.4rem .6rem;text-align:left;font-size:.92rem}
th{background:#f6f8fa}
td.num{text-align:right;font-variant-numeric:tabular-nums}
tr.nodata td{color:#6e7781;font-style:italic}
.blended{font-size:1.05rem}
li{margin:.2rem 0}
footer{margin-top:2rem;font-size:.8rem;color:#6e7781;border-top:1px solid #d0d7de;padding-top:.5rem}
"#;

/// Escape a dynamic string for HTML text content. The five entities that
/// matter in text and quoted attributes — `&`, `<`, `>`, `"`, `'` — so a
/// strategy name or week string can never break out of the document (the
/// report is generated for the owner, but it is still machine-rendered).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

impl MonthlyReport {
    /// Render the §13 scoreboard to markdown. Every section plus the required
    /// benchmark row is always present (OPS-6); empty inputs render an
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

        // 6. Whale band-accuracy trend (RES-4, spec 029 LIQ-6): the weekly
        // `liq.est_bands` validation grade against spec 028 real liq prices.
        // Numbers come from the loaded `band_accuracy.jsonl` rows — grounded,
        // never invented.
        s.push_str("\n## Whale Band Accuracy (RES-4)\n\n");
        s.push_str("| Week | Observations | Mean rel. error | Coverage |\n|---|---|---|---|\n");
        if self.band_accuracy.is_empty() {
            s.push_str("| _no data_ | — | — | — |\n");
        }
        for b in &self.band_accuracy {
            s.push_str(&format!(
                "| {} | {} | {:.2}% | {:.1}% |\n",
                b.week,
                b.observations,
                b.mean_relative_error * 100.0,
                b.coverage * 100.0
            ));
        }

        // 7. Delivery accountability (OPS-6/OPS-9): the quiet-hours Telegram
        // batch ledger. A queued line is a P3 alert that was batched but NOT
        // yet delivered — anything still queued at month-end is a delivery
        // gap, surfaced here instead of silently lost (W-6). An empty ledger
        // is rendered strictly grounded: nothing is pending NOW (the ledger
        // records only what is queued, not delivery history).
        s.push_str("\n## Delivery Accountability (Telegram queue)\n\n");
        if self.telegram_pending.is_empty() {
            s.push_str("_No pending alerts in the quiet-hours Telegram queue._\n");
        } else {
            s.push_str("| Alert | Severity | Queued | Detail | Runbook |\n|---|---|---|---|---|\n");
            for p in &self.telegram_pending {
                s.push_str(&format!(
                    "| {} | {} | {}d | {} | {} |\n",
                    p.id,
                    p.severity.as_str(),
                    p.queued_days,
                    p.detail,
                    p.runbook
                ));
            }
        }

        // 7b. Delivery log — what WAS delivered this month (OPS-6): the
        // flushed records from `journal/telegram/delivered.jsonl`, written by
        // `telegram-flush` on each successful send. The accountability
        // counterpart to the pending queue above: evidence the ledger
        // actually worked. Rows are grounded on the loader (injected clock,
        // PD-3), never invented.
        s.push_str("\n### Delivered this month (Telegram delivery log)\n\n");
        if self.telegram_delivered.is_empty() {
            s.push_str("_No deliveries recorded in the Telegram delivery log._\n");
        } else {
            s.push_str("| Alert | Delivered |\n|---|---|\n");
            for d in &self.telegram_delivered {
                s.push_str(&format!(
                    "| {} | {} |\n",
                    d.id,
                    delivered_age(d.delivered_days_ago)
                ));
            }
        }

        // 8. Benchmark row (REQUIRED).
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

    /// Render the §13 scoreboard to a self-contained HTML page (spec 009:
    /// markdown + HTML in `journal/reports/{YYYY-MM}/`). Same grounding
    /// contract as [`Self::render_markdown`] — every number comes from the
    /// input struct, nothing is computed or invented; empty inputs render an
    /// explicit "no data" row. Dynamic strings (strategy names, weeks) are
    /// HTML-escaped so no external markup can break the document.
    pub fn render_html(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Monthly Report — {}</title>\n",
            html_escape(&self.month)
        ));
        s.push_str("<style>");
        s.push_str(REPORT_CSS);
        s.push_str("</style>\n</head>\n<body>\n");

        s.push_str(&format!(
            "<h1>Monthly Report — {}</h1>\n",
            html_escape(&self.month)
        ));
        s.push_str(&format!(
            "<p class=\"blended\">Blended net return <strong>{}</strong>, max drawdown <strong>{}</strong>.</p>\n",
            pct(self.blended_return),
            pct(self.blended_max_drawdown)
        ));

        // 1. Equity & Drawdown (per strategy).
        s.push_str("<h2>Equity &amp; Drawdown</h2>\n");
        s.push_str(
            "<table><thead><tr><th>Strategy</th><th>Net return</th><th>Max DD</th></tr></thead><tbody>\n",
        );
        if self.strategies.is_empty() {
            s.push_str("<tr class=\"nodata\"><td colspan=\"3\">no data</td></tr>\n");
        }
        for r in &self.strategies {
            s.push_str(&format!(
                "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>\n",
                html_escape(&r.strategy),
                pct(r.net_return),
                pct(r.max_drawdown)
            ));
        }
        s.push_str("</tbody></table>\n");

        // 2. Expectancy after costs.
        s.push_str("<h2>Expectancy (after costs)</h2>\n");
        s.push_str(
            "<table><thead><tr><th>Strategy</th><th>Expectancy (R)</th><th>Trades</th><th>Win rate</th></tr></thead><tbody>\n",
        );
        if self.strategies.is_empty() {
            s.push_str("<tr class=\"nodata\"><td colspan=\"4\">no data</td></tr>\n");
        }
        for r in &self.strategies {
            s.push_str(&format!(
                "<tr><td>{}</td><td class=\"num\">{:+.3}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>\n",
                html_escape(&r.strategy),
                r.expectancy_r,
                r.trades,
                pct(r.win_rate)
            ));
        }
        s.push_str("</tbody></table>\n");

        // 3. Tracking error (live vs paper vs backtest).
        s.push_str("<h2>Tracking Error (live vs paper vs backtest)</h2>\n");
        s.push_str(
            "<table><thead><tr><th>Strategy</th><th>Live</th><th>Paper</th><th>Backtest</th><th>Tracking err</th></tr></thead><tbody>\n",
        );
        if self.tracking.is_empty() {
            s.push_str("<tr class=\"nodata\"><td colspan=\"5\">no data</td></tr>\n");
        }
        for t in &self.tracking {
            s.push_str(&format!(
                "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>\n",
                html_escape(&t.strategy),
                pct(t.live_return),
                pct(t.paper_return),
                pct(t.backtest_return),
                pct(t.tracking_error)
            ));
        }
        s.push_str("</tbody></table>\n");

        // 4. Cost breakdown.
        s.push_str("<h2>Cost Breakdown</h2>\n");
        s.push_str(&format!(
            "<table><thead><tr><th>Fees</th><th>Slippage vs model</th><th>Funding</th><th>Infra</th></tr></thead><tbody>\n<tr>\
             <td class=\"num\">{:.2}</td><td class=\"num\">{:.2}</td><td class=\"num\">{:.2}</td><td class=\"num\">{:.2}</td>\
             </tr></tbody></table>\n",
            self.costs.fees,
            self.costs.slippage_vs_model,
            self.costs.funding,
            self.costs.infra
        ));

        // 5. Funnel transitions & kills.
        s.push_str("<h2>Funnel Transitions &amp; Kills</h2>\n");
        if self.funnel.is_empty() {
            s.push_str("<p class=\"nodata\">No transitions this month.</p>\n");
        } else {
            s.push_str("<ul>\n");
            for f in &self.funnel {
                let arrow = if f.demotion {
                    "⏬ kill/demote"
                } else {
                    "⏫ promote"
                };
                s.push_str(&format!(
                    "<li>{} {}: {} → {}</li>\n",
                    arrow,
                    html_escape(&f.strategy),
                    html_escape(&f.from_stage),
                    html_escape(&f.to_stage)
                ));
            }
            s.push_str("</ul>\n");
        }

        // 6. Whale band-accuracy trend (RES-4, spec 029 LIQ-6) — grounded on
        // the loaded `band_accuracy.jsonl` rows, never invented.
        s.push_str("<h2>Whale Band Accuracy (RES-4)</h2>\n");
        s.push_str(
            "<table><thead><tr><th>Week</th><th>Observations</th><th>Mean rel. error</th><th>Coverage</th></tr></thead><tbody>\n",
        );
        if self.band_accuracy.is_empty() {
            s.push_str("<tr class=\"nodata\"><td colspan=\"4\">no data</td></tr>\n");
        }
        for b in &self.band_accuracy {
            s.push_str(&format!(
                "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{:.2}%</td><td class=\"num\">{:.1}%</td></tr>\n",
                html_escape(&b.week),
                b.observations,
                b.mean_relative_error * 100.0,
                b.coverage * 100.0
            ));
        }
        s.push_str("</tbody></table>\n");

        // 7. Delivery accountability (OPS-6/OPS-9) — grounded on the loaded
        // `journal/telegram/batch.jsonl` rows, never invented. An empty
        // ledger renders strictly grounded: nothing pending NOW.
        s.push_str("<h2>Delivery Accountability (Telegram queue)</h2>\n");
        if self.telegram_pending.is_empty() {
            s.push_str(
                "<p class=\"nodata\">No pending alerts in the quiet-hours Telegram queue.</p>\n",
            );
        } else {
            s.push_str(
                "<table><thead><tr><th>Alert</th><th>Severity</th><th>Queued</th><th>Detail</th><th>Runbook</th></tr></thead><tbody>\n",
            );
            for p in &self.telegram_pending {
                s.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td class=\"num\">{}d</td><td>{}</td><td>{}</td></tr>\n",
                    html_escape(&p.id),
                    p.severity.as_str(),
                    p.queued_days,
                    html_escape(&p.detail),
                    html_escape(&p.runbook)
                ));
            }
            s.push_str("</tbody></table>\n");
        }

        // 7b. Delivery log — what WAS delivered this month (OPS-6), grounded
        // on the loaded `delivered.jsonl` rows, never invented. An empty log
        // renders strictly grounded: no flush has recorded a delivery.
        s.push_str("<h3>Delivered this month (Telegram delivery log)</h3>\n");
        if self.telegram_delivered.is_empty() {
            s.push_str(
                "<p class=\"nodata\">No deliveries recorded in the Telegram delivery log.</p>\n",
            );
        } else {
            s.push_str("<table><thead><tr><th>Alert</th><th>Delivered</th></tr></thead><tbody>\n");
            for d in &self.telegram_delivered {
                s.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td></tr>\n",
                    html_escape(&d.id),
                    delivered_age(d.delivered_days_ago)
                ));
            }
            s.push_str("</tbody></table>\n");
        }

        // 8. Benchmark row (REQUIRED, OPS-6).
        s.push_str("<h2>Benchmark</h2>\n");
        s.push_str(&format!(
            "<table><thead><tr><th>Book</th><th>BTC hold</th><th>T-bill</th></tr></thead><tbody>\n<tr>\
             <td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td>\
             </tr></tbody></table>\n",
            pct(self.benchmark.book_return),
            pct(self.benchmark.btc_hold_return),
            pct(self.benchmark.tbill_return)
        ));

        s.push_str(&format!(
            "<footer>Generated from journals/tracker — every number grounded (OPS-6). {}</footer>\n</body>\n</html>\n",
            html_escape(&self.month)
        ));
        s
    }
}

/// Parse the weekly band-accuracy trend journal (`band_accuracy.jsonl`, one
/// JSON object per graded week — the exact shape `run_band_accuracy.py`
/// appends: `week`, `n`, `mean_relative_error`, `coverage`, plus the optional
/// provenance echoes `run_id`/`config_hash`) into rows sorted by ISO week.
///
/// Fail-closed (CONV-8): every non-empty line must be a JSON object with the
/// job's field set well-typed — `week` a non-empty string, `n` a non-negative
/// integer, `mean_relative_error` a finite `≥ 0` number, `coverage` a finite
/// number in `[0, 1]`. A corrupt line is evidence corruption and fails the
/// whole load (naming the line) rather than silently dropping a graded week;
/// blank lines (the append-only journal's trailing newline) are skipped.
pub fn parse_band_accuracy_trend(text: &str) -> Result<Vec<BandAccuracyRow>, String> {
    let mut rows = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let lineno = idx + 1;
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| format!("band_accuracy.jsonl line {lineno}: {e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| format!("band_accuracy.jsonl line {lineno}: not an object"))?;
        let week = obj
            .get("week")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("band_accuracy.jsonl line {lineno}: missing 'week'"))?
            .to_string();
        let observations = obj
            .get("n")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("band_accuracy.jsonl line {lineno}: missing 'n'"))?;
        let mean_relative_error = obj
            .get("mean_relative_error")
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| {
                format!("band_accuracy.jsonl line {lineno}: invalid 'mean_relative_error'")
            })?;
        let coverage = obj
            .get("coverage")
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
            .ok_or_else(|| format!("band_accuracy.jsonl line {lineno}: invalid 'coverage'"))?;
        let run_id = obj
            .get("run_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        rows.push(BandAccuracyRow {
            week,
            observations,
            mean_relative_error,
            coverage,
            run_id,
        });
    }
    // Deterministic report order: ISO weeks sort lexically ("2026-W30" <
    // "2026-W31"), independent of the append order on disk (CONV-10).
    rows.sort_by(|a, b| a.week.cmp(&b.week));
    Ok(rows)
}

/// Load the trend journal from disk into rows for the report.
///
/// A missing file is a "no data" month, not an error (RES-5): the weekly
/// study may not have produced evidence yet (no spec 028 census logs). A
/// present-but-corrupt journal fails closed (CONV-8) — evidence corruption is
/// never silently dropped from the report.
pub fn load_band_accuracy_trend(path: &Path) -> Result<Vec<BandAccuracyRow>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    parse_band_accuracy_trend(&text)
}

/// Append one record line to `<runs-dir>/index.jsonl` — the shared RES-4/SIM-10
/// run tracker (append-only, W-6; fsynced so a crash cannot lose the line,
/// OPS-12 spirit — the same contract as the whale_study binary's own record
/// append and `append_batch`). Used by `band-accuracy-decay` to journal each
/// weekly verdict as its own line, correlated to the study's run record by
/// `run_id`/`week`.
pub fn append_run_record(runs_dir: &Path, record: &serde_json::Value) -> Result<(), String> {
    std::fs::create_dir_all(runs_dir).map_err(|e| format!("create {}: {e}", runs_dir.display()))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(runs_dir.join("index.jsonl"))
        .map_err(|e| format!("open runs index: {e}"))?; // JSONL: exactly one record per physical line (append-only W-6).
    let mut line = serde_json::to_string(record).map_err(|e| e.to_string())?;
    line.push('\n');
    std::io::Write::write_all(&mut f, line.as_bytes())
        .map_err(|e| format!("write runs index: {e}"))?;
    f.sync_all().map_err(|e| format!("sync runs index: {e}"))?;
    Ok(())
}

/// Write the monthly report to disk as markdown + HTML (spec 009: rendered to
/// `journal/reports/{YYYY-MM}/`). The caller passes the month directory (e.g.
/// `journal/reports/2026-06`); it is created if missing, and `report.md` +
/// `report.html` are written (overwrite — a regenerated render is the same
/// month's scoreboard, not append-only evidence, W-6). Returns the two paths.
pub fn write_monthly_report(
    dir: &Path,
    report: &MonthlyReport,
) -> Result<(PathBuf, PathBuf), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let md_path = dir.join("report.md");
    let html_path = dir.join("report.html");
    std::fs::write(&md_path, report.render_markdown())
        .map_err(|e| format!("write {}: {e}", md_path.display()))?;
    std::fs::write(&html_path, report.render_html())
        .map_err(|e| format!("write {}: {e}", html_path.display()))?;
    Ok((md_path, html_path))
}

/// Weekly band-accuracy drift/decay check (OPS-13): raises `band-accuracy-decay`
/// (P3) when the RES-4 validation quality degrades over the trailing weeks of
/// the trend, mirroring the RES-3 edge-decay flag (spec 010) — needs ≥ 12
/// graded weeks, then compares the trailing 4-week mean against the 12-week
/// mean:
///
/// - **coverage decay**: `mean4_cov < 0.5 × mean12_cov`, and only when the
///   12-week baseline was meaningfully covered (`mean12_cov ≥ 0.5`) — a grade
///   that was never good is a different problem than one that decayed;
/// - **MRE decay**: `mean4_mre > 2.0 × mean12_mre` (the error doubled), with
///   a positive baseline (`mean12_mre > 0` — a perfect baseline has nothing to
///   decay from).
///
/// Faithful to RES-3: `mean12` is the mean over the full trailing 12-row
/// window (including the recent 4), `mean4` over the recent 4. The trailing 12
/// rows are the most recent *graded* weeks of the journal; the study skips
/// weeks without recorded census data (LIQ-10), so gaps in the ISO calendar are
/// not decay. Deterministic (CONV-9), alert-only (W-6 — this check never
/// mutates the trend journal).
pub fn band_accuracy_decay_alert(rows: &[BandAccuracyRow], dedupe_ns: i64) -> Option<Alert> {
    const WINDOW: usize = 12;
    const TRAILING: usize = 4;
    if rows.len() < WINDOW {
        return None; // not enough graded weeks to call decay (RES-3)
    }
    let last = &rows[rows.len() - WINDOW..];
    let trailing = &last[WINDOW - TRAILING..];
    let mean = |w: &[BandAccuracyRow], f: fn(&BandAccuracyRow) -> f64| {
        w.iter().map(f).sum::<f64>() / w.len() as f64
    };
    let base_cov = mean(last, |r| r.coverage);
    let base_mre = mean(last, |r| r.mean_relative_error);
    let trail_cov = mean(trailing, |r| r.coverage);
    let trail_mre = mean(trailing, |r| r.mean_relative_error);

    let cov_decayed = base_cov >= 0.5 && trail_cov < 0.5 * base_cov;
    let mre_decayed = base_mre > 0.0 && trail_mre > 2.0 * base_mre;
    if !cov_decayed && !mre_decayed {
        return None;
    }

    let span = |w: &[BandAccuracyRow]| match (w.first(), w.last()) {
        (Some(a), Some(b)) => format!("{}..{}", a.week, b.week),
        _ => "?".to_string(),
    };
    let mut parts = Vec::new();
    if cov_decayed {
        parts.push(format!(
            "coverage trailing 4-wk mean {trail_cov:.3} < half of 12-wk mean {base_cov:.3} ({})",
            span(trailing)
        ));
    }
    if mre_decayed {
        parts.push(format!(
            "MRE trailing 4-wk mean {trail_mre:.3} > double 12-wk mean {base_mre:.3} ({})",
            span(trailing)
        ));
    }
    Some(Alert::new(
        "band-accuracy-decay",
        Severity::P3,
        dedupe_ns,
        format!("RES-4 band accuracy decaying: {}", parts.join("; ")),
    ))
}
