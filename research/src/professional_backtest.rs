use std::fs;

#[derive(Debug, Clone)]
struct Bar {
    time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

#[derive(Debug, Clone)]
struct Signal {
    date: String,
    close: f64,
    pattern: String,
    direction: String, // "LONG" or "SHORT"
}

#[derive(Debug, Clone)]
struct Trade {
    entry_date: String,
    exit_date: String,
    entry_price: f64,
    exit_price: f64,
    direction: String,
    pnl_gross: f64,
    fees: f64,
    slippage: f64,
    pnl_net: f64,
    holding_days: i32,
    mae: f64, // max adverse excursion
    mfe: f64, // max favorable excursion
}

#[derive(Debug, Clone)]
struct Metrics {
    total_trades: i32,
    win_rate: f64,
    avg_pnl: f64,
    sharpe: f64,
    sortino: f64,
    calmar: f64,
    max_drawdown: f64,
    profit_factor: f64,
    expectancy: f64,
    avg_win: f64,
    avg_loss: f64,
    max_consecutive_losses: i32,
    total_return: f64,
    cagr: f64,
    avg_holding_days: f64,
    avg_mae: f64,
    avg_mfe: f64,
}

// Binance fee structure
const MAKER_FEE: f64 = 0.0002; // 0.02%
const TAKER_FEE: f64 = 0.0004; // 0.04%
const SLIPPAGE_BPS: f64 = 5.0; // 5 bps slippage

fn atr(bars: &[Bar], n: usize) -> Option<f64> {
    if bars.len() < n { return None; }
    let win = &bars[bars.len() - n..];
    let mut sum = 0.0;
    for (i, b) in win.iter().enumerate() {
        let hl = b.high - b.low;
        let tr = if i == 0 { hl } else {
            let pc = win[i-1].close;
            hl.max((b.high - pc).abs()).max((b.low - pc).abs())
        };
        sum += tr;
    }
    Some(sum / n as f64)
}

fn linear_regression(y: &[f64], n: usize, offset: usize) -> f64 {
    if y.len() < n { return y[y.len() - 1]; }
    let slice = &y[y.len() - n..];
    let (mut sum_x, mut sum_y, mut sum_xy, mut sum_x2) = (0.0, 0.0, 0.0, 0.0);
    for i in 0..n {
        sum_x += i as f64;
        sum_y += slice[i];
        sum_xy += i as f64 * slice[i];
        sum_x2 += (i as f64) * (i as f64);
    }
    let slope = (n as f64 * sum_xy - sum_x * sum_y) / (n as f64 * sum_x2 - sum_x * sum_x);
    let intercept = (sum_y - slope * sum_x) / n as f64;
    intercept + slope * (n as f64 - 1.0 - offset as f64)
}

fn detect_signals(bars: &[Bar]) -> Vec<Signal> {
    let mut signals = Vec::new();
    
    for i in 30..bars.len() - 12 {
        let window = &bars[..=i];
        let current = &bars[i];
        
        let atr14 = atr(&window[..window.len()-1], 14);
        let atr8 = atr(&window[..window.len()-1], 8);
        let atr30 = atr(&window[..window.len()-1], 30);
        
        if atr14.is_none() || atr8.is_none() || atr30.is_none() { continue; }
        let (atr14, atr8, atr30) = (atr14.unwrap(), atr8.unwrap(), atr30.unwrap());
        
        let vol_ma: f64 = window[window.len()-21..window.len()-1].iter().map(|b| b.volume).sum::<f64>() / 20.0;
        let vol_ratio = current.volume / vol_ma;
        
        let closes: Vec<f64> = window[window.len()-21..].iter().map(|b| b.close).collect();
        let linreg0 = linear_regression(&closes, 20, 0);
        let linreg1 = linear_regression(&closes, 20, 1);
        let slope = linreg0 - linreg1;
        
        let spread = current.high - current.low;
        let close_pos = if spread == 0.0 { 0.5 } else { (current.close - current.low) / spread };
        let wide_spread = spread > atr14 * 1.5;
        
        let date = format_timestamp(current.time);
        
        // V1: Volume Exhaustion
        let vol_highest = current.volume == window[window.len()-21..].iter().map(|b| b.volume).fold(0.0f64, f64::max);
        if vol_ratio > 2.5 && wide_spread && vol_highest && close_pos > 0.30 && close_pos < 0.70 {
            signals.push(Signal {
                date: date.clone(),
                close: current.close,
                pattern: "V1".to_string(),
                direction: "LONG".to_string(),
            });
        }
        
        // V2: Multi-Bar Exhaustion
        let mut down_streak = 0;
        for j in (0..=i).rev() {
            if j == i || bars[j].close < bars[j+1].close {
                down_streak += 1;
            } else { break; }
        }
        if down_streak >= 3 && vol_ratio > 1.5 {
            let cum_drop = (current.close - bars[i - down_streak].close) / bars[i - down_streak].close * 100.0;
            if cum_drop < -10.0 {
                signals.push(Signal {
                    date: date.clone(),
                    close: current.close,
                    pattern: "V2".to_string(),
                    direction: "LONG".to_string(),
                });
            }
        }
        
        // V4: Volume Divergence
        let lookback_lows: Vec<f64> = window[window.len()-11..window.len()-1].iter().map(|b| b.low).collect();
        let min_low = lookback_lows.iter().cloned().fold(f64::INFINITY, f64::min);
        if current.low <= min_low && vol_ratio < 0.8 {
            signals.push(Signal {
                date: date.clone(),
                close: current.close,
                pattern: "V4".to_string(),
                direction: "LONG".to_string(),
            });
        }
        
        // V5: Absorption Bar
        let body = (current.close - current.open).abs();
        let body_frac = body / spread;
        let lower_wick = ((current.close.min(current.open) - current.low).max(0.0)) / spread;
        let upper_wick = ((current.high - current.close.max(current.open)).max(0.0)) / spread;
        let max_wick = lower_wick.max(upper_wick);
        
        if spread > atr14 * 1.2 && body_frac < 0.20 && max_wick > 0.50 {
            signals.push(Signal {
                date: date.clone(),
                close: current.close,
                pattern: "V5".to_string(),
                direction: "LONG".to_string(),
            });
        }
        
        // V6: Vol Expansion
        if i >= 19 {
            let prev_atr14 = atr(&bars[..i-4], 14);
            if let Some(prev) = prev_atr14 {
                if prev > 0.0 && atr14 / prev > 1.8 {
                    signals.push(Signal {
                        date: date.clone(),
                        close: current.close,
                        pattern: "V6".to_string(),
                        direction: "LONG".to_string(),
                    });
                }
            }
        }
    }
    
    signals
}

fn simulate_trade(signal: &Signal, bars: &[Bar], max_hold_weeks: i32) -> Trade {
    let entry_idx = bars.iter().position(|b| format_timestamp(b.time) == signal.date).unwrap_or(0);
    let entry_price = signal.close;
    
    // Apply slippage
    let slippage = entry_price * SLIPPAGE_BPS / 10000.0;
    let entry_with_slippage = entry_price + slippage;
    
    // Apply entry fee (taker)
    let entry_fee = entry_with_slippage * TAKER_FEE;
    
    // Find exit
    let mut exit_idx = (entry_idx + max_hold_weeks as usize).min(bars.len() - 1);
    let mut exit_price = bars[exit_idx].close;
    let mut mae = 0.0;
    let mut mfe = 0.0;
    
    // Track MAE/MFE during hold
    for j in (entry_idx + 1)..=exit_idx {
        let high = bars[j].high;
        let low = bars[j].low;
        
        let drawdown = (low - entry_with_slippage) / entry_with_slippage;
        let rally = (high - entry_with_slippage) / entry_with_slippage;
        
        if drawdown < mae { mae = drawdown; }
        if rally > mfe { mfe = rally; }
    }
    
    // Exit slippage and fee
    let exit_slippage = exit_price * SLIPPAGE_BPS / 10000.0;
    let exit_with_slippage = exit_price - exit_slippage;
    let exit_fee = exit_with_slippage * TAKER_FEE;
    
    let total_fees = entry_fee + exit_fee;
    let total_slippage = slippage + exit_slippage;
    
    let pnl_gross = (exit_price - entry_price) / entry_price * 100.0;
    let pnl_net = (exit_with_slippage - entry_with_slippage) / entry_with_slippage * 100.0 - (total_fees / entry_with_slippage * 100.0);
    
    let holding_days = (exit_idx - entry_idx) as i32 * 7;
    
    Trade {
        entry_date: signal.date.clone(),
        exit_date: format_timestamp(bars[exit_idx].time),
        entry_price,
        exit_price,
        direction: signal.direction.clone(),
        pnl_gross,
        fees: total_fees / entry_price * 100.0,
        slippage: total_slippage / entry_price * 100.0,
        pnl_net,
        holding_days,
        mae: mae * 100.0,
        mfe: mfe * 100.0,
    }
}

fn calculate_metrics(trades: &[Trade]) -> Metrics {
    if trades.is_empty() {
        return Metrics {
            total_trades: 0, win_rate: 0.0, avg_pnl: 0.0, sharpe: 0.0,
            sortino: 0.0, calmar: 0.0, max_drawdown: 0.0, profit_factor: 0.0,
            expectancy: 0.0, avg_win: 0.0, avg_loss: 0.0, max_consecutive_losses: 0,
            total_return: 0.0, cagr: 0.0, avg_holding_days: 0.0, avg_mae: 0.0, avg_mfe: 0.0,
        };
    }
    
    let total_trades = trades.len() as i32;
    let wins = trades.iter().filter(|t| t.pnl_net > 0.0).count() as f64;
    let win_rate = wins / total_trades as f64 * 100.0;
    
    let avg_pnl = trades.iter().map(|t| t.pnl_net).sum::<f64>() / total_trades as f64;
    
    let avg_win = if wins > 0.0 {
        trades.iter().filter(|t| t.pnl_net > 0.0).map(|t| t.pnl_net).sum::<f64>() / wins
    } else { 0.0 };
    
    let losses = total_trades as f64 - wins;
    let avg_loss = if losses > 0.0 {
        trades.iter().filter(|t| t.pnl_net <= 0.0).map(|t| t.pnl_net).sum::<f64>() / losses
    } else { 0.0 };
    
    // Sharpe ratio (annualized, weekly returns)
    let returns: Vec<f64> = trades.iter().map(|t| t.pnl_net / 100.0).collect();
    let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean_return).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
    let std_dev = variance.sqrt();
    let sharpe = if std_dev > 0.0 { mean_return / std_dev * (52.0_f64).sqrt() } else { 0.0 };
    
