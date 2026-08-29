//! Standalone backtest harness for the 6 climax variant patterns.
//! Reads BTC daily OHLCV from JSON, runs each pattern detector,
//! simulates trades (enter next open, exit after N bars or trailing stop),
//! and computes professional performance metrics.
//!
//! Usage: cargo run --bin backtest_variants -- <path_to_json> [output_dir]

use serde::Deserialize;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;

// ── Bar data ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize)]
struct RawBar {
    time: u64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

#[derive(Debug, Clone, Copy)]
struct Bar {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    vol: f64,
    buy_vol: f64,
    sell_vol: f64,
    vwap: f64,
    n_trades: u64,
    first_ts_ns: i64,
    last_ts_ns: i64,
    close_ts_ns: i64,
}

impl From<&RawBar> for Bar {
    fn from(r: &RawBar) -> Self {
        Bar {
            open: r.open,
            high: r.high,
            low: r.low,
            close: r.close,
            vol: r.volume,
            buy_vol: r.volume,
            sell_vol: 0.0,
            vwap: r.close,
            n_trades: 1,
            first_ts_ns: r.time as i64 * 1_000_000_000,
            last_ts_ns: r.time as i64 * 1_000_000_000,
            close_ts_ns: r.time as i64 * 1_000_000_000,
        }
    }
}

// ── ATR helper ──────────────────────────────────────────────

fn atr(bars: &[Bar], n: usize) -> Option<f64> {
    if n == 0 || bars.len() < n {
        return None;
    }
    let win = &bars[bars.len() - n..];
    let mut sum = 0.0_f64;
    for (i, b) in win.iter().enumerate() {
        let hl = b.high - b.low;
        if !hl.is_finite() || hl < 0.0 {
            return None;
        }
        let tr = if i == 0 {
            hl
        } else {
            let pc = win[i - 1].close;
            hl.max((b.high - pc).abs()).max((b.low - pc).abs())
        };
        sum += tr;
    }
    let a = sum / n as f64;
    if a.is_finite() && a > 0.0 {
        Some(a)
    } else {
        None
    }
}

fn close_pos(b: &Bar) -> Option<f64> {
    let spread = b.high - b.low;
    if !spread.is_finite() || spread <= 0.0 {
        return None;
    }
    let pos = (b.close - b.low) / spread;
    if pos.is_finite() {
        Some(pos)
    } else {
        None
    }
}

// ── Pattern detectors (same logic as climax_variants.rs) ─────

fn detect_v1(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 21 {
        return false;
    }
    let current = &bars[n - 1];
    if !current.close.is_finite() || !current.vol.is_finite() || current.vol <= 0.0 {
        return false;
    }
    // Highest vol in lookback
    let window = &bars[n - 21..n - 1];
    let max_vol = window.iter().map(|b| b.vol).fold(0.0_f64, f64::max);
    if current.vol < max_vol {
        return false;
    }
    // Vol > 2.5x SMA
    let vol_sma = window.iter().map(|b| b.vol).sum::<f64>() / 20.0;
    if vol_sma <= 0.0 || current.vol <= vol_sma * 2.5 {
        return false;
    }
    // Spread > 1.5x ATR
    let spread = current.high - current.low;
    if let Some(a) = atr(&bars[..n - 1], 20) {
        if spread <= a * 1.5 {
            return false;
        }
    }
    // Mid closePos
    let cp = close_pos(current).unwrap_or(0.5);
    cp >= 0.30 && cp <= 0.70
}

fn detect_v2(bars: &[Bar], streak: usize) -> bool {
    let n = bars.len();
    if n < streak + 21 || n < 22 {
        return false;
    }
    // Check streak
    for i in 0..streak {
        if bars[n - 2 - i].close >= bars[n - 3 - i].close {
            return false;
        }
    }
    // Current bar must be down too
    if bars[n - 1].close >= bars[n - 2].close {
        return false;
    }
    // Volume spike
    let vol_sma: f64 = bars[n - 22..n - 1].iter().map(|b| b.vol).sum::<f64>() / 20.0;
    if vol_sma <= 0.0 || bars[n - 1].vol <= vol_sma * 1.5 {
        return false;
    }
    // Cumulative drop
    let start_close = bars[n - 1 - streak].close;
    if start_close <= 0.0 {
        return false;
    }
    let cum_drop = (bars[n - 1].close - start_close) / start_close * 100.0;
    cum_drop <= -8.0
}

fn detect_v3(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 26 {
        return false;
    }
    let short_a = atr(bars, 5);
    let long_a = atr(bars, 20);
    let prev_short = atr(&bars[..n - 1], 5);
    let prev_long = atr(&bars[..n - 1], 20);
    
    if let (Some(sa), Some(la), Some(ps), Some(pl)) = (short_a, long_a, prev_short, prev_long) {
        if pl <= 0.0 || la <= 0.0 {
            return false;
        }
        let was_compressed = ps / pl < 0.60;
        let now_expanded = sa / la > 0.60;
        was_compressed && now_expanded && sa > ps * 1.5
    } else {
        false
    }
}

fn detect_v4(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 16 {
        return false;
    }
    let current = &bars[n - 1];
    if !current.close.is_finite() || !current.vol.is_finite() || current.vol <= 0.0 {
        return false;
    }
    // New 10-bar low
    let prev_lows = &bars[n - 11..n - 1];
    let min_low = prev_lows.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    if current.low > min_low {
        return false;
    }
    // Volume declining: SMA(3) < SMA(10) * 0.8
    let long_sma: f64 = bars[n - 10..].iter().map(|b| b.vol).sum::<f64>() / 10.0;
    let short_sma: f64 = bars[n - 3..].iter().map(|b| b.vol).sum::<f64>() / 3.0;
    if long_sma <= 0.0 {
        return false;
    }
    short_sma < long_sma * 0.80
}

fn detect_v5(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 15 {
        return false;
    }
    let current = &bars[n - 1];
    if !current.close.is_finite() || !current.high.is_finite() || !current.low.is_finite() {
        return false;
    }
    let spread = current.high - current.low;
    if spread <= 0.0 {
        return false;
    }
    // Range > 1.2x ATR
    if let Some(a) = atr(&bars[..n - 1], 14) {
        if spread <= a * 1.2 {
            return false;
        }
    }
    // Body < 20% of range
    let body = (current.close - current.open).abs();
    if body / spread >= 0.20 {
        return false;
    }
    // Dominant wick > 50%
    let lower_wick = (current.close.min(current.open) - current.low).max(0.0) / spread;
    let upper_wick = (current.high - current.close.max(current.open)).max(0.0) / spread;
    lower_wick.max(upper_wick) >= 0.50
}

fn detect_v6(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 20 {
        return false;
    }
    let current_atr = atr(bars, 14);
    let prev_atr = atr(&bars[..n - 5], 14);
    match (current_atr, prev_atr) {
        (Some(ca), Some(pa)) if pa > 0.0 => ca / pa > 1.8,
        _ => false,
    }
}

// ── Trade simulation ────────────────────────────────────────

#[derive(Debug, Clone)]
struct Trade {
    entry_date: String,
    exit_date: String,
    entry_price: f64,
    exit_price: f64,
    pnl_pct: f64,
    bars_held: usize,
    pattern: &'static str,
}

fn simulate_trades(
    raw_bars: &[RawBar],
    signals: &[bool],
    hold_bars: usize,
    trailing_stop_atr: Option<f64>,
    pattern_name: &'static str,
) -> Vec<Trade> {
    let bars: Vec<Bar> = raw_bars.iter().map(Bar::from).collect();
    let mut trades = Vec::new();
    let mut i = 0;
    
    while i < bars.len() {
        if signals[i] {
            // Enter at next bar's open
            let entry_idx = i + 1;
            if entry_idx >= bars.len() {
                break;
            }
            let entry_price = bars[entry_idx].open;
            let entry_date = ts_to_date(raw_bars[entry_idx].time);
            
            // Exit: hold N bars or trailing stop
            let mut exit_idx = (entry_idx + hold_bars).min(bars.len() - 1);
            let mut peak = entry_price;
            let mut stopped = false;
            
            for j in entry_idx..=exit_idx {
                let b = &bars[j];
                if b.high > peak {
                    peak = b.high;
                }
                if let Some(atr_mult) = trailing_stop_atr {
                    if let Some(a) = atr(&bars[..=j], 14) {
                        let stop = peak - atr_mult * a;
                        if b.low <= stop {
                            exit_idx = j;
                            stopped = true;
                            break;
                        }
                    }
                }
            }
            
            let exit_price = if stopped {
                // Stopped out at the stop level
                let a = atr(&bars[..=exit_idx], 14).unwrap_or(0.0);
                peak - trailing_stop_atr.unwrap_or(0.0) * a
            } else {
                bars[exit_idx].close
            };
            
            let exit_date = ts_to_date(raw_bars[exit_idx].time);
            let pnl = (exit_price - entry_price) / entry_price * 100.0;
            
            trades.push(Trade {
                entry_date,
                exit_date,
                entry_price,
                exit_price,
                pnl_pct: pnl,
                bars_held: exit_idx - entry_idx,
                pattern: pattern_name,
            });
            
            i = exit_idx + 1;
        } else {
            i += 1;
        }
    }
    
    trades
}

fn ts_to_date(ts: u64) -> String {
    let days = ts / 86400;
    let secs = ts % 86400;
    let _ = secs;
    // Simple UTC date from epoch days
    let mut y = 1970u64;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let months = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0u64;
    while m < 12 && remaining >= months[m as usize] {
        remaining -= months[m as usize];
        m += 1;
    }
    format!("{y}-{:02}-{:02}", m + 1, remaining + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

// ── Performance metrics ─────────────────────────────────────

struct Metrics {
    total_trades: usize,
    win_rate: f64,
    avg_pnl: f64,
    median_pnl: f64,
    std_dev: f64,
    sharpe: f64,
    sortino: f64,
    calmar: f64,
    max_drawdown: f64,
    profit_factor: f64,
    expectancy: f64,
    avg_win: f64,
    avg_loss: f64,
    max_consecutive_losses: usize,
    avg_bars_held: f64,
    total_return: f64,
    cagr: f64,
    payoff_ratio: f64,
}

fn compute_metrics(trades: &[Trade], annual_risk_free: f64) -> Metrics {
    if trades.is_empty() {
        return Metrics {
            total_trades: 0,
            win_rate: 0.0,
            avg_pnl: 0.0,
            median_pnl: 0.0,
            std_dev: 0.0,
            sharpe: 0.0,
            sortino: 0.0,
            calmar: 0.0,
            max_drawdown: 0.0,
            profit_factor: 0.0,
            expectancy: 0.0,
            avg_win: 0.0,
            avg_loss: 0.0,
            max_consecutive_losses: 0,
            avg_bars_held: 0.0,
            total_return: 0.0,
            cagr: 0.0,
            payoff_ratio: 0.0,
        };
    }
    
    let n = trades.len() as f64;
    let pnls: Vec<f64> = trades.iter().map(|t| t.pnl_pct).collect();
    
    // Win rate
    let wins: Vec<f64> = pnls.iter().filter(|&&p| p > 0.0).copied().collect();
    let losses: Vec<f64> = pnls.iter().filter(|&&p| p <= 0.0).copied().collect();
    let win_rate = wins.len() as f64 / n * 100.0;
    
    // Average PnL
    let avg_pnl = pnls.iter().sum::<f64>() / n;
    
    // Median
    let mut sorted = pnls.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_pnl = if sorted.len() % 2 == 0 {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };
    
    // Std dev
    let variance = pnls.iter().map(|p| (p - avg_pnl).powi(2)).sum::<f64>() / n;
    let std_dev = variance.sqrt();
    
    // Sharpe (annualized, assuming ~12 trades/year for daily signals)
    let trades_per_year = (n / (trades.last().unwrap().bars_held as f64 / 365.0)).max(1.0);
    let excess_return = avg_pnl * trades_per_year - annual_risk_free;
    let sharpe = if std_dev > 0.0 {
        excess_return / (std_dev * trades_per_year.sqrt())
    } else {
        0.0
    };
    
    // Sortino (downside deviation only)
    let downside_sq: f64 = pnls.iter().map(|p| p.min(0.0).powi(2)).sum::<f64>() / n;
    let downside_dev = downside_sq.sqrt();
    let sortino = if downside_dev > 0.0 {
        excess_return / (downside_dev * trades_per_year.sqrt())
    } else {
        0.0
    };
    
    // Max drawdown (from equity curve)
    let mut equity = 100.0_f64;
    let mut peak = 100.0_f64;
    let mut max_dd = 0.0_f64;
    for t in trades {
        equity *= 1.0 + t.pnl_pct / 100.0;
        if equity > peak {
            peak = equity;
        }
        let dd = (peak - equity) / peak * 100.0;
        if dd > max_dd {
            max_dd = dd;
        }
    }
    
    // Calmar
    let total_return_pct = (equity / 100.0 - 1.0) * 100.0;
    let total_bars: usize = trades.iter().map(|t| t.bars_held).sum();
    let years = total_bars as f64 / 365.0;
    let cagr = if years > 0.0 {
        ((equity / 100.0).powf(1.0 / years) - 1.0) * 100.0
    } else {
        0.0
    };
    let calmar = if max_dd > 0.0 {
        cagr / max_dd
    } else {
        0.0
    };
    
    // Profit factor
    let gross_profit: f64 = wins.iter().sum();
    let gross_loss: f64 = losses.iter().map(|l| l.abs()).sum();
    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };
    
    // Expectancy (avg $ won per $ risked)
    let avg_win = if !wins.is_empty() {
        wins.iter().sum::<f64>() / wins.len() as f64
    } else {
        0.0
    };
    let avg_loss = if !losses.is_empty() {
        losses.iter().sum::<f64>() / losses.len() as f64
    } else {
        0.0
    };
    let win_loss_ratio = if avg_loss != 0.0 {
        avg_win / avg_loss.abs()
    } else {
        0.0
    };
    let expectancy = (win_rate / 100.0 * avg_win) + ((100.0 - win_rate) / 100.0 * avg_loss);
    
    // Max consecutive losses
    let mut max_consec = 0usize;
    let mut current_consec = 0usize;
    for &p in &pnls {
        if p <= 0.0 {
            current_consec += 1;
            if current_consec > max_consec {
                max_consec = current_consec;
            }
        } else {
            current_consec = 0;
        }
    }
    
    let avg_bars_held = trades.iter().map(|t| t.bars_held as f64).sum::<f64>() / n;
    
    Metrics {
        total_trades: trades.len(),
        win_rate,
        avg_pnl,
        median_pnl,
        std_dev,
        sharpe,
        sortino,
        calmar,
        max_drawdown: max_dd,
        profit_factor,
        expectancy,
        avg_win,
        avg_loss,
        max_consecutive_losses: max_consec,
        avg_bars_held,
        total_return: total_return_pct,
        cagr,
        payoff_ratio: win_loss_ratio,
    }
}

// ── Report generation ───────────────────────────────────────

fn generate_report(
    all_trades: &[(String, Vec<Trade>)],
    output_dir: &Path,
) {
    let mut report = String::new();
    report.push_str("# Climax Variant Patterns — Backtest Report\n\n");
    report.push_str("**Asset:** BTCUSDT Daily  \n");
    report.push_str("**Period:** 2017-08-17 to 2026-08-27 (3,298 bars)  \n");
    report.push_str("**Strategy:** Buy next open on signal, hold 10 days  \n");
    report.push_str("**Trailing stop:** 2× ATR(14)  \n\n");
    report.push_str("---\n\n");
    
    // Summary table
    report.push_str("## Summary\n\n");
    report.push_str("| Pattern | Trades | Win% | Avg PnL | Sharpe | Sortino | Max DD | Profit Factor | Expectancy | Avg Hold |\n");
    report.push_str("|---------|--------|------|---------|--------|---------|--------|---------------|------------|----------|\n");
    
    for (name, trades) in all_trades {
        let m = compute_metrics(trades, 4.0); // 4% risk-free
        report.push_str(&format!(
            "| {} | {} | {:.1}% | {:.2}% | {:.2} | {:.2} | {:.1}% | {:.2} | {:.2}% | {:.0}d |\n",
            name, m.total_trades, m.win_rate, m.avg_pnl, m.sharpe, m.sortino,
            m.max_drawdown, m.profit_factor, m.expectancy, m.avg_bars_held
        ));
    }
    
    report.push_str("\n---\n\n");
    
    // Detailed metrics per pattern
    for (name, trades) in all_trades {
        let m = compute_metrics(trades, 4.0);
        report.push_str(&format!("## {} — Detailed Metrics\n\n", name));
        report.push_str(&format!("- **Total trades:** {}\n", m.total_trades));
        report.push_str(&format!("- **Win rate:** {:.1}%\n", m.win_rate));
        report.push_str(&format!("- **Average PnL:** {:.2}%\n", m.avg_pnl));
        report.push_str(&format!("- **Median PnL:** {:.2}%\n", m.median_pnl));
        report.push_str(&format!("- **Std deviation:** {:.2}%\n", m.std_dev));
        report.push_str(&format!("- **Sharpe ratio:** {:.2}\n", m.sharpe));
        report.push_str(&format!("- **Sortino ratio:** {:.2}\n", m.sortino));
        report.push_str(&format!("- **Calmar ratio:** {:.2}\n", m.calmar));
        report.push_str(&format!("- **Max drawdown:** {:.1}%\n", m.max_drawdown));
        report.push_str(&format!("- **Profit factor:** {:.2}\n", m.profit_factor));
        report.push_str(&format!("- **Expectancy:** {:.2}%\n", m.expectancy));
        report.push_str(&format!("- **Payoff ratio:** {:.2}\n", m.payoff_ratio));
        report.push_str(&format!("- **Avg win:** {:.2}%\n", m.avg_win));
        report.push_str(&format!("- **Avg loss:** {:.2}%\n", m.avg_loss));
        report.push_str(&format!("- **Max consecutive losses:** {}\n", m.max_consecutive_losses));
        report.push_str(&format!("- **Avg bars held:** {:.1}\n", m.avg_bars_held));
        report.push_str(&format!("- **Total return:** {:.1}%\n", m.total_return));
        report.push_str(&format!("- **CAGR:** {:.1}%\n", m.cagr));
        
        // Trade log (last 20)
        if trades.len() > 0 {
            report.push_str("\n### Trade Log (last 20)\n\n");
            report.push_str("| # | Entry | Exit | Entry $ | Exit $ | PnL% | Bars |\n");
            report.push_str("|---|-------|------|---------|--------|------|------|\n");
            let start = trades.len().saturating_sub(20);
            for (i, t) in trades[start..].iter().enumerate() {
                report.push_str(&format!(
                    "| {} | {} | {} | ${:.0} | ${:.0} | {:.2}% | {} |\n",
                    start + i + 1, t.entry_date, t.exit_date, t.entry_price, t.exit_price,
                    t.pnl_pct, t.bars_held
                ));
            }
        }
        report.push_str("\n");
    }
    
    // Combined portfolio
    report.push_str("---\n\n");
    report.push_str("## Combined Portfolio (all patterns)\n\n");
    let all: Vec<Trade> = all_trades.iter().flat_map(|(_, t)| t.clone()).collect();
    let mut sorted_all = all.clone();
    sorted_all.sort_by(|a, b| a.entry_date.cmp(&b.entry_date));
    let m = compute_metrics(&sorted_all, 4.0);
    report.push_str(&format!("- **Total trades:** {}\n", m.total_trades));
    report.push_str(&format!("- **Win rate:** {:.1}%\n", m.win_rate));
    report.push_str(&format!("- **Average PnL:** {:.2}%\n", m.avg_pnl));
    report.push_str(&format!("- **Sharpe ratio:** {:.2}\n", m.sharpe));
    report.push_str(&format!("- **Sortino ratio:** {:.2}\n", m.sortino));
    report.push_str(&format!("- **Max drawdown:** {:.1}%\n", m.max_drawdown));
    report.push_str(&format!("- **Profit factor:** {:.2}\n", m.profit_factor));
    report.push_str(&format!("- **Total return:** {:.1}%\n", m.total_return));
    report.push_str(&format!("- **CAGR:** {:.1}%\n", m.cagr));
    
    // Yearly breakdown
    report.push_str("\n### Yearly Breakdown\n\n");
    report.push_str("| Year | Trades | Win% | Total PnL | Avg PnL |\n");
    report.push_str("|------|--------|------|-----------|----------|\n");
    
    let mut yearly: std::collections::BTreeMap<String, Vec<f64>> = std::collections::BTreeMap::new();
    for t in &sorted_all {
        let year = t.entry_date[..4].to_string();
        yearly.entry(year).or_default().push(t.pnl_pct);
    }
    for (year, pnls) in &yearly {
        let n = pnls.len();
        let wins = pnls.iter().filter(|&&p| p > 0.0).count();
        let total: f64 = pnls.iter().sum();
        let avg = total / n as f64;
        report.push_str(&format!(
            "| {} | {} | {:.0}% | {:.1}% | {:.2}% |\n",
            year, n, wins as f64 / n as f64 * 100.0, total, avg
        ));
    }
    
    fs::write(output_dir.join("backtest_report.md"), &report).expect("write report");
    println!("Report written to {:?}", output_dir.join("backtest_report.md"));
    
    // Also write CSV trade log
    let mut csv = String::from("pattern,entry_date,exit_date,entry_price,exit_price,pnl_pct,bars_held\n");
    for (name, trades) in all_trades {
        for t in trades {
            csv.push_str(&format!(
                "{},{},{},{:.2},{:.2},{:.2},{}\n",
                name, t.entry_date, t.exit_date, t.entry_price, t.exit_price, t.pnl_pct, t.bars_held
            ));
        }
    }
    fs::write(output_dir.join("trades.csv"), &csv).expect("write csv");
    println!("Trade log written to {:?}", output_dir.join("trades.csv"));
}

// ── Main ────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input_path = args.get(1).map(|s| s.as_str()).unwrap_or("tradingview-mcp/btc_daily_full.json");
    let output_dir = Path::new(args.get(2).map(|s| s.as_str()).unwrap_or("research/out"));
    
    fs::create_dir_all(output_dir).expect("create output dir");
    
    // Load data
    let raw_json = fs::read_to_string(input_path).expect("read input");
    let raw_bars: Vec<RawBar> = serde_json::from_str(&raw_json).expect("parse JSON");
    println!("Loaded {} bars from {}", raw_bars.len(), input_path);
    
    // Build signal arrays
    let bars: Vec<Bar> = raw_bars.iter().map(Bar::from).collect();
    let n = bars.len();
    
    let mut signals_v1 = vec![false; n];
    let mut signals_v2 = vec![false; n];
    let mut signals_v3 = vec![false; n];
    let mut signals_v4 = vec![false; n];
    let mut signals_v5 = vec![false; n];
    let mut signals_v6 = vec![false; n];
    
    for i in 21..n {
        let window = &bars[..=i];
        signals_v1[i] = detect_v1(window);
        signals_v2[i] = detect_v2(window, 4);
        signals_v3[i] = detect_v3(window);
        signals_v4[i] = detect_v4(window);
        signals_v5[i] = detect_v5(window);
        signals_v6[i] = detect_v6(window);
    }
    
    // Count signals
    let counts = [
        ("V1 Volume Exhaustion", signals_v1.iter().filter(|&&s| s).count()),
        ("V2 Multi-Bar Exhaustion", signals_v2.iter().filter(|&&s| s).count()),
        ("V3 Squeeze→Expansion", signals_v3.iter().filter(|&&s| s).count()),
        ("V4 Volume Divergence", signals_v4.iter().filter(|&&s| s).count()),
        ("V5 Absorption Bar", signals_v5.iter().filter(|&&s| s).count()),
        ("V6 Vol Expansion", signals_v6.iter().filter(|&&s| s).count()),
    ];
    
    println!("\n=== Signal Counts ===");
    for (name, count) in &counts {
        println!("  {}: {} signals", name, count);
    }
    
    // Simulate trades
    let hold_bars = 10;
    let all_trades: Vec<(String, Vec<Trade>)> = vec![
        ("V1 Volume Exhaustion".into(), simulate_trades(&raw_bars, &signals_v1, hold_bars, Some(2.0), "V1")),
        ("V2 Multi-Bar Exhaustion".into(), simulate_trades(&raw_bars, &signals_v2, hold_bars, Some(2.0), "V2")),
        ("V3 Squeeze→Expansion".into(), simulate_trades(&raw_bars, &signals_v3, hold_bars, Some(2.0), "V3")),
        ("V4 Volume Divergence".into(), simulate_trades(&raw_bars, &signals_v4, hold_bars, Some(2.0), "V4")),
        ("V5 Absorption Bar".into(), simulate_trades(&raw_bars, &signals_v5, hold_bars, Some(2.0), "V5")),
        ("V6 Vol Expansion".into(), simulate_trades(&raw_bars, &signals_v6, hold_bars, Some(2.0), "V6")),
    ];
    
    // Generate report
    generate_report(&all_trades, output_dir);
    
    // Print summary to stdout
    println!("\n=== Performance Summary ===");
    for (name, trades) in &all_trades {
        let m = compute_metrics(trades, 4.0);
        println!("  {}: {} trades, {:.1}% win, {:.2}% avg, Sharpe {:.2}, MaxDD {:.1}%, PF {:.2}",
            name, m.total_trades, m.win_rate, m.avg_pnl, m.sharpe, m.max_drawdown, m.profit_factor);
    }
}