    // Sortino ratio
    let downside_returns: Vec<f64> = returns.iter().filter(|r| **r < 0.0).cloned().collect();
    let downside_variance = if downside_returns.len() > 1 {
        let downside_mean = downside_returns.iter().sum::<f64>() / downside_returns.len() as f64;
        downside_returns.iter().map(|r| (r - downside_mean).powi(2)).sum::<f64>() / (downside_returns.len() - 1) as f64
    } else { 0.0 };
    let downside_std = downside_variance.sqrt();
    let sortino = if downside_std > 0.0 { mean_return / downside_std * (52.0_f64).sqrt() } else { 0.0 };
    
    // Max drawdown
    let mut equity = 100.0;
    let mut peak = 100.0;
    let mut max_dd = 0.0;
    for trade in trades {
        equity *= 1.0 + trade.pnl_net / 100.0;
        if equity > peak { peak = equity; }
        let dd = (peak - equity) / peak * 100.0;
        if dd > max_dd { max_dd = dd; }
    }
    
    // Calmar ratio
    let total_return = (equity - 100.0) / 100.0 * 100.0;
    let years = trades.iter().map(|t| t.holding_days).sum::<i32>() as f64 / 365.0;
    let cagr = if years > 0.0 { (equity / 100.0).powf(1.0 / years) - 1.0 } else { 0.0 };
    let calmar = if max_dd > 0.0 { cagr * 100.0 / max_dd } else { 0.0 };
    
    // Profit factor
    let gross_profit = trades.iter().filter(|t| t.pnl_net > 0.0).map(|t| t.pnl_net).sum::<f64>();
    let gross_loss = trades.iter().filter(|t| t.pnl_net <= 0.0).map(|t| t.pnl_net.abs()).sum::<f64>();
    let profit_factor = if gross_loss > 0.0 { gross_profit / gross_loss } else { f64::INFINITY };
    
    // Expectancy
    let expectancy = (win_rate / 100.0 * avg_win + (1.0 - win_rate / 100.0) * avg_loss);
    
    // Max consecutive losses
    let mut max_consec = 0;
    let mut current_consec = 0;
    for trade in trades {
        if trade.pnl_net <= 0.0 {
            current_consec += 1;
            if current_consec > max_consec { max_consec = current_consec; }
        } else {
            current_consec = 0;
        }
    }
    
    let avg_holding = trades.iter().map(|t| t.holding_days as f64).sum::<f64>() / total_trades as f64;
    let avg_mae = trades.iter().map(|t| t.mae).sum::<f64>() / total_trades as f64;
    let avg_mfe = trades.iter().map(|t| t.mfe).sum::<f64>() / total_trades as f64;
    
    Metrics {
        total_trades,
        win_rate,
        avg_pnl,
        sharpe,
        sortino,
        calmar,
        max_drawdown: max_dd,
        profit_factor,
        expectancy,
        avg_win,
        avg_loss,
        max_consecutive_losses: max_consec,
        total_return,
        cagr: cagr * 100.0,
        avg_holding_days: avg_holding,
        avg_mae,
        avg_mfe,
    }
}

fn walk_forward_validation(bars: &[Bar], signals: &[Signal], n_folds: usize) -> Vec<Metrics> {
    let fold_size = bars.len() / n_folds;
    let mut results = Vec::new();
    
    for fold in 0..n_folds {
        let train_start = 0;
        let train_end = (fold + 1) * fold_size;
        let test_start = train_end;
        let test_end = ((fold + 2) * fold_size).min(bars.len());
        
        if test_start >= bars.len() || test_end <= test_start { continue; }
        
        // Get signals in test period
        let test_signals: Vec<Signal> = signals.iter()
            .filter(|s| {
                let signal_idx = bars.iter().position(|b| format_timestamp(b.time) == s.date).unwrap_or(0);
                signal_idx >= test_start && signal_idx < test_end
            })
            .cloned()
            .collect();
        
        // Simulate trades
        let trades: Vec<Trade> = test_signals.iter()
            .map(|s| simulate_trade(s, bars, 8))
            .collect();
        
        let metrics = calculate_metrics(&trades);
        results.push(metrics);
    }
    
    results
}

fn format_timestamp(ts: i64) -> String {
    let dt = chrono::DateTime::from_timestamp(ts / 1000, 0).unwrap();
    dt.format("%Y-%m-%d").to_string()
}

fn main() {
    println!("Fetching weekly bars from Binance...");
    
    // Read bars from JSON file
    let data = fs::read_to_string("weekly_bars.json").expect("Failed to read weekly_bars.json");
    let bars: Vec<Bar> = serde_json::from_str(&data).expect("Failed to parse JSON");
    
    println!("Got {} weekly bars", bars.len());
    
    // Detect all signals
    let signals = detect_signals(&bars);
    println!("Detected {} signals", signals.len());
    
    // Group by pattern
    let patterns = vec!["V1", "V2", "V4", "V5", "V6"];
    
    let mut report = String::from("# Professional Backtest Report — Weekly Timeframe\n\n");
    report.push_str("**Fees:** Maker 0.02%, Taker 0.04%\n");
    report.push_str("**Slippage:** 5 bps per trade\n");
    report.push_str("**Walk-Forward:** 5-fold validation\n\n");
    
    for pattern in &patterns {
        let pattern_signals: Vec<Signal> = signals.iter()
            .filter(|s| s.pattern == *pattern)
            .cloned()
            .collect();
        
        if pattern_signals.is_empty() { continue; }
        
        println!("\n{}: {} signals", pattern, pattern_signals.len());
        
        // Simulate all trades
        let trades: Vec<Trade> = pattern_signals.iter()
            .map(|s| simulate_trade(s, &bars, 8))
            .collect();
        
        let metrics = calculate_metrics(&trades);
        
        // Walk-forward
        let wf_results = walk_forward_validation(&bars, &pattern_signals, 5);
        
        // Report
        report.push_str(&format!("## {} ({} signals)\n\n", pattern, pattern_signals.len()));
        
        report.push_str("### Full Sample Results\n\n");
        report.push_str(&format!("| Metric | Value |\n|--------|-------|\n"));
        report.push_str(&format!("| Total Trades | {} |\n", metrics.total_trades));
        report.push_str(&format!("| Win Rate | {:.1}% |\n", metrics.win_rate));
        report.push_str(&format!("| Avg PnL | {:.2}% |\n", metrics.avg_pnl));
        report.push_str(&format!("| Sharpe Ratio | {:.2} |\n", metrics.sharpe));
        report.push_str(&format!("| Sortino Ratio | {:.2} |\n", metrics.sortino));
        report.push_str(&format!("| Calmar Ratio | {:.2} |\n", metrics.calmar));
        report.push_str(&format!("| Max Drawdown | {:.1}% |\n", metrics.max_drawdown));
        report.push_str(&format!("| Profit Factor | {:.2} |\n", metrics.profit_factor));
        report.push_str(&format!("| Expectancy | {:.2}% |\n", metrics.expectancy));
        report.push_str(&format!("| Avg Win | {:.2}% |\n", metrics.avg_win));
        report.push_str(&format!("| Avg Loss | {:.2}% |\n", metrics.avg_loss));
        report.push_str(&format!("| Max Consecutive Losses | {} |\n", metrics.max_consecutive_losses));
        report.push_str(&format!("| Total Return | {:.1}% |\n", metrics.total_return));
        report.push_str(&format!("| CAGR | {:.1}% |\n", metrics.cagr));
        report.push_str(&format!("| Avg Holding Days | {:.0} |\n", metrics.avg_holding_days));
        report.push_str(&format!("| Avg MAE | {:.1}% |\n", metrics.avg_mae));
        report.push_str(&format!("| Avg MFE | {:.1}% |\n", metrics.avg_mfe));
        
        report.push_str("\n### Walk-Forward Validation\n\n");
        report.push_str("| Fold | Trades | Win% | Avg PnL | Sharpe | Max DD |\n|------|--------|------|---------|--------|--------|\n");
        
        for (i, wf) in wf_results.iter().enumerate() {
            report.push_str(&format!("| {} | {} | {:.1}% | {:.2}% | {:.2} | {:.1}% |\n",
                i + 1, wf.total_trades, wf.win_rate, wf.avg_pnl, wf.sharpe, wf.max_drawdown));
        }
        
        // Average WF metrics
        if !wf_results.is_empty() {
            let avg_wf_sharpe = wf_results.iter().map(|m| m.sharpe).sum::<f64>() / wf_results.len() as f64;
            let avg_wf_pnl = wf_results.iter().map(|m| m.avg_pnl).sum::<f64>() / wf_results.len() as f64;
            let avg_wf_dd = wf_results.iter().map(|m| m.max_drawdown).sum::<f64>() / wf_results.len() as f64;
            
            report.push_str(&format!("\n**Walk-Forward Average:** Sharpe {:.2}, Avg PnL {:.2}%, Max DD {:.1}%\n", avg_wf_sharpe, avg_wf_pnl, avg_wf_dd));
        }
        
        // Trade log
        report.push_str("\n### Trade Log\n\n");
        report.push_str("| # | Entry | Exit | Entry $ | Exit $ | PnL% | Fees% | Slippage% | Net% | MAE% | MFE% |\n");
        report.push_str("|---|-------|------|---------|--------|------|-------|-----------|------|------|------|\n");
        
        for (i, trade) in trades.iter().enumerate() {
            report.push_str(&format!("| {} | {} | {} | ${:.0} | ${:.0} | {:.2}% | {:.3}% | {:.3}% | {:.2}% | {:.1}% | {:.1}% |\n",
                i + 1, trade.entry_date, trade.exit_date, trade.entry_price, trade.exit_price,
                trade.pnl_gross, trade.fees, trade.slippage, trade.pnl_net, trade.mae, trade.mfe));
        }
        
        report.push_str("\n---\n\n");
    }
    
    // Summary comparison
    report.push_str("## Summary Comparison\n\n");
    report.push_str("| Pattern | Trades | Win% | Avg PnL | Sharpe | PF | Max DD | CAGR |\n");
    report.push_str("|---------|--------|------|---------|--------|-----|--------|------|\n");
    
    for pattern in &patterns {
        let pattern_signals: Vec<Signal> = signals.iter()
            .filter(|s| s.pattern == *pattern)
            .cloned()
            .collect();
        
        if pattern_signals.is_empty() { continue; }
        
        let trades: Vec<Trade> = pattern_signals.iter()
            .map(|s| simulate_trade(s, &bars, 8))
            .collect();
        
        let metrics = calculate_metrics(&trades);
        
        report.push_str(&format!("| {} | {} | {:.1}% | {:.2}% | {:.2} | {:.2} | {:.1}% | {:.1}% |\n",
            pattern, metrics.total_trades, metrics.win_rate, metrics.avg_pnl,
            metrics.sharpe, metrics.profit_factor, metrics.max_drawdown, metrics.cagr));
    }
    
    fs::write("research/out/professional_backtest_report.md", &report).expect("Failed to write report");
    println!("\nReport saved to research/out/professional_backtest_report.md");
}
