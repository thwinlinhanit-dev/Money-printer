//! Comprehensive parameter sweep for climax variant patterns.
//! Tests every combination of: hold periods, stop types, entry/exit rules,
//! pattern combinations, regime filters, cooldowns, directional signals,
//! position sizing, and more. Ranks all results by Sharpe ratio.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;

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
}

impl From<&RawBar> for Bar {
    fn from(r: &RawBar) -> Self {
        Bar { open: r.open, high: r.high, low: r.low, close: r.close, vol: r.volume }
    }
}

// ── Indicators ──────────────────────────────────────────────

fn atr(bars: &[Bar], n: usize) -> Option<f64> {
    if n == 0 || bars.len() < n { return None; }
    let win = &bars[bars.len() - n..];
    let mut sum = 0.0_f64;
    for (i, b) in win.iter().enumerate() {
        let hl = b.high - b.low;
        let tr = if i == 0 { hl } else {
            let pc = win[i-1].close;
            hl.max((b.high - pc).abs()).max((b.low - pc).abs())
        };
        sum += tr;
    }
    let a = sum / n as f64;
    if a.is_finite() && a > 0.0 { Some(a) } else { None }
}

fn sma(vals: &[f64], n: usize) -> Option<f64> {
    if vals.len() < n || n == 0 { return None; }
    let s: f64 = vals[vals.len()-n..].iter().sum();
    let a = s / n as f64;
    if a.is_finite() && a > 0.0 { Some(a) } else { None }
}

fn efficiency_ratio(closes: &[f64], n: usize) -> Option<f64> {
    if closes.len() < n + 1 { return None; }
    let net = (closes[closes.len()-1] - closes[closes.len()-n-1]).abs();
    let mut path = 0.0;
    for w in closes[closes.len()-n-1..].windows(2) {
        path += (w[1] - w[0]).abs();
    }
    if path <= 0.0 { Some(1.0) } else { Some(net / path) }
}

// ── Pattern detectors ───────────────────────────────────────

fn detect_v1(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 21 { return false; }
    let c = &bars[n-1];
    if !c.close.is_finite() || !c.vol.is_finite() || c.vol <= 0.0 { return false; }
    let win = &bars[n-21..n-1];
    let max_vol = win.iter().map(|b| b.vol).fold(0.0_f64, f64::max);
    if c.vol < max_vol { return false; }
    let vol_sma = win.iter().map(|b| b.vol).sum::<f64>() / 20.0;
    if vol_sma <= 0.0 || c.vol <= vol_sma * 2.5 { return false; }
    let spread = c.high - c.low;
    if let Some(a) = atr(&bars[..n-1], 20) {
        if spread <= a * 1.5 { return false; }
    }
    let cp = if spread > 0.0 { (c.close - c.low) / spread } else { 0.5 };
    cp >= 0.30 && cp <= 0.70
}

fn detect_v1_buy(bars: &[Bar]) -> bool {
    let n = bars.len();
    if !detect_v1(bars) { return false; }
    // Selling climax: downtrend + close in upper portion → buy
    let slope = bars[n-1].close - bars[n-21].close;
    slope < 0.0
}

fn detect_v1_sell(bars: &[Bar]) -> bool {
    let n = bars.len();
    if !detect_v1(bars) { return false; }
    let slope = bars[n-1].close - bars[n-21].close;
    slope > 0.0
}

fn detect_v2(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 25 { return false; }
    // 4+ consecutive down bars
    let mut streak = 0usize;
    for i in (1..n).rev() {
        if bars[i].close < bars[i-1].close { streak += 1; } else { break; }
    }
    if streak < 4 { return false; }
    // Volume spike on current bar
    let vol_sma = bars[n-22..n-1].iter().map(|b| b.vol).sum::<f64>() / 20.0;
    if vol_sma <= 0.0 || bars[n-1].vol <= vol_sma * 1.5 { return false; }
    // Cumulative drop > 8%
    let start = bars[n - 1 - streak].close;
    if start <= 0.0 { return false; }
    (bars[n-1].close - start) / start * 100.0 <= -8.0
}

fn detect_v3(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 26 { return false; }
    let sa = atr(bars, 5);
    let la = atr(bars, 20);
    let ps = atr(&bars[..n-1], 5);
    let pl = atr(&bars[..n-1], 20);
    if let (Some(sa), Some(la), Some(ps), Some(pl)) = (sa, la, ps, pl) {
        if pl <= 0.0 || la <= 0.0 { return false; }
        ps / pl < 0.60 && sa / la > 0.60 && sa > ps * 1.5
    } else { false }
}

fn detect_v4(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 16 { return false; }
    let c = &bars[n-1];
    if !c.vol.is_finite() || c.vol <= 0.0 { return false; }
    let min_low = bars[n-11..n-1].iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    if c.low > min_low { return false; }
    let long_sma = bars[n-10..].iter().map(|b| b.vol).sum::<f64>() / 10.0;
    let short_sma = bars[n-3..].iter().map(|b| b.vol).sum::<f64>() / 3.0;
    if long_sma <= 0.0 { return false; }
    short_sma < long_sma * 0.80
}

fn detect_v5(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 15 { return false; }
    let c = &bars[n-1];
    if !c.close.is_finite() { return false; }
    let spread = c.high - c.low;
    if spread <= 0.0 { return false; }
    if let Some(a) = atr(&bars[..n-1], 14) {
        if spread <= a * 1.2 { return false; }
    }
    let body = (c.close - c.open).abs();
    if body / spread >= 0.20 { return false; }
    let lw = (c.close.min(c.open) - c.low).max(0.0) / spread;
    let uw = (c.high - c.close.max(c.open)).max(0.0) / spread;
    lw.max(uw) >= 0.50
}

fn detect_v6(bars: &[Bar]) -> bool {
    let n = bars.len();
    if n < 20 { return false; }
    let ca = atr(bars, 14);
    let pa = atr(&bars[..n-5], 14);
    match (ca, pa) { (Some(ca), Some(pa)) if pa > 0.0 => ca / pa > 1.8, _ => false }
}

// ── Trade simulation engine ─────────────────────────────────

#[derive(Debug, Clone)]
struct Config {
    hold_bars: usize,
    stop_type: StopType,
    entry_rule: EntryRule,
    trend_filter: TrendFilter,
    vol_filter: VolFilter,
    cooldown: usize,
    take_profit: Option<f64>,
    pattern_combo: PatternCombo,
    directional: Direction,
    sizing: Sizing,
}

#[derive(Debug, Clone)]
enum StopType { None, Fixed(f64), Atr(f64), TrailingAtr(f64) }
#[derive(Debug, Clone)]
enum EntryRule { SignalBarClose, NextOpen, NextClose }
#[derive(Debug, Clone)]
enum TrendFilter { None, Uptrend, Downtrend, Any }
#[derive(Debug, Clone)]
enum VolFilter { None, AboveAvg, BelowAvg }
#[derive(Debug, Clone)]
enum PatternCombo { V1, V2, V3, V4, V5, V6, V1V5, V1V4, V4V5, V1V5V4, Any2, Any3, All }
#[derive(Debug, Clone)]
enum Direction { Both, BuyOnly, SellOnly }
#[derive(Debug, Clone)]
enum Sizing { Fixed, VolTarget }

#[derive(Debug, Clone)]
struct Trade {
    entry_price: f64,
    exit_price: f64,
    pnl_pct: f64,
    bars_held: usize,
}

fn signal(bars: &[Bar], combo: &PatternCombo, dir: &Direction) -> bool {
    let v1 = detect_v1(bars);
    let v2 = detect_v2(bars);
    let v3 = detect_v3(bars);
    let v4 = detect_v4(bars);
    let v5 = detect_v5(bars);
    let v6 = detect_v6(bars);
    
    let matched = match combo {
        PatternCombo::V1 => v1,
        PatternCombo::V2 => v2,
        PatternCombo::V3 => v3,
        PatternCombo::V4 => v4,
        PatternCombo::V5 => v5,
        PatternCombo::V6 => v6,
        PatternCombo::V1V5 => v1 && v5,
        PatternCombo::V1V4 => v1 && v4,
        PatternCombo::V4V5 => v4 && v5,
        PatternCombo::V1V5V4 => v1 && v5 && v4,
        PatternCombo::Any2 => [v1,v2,v3,v4,v5,v6].iter().filter(|&&x| x).count() >= 2,
        PatternCombo::Any3 => [v1,v2,v3,v4,v5,v6].iter().filter(|&&x| x).count() >= 3,
        PatternCombo::All => v1 && v2 && v3 && v4 && v5 && v6,
    };
    
    if !matched { return false; }
    
    match dir {
        Direction::Both => true,
        Direction::BuyOnly => detect_v1_buy(bars) || detect_v4(bars) || detect_v5(bars),
        Direction::SellOnly => detect_v1_sell(bars),
    }
}

fn trend_ok(bars: &[Bar], filter: &TrendFilter) -> bool {
    match filter {
        TrendFilter::None => true,
        TrendFilter::Any => true,
        TrendFilter::Uptrend => {
            efficiency_ratio(&bars.iter().map(|b| b.close).collect::<Vec<_>>(), 20)
                .map(|er| er > 0.3 && bars[bars.len()-1].close > bars[bars.len()-21].close)
                .unwrap_or(false)
        }
        TrendFilter::Downtrend => {
            efficiency_ratio(&bars.iter().map(|b| b.close).collect::<Vec<_>>(), 20)
                .map(|er| er > 0.3 && bars[bars.len()-1].close < bars[bars.len()-21].close)
                .unwrap_or(false)
        }
    }
}

fn vol_ok(bars: &[Bar], filter: &VolFilter) -> bool {
    match filter {
        VolFilter::None => true,
        VolFilter::AboveAvg => {
            let vols: Vec<f64> = bars.iter().map(|b| b.vol).collect();
            let cur = vols[vols.len()-1];
            sma(&vols, 20).map(|s| cur > s).unwrap_or(false)
        }
        VolFilter::BelowAvg => {
            let vols: Vec<f64> = bars.iter().map(|b| b.vol).collect();
            let cur = vols[vols.len()-1];
            sma(&vols, 20).map(|s| cur < s).unwrap_or(false)
        }
    }
}

fn simulate(raw_bars: &[RawBar], cfg: &Config) -> Vec<Trade> {
    let bars: Vec<Bar> = raw_bars.iter().map(Bar::from).collect();
    let n = bars.len();
    let mut trades = Vec::new();
    let mut last_exit = 0usize;
    let mut i = 30usize; // warmup
    
    while i < n - 1 {
        // Cooldown check
        if i < last_exit + cfg.cooldown { i += 1; continue; }
        
        // Signal check
        if !signal(&bars[..=i], &cfg.pattern_combo, &cfg.directional) { i += 1; continue; }
        
        // Trend filter
        if !trend_ok(&bars[..=i], &cfg.trend_filter) { i += 1; continue; }
        
        // Vol filter
        if !vol_ok(&bars[..=i], &cfg.vol_filter) { i += 1; continue; }
        
        // Entry
        let entry_idx = match cfg.entry_rule {
            EntryRule::SignalBarClose => i,
            EntryRule::NextOpen => i + 1,
            EntryRule::NextClose => i + 1,
        };
        if entry_idx >= n { break; }
        
        let entry_price = match cfg.entry_rule {
            EntryRule::NextClose => bars[entry_idx].close,
            _ => bars[entry_idx].open,
        };
        
        // Exit simulation
        let max_hold = (entry_idx + cfg.hold_bars).min(n - 1);
        let mut exit_idx = max_hold;
        let mut peak = entry_price;
        let mut trough = entry_price;
        let mut stopped = false;
        let mut tp_hit = false;
        
        for j in entry_idx..=max_hold {
            let b = &bars[j];
            
            match &cfg.stop_type {
                StopType::None => {}
                StopType::Fixed(pct) => {
                    let stop = entry_price * (1.0 - pct / 100.0);
                    if b.low <= stop { exit_idx = j; stopped = true; break; }
                    let tp = entry_price * (1.0 + pct / 100.0);
                    if b.high >= tp && cfg.take_profit.is_none() { exit_idx = j; tp_hit = true; break; }
                }
                StopType::Atr(mult) => {
                    if let Some(a) = atr(&bars[..=j], 14) {
                        let stop = entry_price - mult * a;
                        if b.low <= stop { exit_idx = j; stopped = true; break; }
                    }
                }
                StopType::TrailingAtr(mult) => {
                    if b.high > peak { peak = b.high; }
                    if let Some(a) = atr(&bars[..=j], 14) {
                        let stop = peak - mult * a;
                        if b.low <= stop { exit_idx = j; stopped = true; break; }
                    }
                }
            }
            
            // Take profit
            if let Some(tp_pct) = cfg.take_profit {
                if b.high >= entry_price * (1.0 + tp_pct / 100.0) {
                    exit_idx = j;
                    tp_hit = true;
                    break;
                }
            }
        }
        
        let exit_price = bars[exit_idx].close;
        let pnl = (exit_price - entry_price) / entry_price * 100.0;
        
        trades.push(Trade {
            entry_price,
            exit_price,
            pnl_pct: pnl,
            bars_held: exit_idx - entry_idx,
        });
        
        last_exit = exit_idx;
        i = exit_idx + 1;
    }
    trades
}

// ── Metrics ─────────────────────────────────────────────────

#[derive(Clone)]
struct Metrics {
    trades: usize,
    win_rate: f64,
    avg_pnl: f64,
    sharpe: f64,
    sortino: f64,
    max_dd: f64,
    profit_factor: f64,
    expectancy: f64,
    cagr: f64,
    total_return: f64,
    avg_bars: f64,
}

fn metrics(trades: &[Trade]) -> Metrics {
    if trades.is_empty() {
        return Metrics { trades:0, win_rate:0., avg_pnl:0., sharpe:0., sortino:0.,
            max_dd:0., profit_factor:0., expectancy:0., cagr:0., total_return:0., avg_bars:0. };
    }
    let n = trades.len() as f64;
    let pnls: Vec<f64> = trades.iter().map(|t| t.pnl_pct).collect();
    let wins: Vec<f64> = pnls.iter().filter(|&&p| p > 0.0).copied().collect();
    let losses: Vec<f64> = pnls.iter().filter(|&&p| p <= 0.0).copied().collect();
    
    let win_rate = wins.len() as f64 / n * 100.0;
    let avg_pnl = pnls.iter().sum::<f64>() / n;
    let variance = pnls.iter().map(|p| (p - avg_pnl).powi(2)).sum::<f64>() / n;
    let std_dev = variance.sqrt();
    
    let trades_per_year = (n / (trades.iter().map(|t| t.bars_held as f64).sum::<f64>() / 365.0)).max(1.0);
    let excess = avg_pnl * trades_per_year - 4.0;
    let sharpe = if std_dev > 0.0 { excess / (std_dev * trades_per_year.sqrt()) } else { 0.0 };
    
    let down_sq: f64 = pnls.iter().map(|p| p.min(0.0).powi(2)).sum::<f64>() / n;
    let down_dev = down_sq.sqrt();
    let sortino = if down_dev > 0.0 { excess / (down_dev * trades_per_year.sqrt()) } else { 0.0 };
    
    let mut eq = 100.0_f64;
    let mut peak = 100.0_f64;
    let mut max_dd = 0.0_f64;
    for t in trades {
        eq *= 1.0 + t.pnl_pct / 100.0;
        if eq > peak { peak = eq; }
        let dd = (peak - eq) / peak * 100.0;
        if dd > max_dd { max_dd = dd; }
    }
    
    let gp: f64 = wins.iter().sum();
    let gl: f64 = losses.iter().map(|l| l.abs()).sum();
    let pf = if gl > 0.0 { gp / gl } else if gp > 0.0 { f64::INFINITY } else { 0.0 };
    
    let avg_win = if !wins.is_empty() { wins.iter().sum::<f64>() / wins.len() as f64 } else { 0.0 };
    let avg_loss = if !losses.is_empty() { losses.iter().sum::<f64>() / losses.len() as f64 } else { 0.0 };
    let expectancy = (win_rate / 100.0 * avg_win) + ((100.0 - win_rate) / 100.0 * avg_loss);
    
    let total_bars: f64 = trades.iter().map(|t| t.bars_held as f64).sum();
    let years = total_bars as f64 / 365.0;
    let cagr = if years > 0.0 { ((eq / 100.0).powf(1.0 / years) - 1.0) * 100.0 } else { 0.0 };
    let total_return = (eq / 100.0 - 1.0) * 100.0;
    let avg_bars = trades.iter().map(|t| t.bars_held as f64).sum::<f64>() / n;
    
    Metrics { trades: trades.len(), win_rate, avg_pnl, sharpe, sortino, max_dd,
        profit_factor: pf, expectancy, cagr, total_return, avg_bars }
}

// ── Main sweep ──────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input_path = args.get(1).map(|s| s.as_str()).unwrap_or("tradingview-mcp/btc_daily_full.json");
    let output_dir = args.get(2).map(|s| s.as_str()).unwrap_or("research/out");
    
    fs::create_dir_all(output_dir).expect("create dir");
    
    let raw_json = fs::read_to_string(input_path).expect("read");
    let raw_bars: Vec<RawBar> = serde_json::from_str(&raw_json).expect("parse");
    println!("Loaded {} bars", raw_bars.len());
    
    let mut results: Vec<(String, Config, Metrics)> = Vec::new();
    
    // ── Sweep dimensions ──────────────────────────────────
    // Phase 1: core sweep — hold × stop × pattern (manageable size)
    let hold_periods = [3, 5, 7, 10, 15, 20];
    let stop_types = [
        ("none", StopType::None),
        ("fixed_5pct", StopType::Fixed(5.0)),
        ("atr_1.5", StopType::Atr(1.5)),
        ("atr_2.0", StopType::Atr(2.0)),
        ("trail_2.0", StopType::TrailingAtr(2.0)),
        ("trail_3.0", StopType::TrailingAtr(3.0)),
    ];
    let take_profits: [Option<f64>; 3] = [None, Some(8.0), Some(15.0)];
    let cooldowns = [0, 5];
    let combo_names = [
        ("V1", PatternCombo::V1),
        ("V4", PatternCombo::V4),
        ("V5", PatternCombo::V5),
        ("V6", PatternCombo::V6),
        ("V1V5", PatternCombo::V1V5),
        ("V1V4", PatternCombo::V1V4),
        ("V4V5", PatternCombo::V4V5),
        ("Any2", PatternCombo::Any2),
    ];
    let directions = [
        ("both", Direction::Both),
        ("buy", Direction::BuyOnly),
    ];
    let trend_filters = [
        ("none", TrendFilter::None),
        ("uptrend", TrendFilter::Uptrend),
    ];
    let vol_filters = [
        ("none", VolFilter::None),
        ("above", VolFilter::AboveAvg),
    ];
    
    let mut count = 0usize;
    let total = hold_periods.len() * stop_types.len() * take_profits.len() * cooldowns.len()
        * combo_names.len() * directions.len() * trend_filters.len() * vol_filters.len();
    
    for &hold in &hold_periods {
        for (sn, st) in &stop_types {
            for tp in &take_profits {
                for &cd in &cooldowns {
                    for (cn, cc) in &combo_names {
                        for (dn, dd) in &directions {
                            for (tn, tf) in &trend_filters {
                                for (vn, vf) in &vol_filters {
                                    let cfg = Config {
                                        hold_bars: hold,
                                        stop_type: st.clone(),
                                        entry_rule: EntryRule::NextOpen,
                                        trend_filter: tf.clone(),
                                        vol_filter: vf.clone(),
                                        cooldown: cd,
                                        take_profit: *tp,
                                        pattern_combo: cc.clone(),
                                        directional: dd.clone(),
                                        sizing: Sizing::Fixed,
                                    };
                                    
                                    let trades = simulate(&raw_bars, &cfg);
                                    let m = metrics(&trades);
                                    
                                    if m.trades >= 5 { // minimum 5 trades for significance
                                        let name = format!("{}_{}_hold{}_cd{}_{}_{}_{}",
                                            cn, dn, hold, cd, sn,
                                            if tp.is_some() { format!("tp{}", tp.unwrap()) } else { "notp".into() },
                                            tn);
                                        results.push((name, cfg, m));
                                    }
                                    
                                    count += 1;
                                    if count % 5000 == 0 {
                                        eprint!("\r  Swept {}/{} ({:.0}%)", count, total, count as f64 / total as f64 * 100.0);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("\r  Swept {} combinations, {} with ≥5 trades      ", count, results.len());
    
    // Sort by Sharpe ratio (descending)
    results.sort_by(|a, b| b.2.sharpe.partial_cmp(&a.2.sharpe).unwrap());
    
    // ── Generate report ───────────────────────────────────
    let mut report = String::new();
    report.push_str("# Climax Variant Patterns — Comprehensive Parameter Sweep\n\n");
    report.push_str(&format!("**Total combinations tested:** {}  \n", count));
    report.push_str(&format!("**Results with ≥5 trades:** {}  \n", results.len()));
    report.push_str("**Sorted by:** Sharpe ratio (descending)  \n\n");
    report.push_str("---\n\n");
    
    // Top 30
    report.push_str("## Top 30 Configurations by Sharpe\n\n");
    report.push_str("| # | Config | Trades | Win% | Avg PnL | Sharpe | Sortino | Max DD | PF | CAGR | Return |\n");
    report.push_str("|---|--------|--------|------|---------|--------|---------|--------|-----|------|--------|\n");
    for (i, (name, _, m)) in results.iter().take(30).enumerate() {
        report.push_str(&format!(
            "| {} | {} | {} | {:.1}% | {:.2}% | {:.2} | {:.2} | {:.1}% | {:.2} | {:.1}% | {:.1}% |\n",
            i+1, name, m.trades, m.win_rate, m.avg_pnl, m.sharpe, m.sortino,
            m.max_dd, m.profit_factor, m.cagr, m.total_return
        ));
    }
    
    // Top 10 by Sortino
    let mut by_sortino = results.clone();
    by_sortino.sort_by(|a, b| b.2.sortino.partial_cmp(&a.2.sortino).unwrap());
    report.push_str("\n## Top 10 by Sortino Ratio\n\n");
    report.push_str("| # | Config | Trades | Win% | Sharpe | Sortino | Max DD | PF |\n");
    report.push_str("|---|--------|--------|------|--------|---------|--------|-----|\n");
    for (i, (name, _, m)) in by_sortino.iter().take(10).enumerate() {
        report.push_str(&format!(
            "| {} | {} | {} | {:.1}% | {:.2} | {:.2} | {:.1}% | {:.2} |\n",
            i+1, name, m.trades, m.win_rate, m.sharpe, m.sortino, m.max_dd, m.profit_factor
        ));
    }
    
    // Top 10 by Profit Factor
    let mut by_pf = results.clone();
    by_pf.sort_by(|a, b| b.2.profit_factor.partial_cmp(&a.2.profit_factor).unwrap());
    report.push_str("\n## Top 10 by Profit Factor\n\n");
    report.push_str("| # | Config | Trades | Win% | Sharpe | PF | CAGR | Return |\n");
    report.push_str("|---|--------|--------|------|--------|-----|------|--------|\n");
    for (i, (name, _, m)) in by_pf.iter().take(10).enumerate() {
        report.push_str(&format!(
            "| {} | {} | {} | {:.1}% | {:.2} | {:.2} | {:.1}% | {:.1}% |\n",
            i+1, name, m.trades, m.win_rate, m.sharpe, m.profit_factor, m.cagr, m.total_return
        ));
    }
    
    // Top 10 by CAGR (min 10 trades)
    let mut by_cagr: Vec<_> = results.iter().filter(|(_, _, m)| m.trades >= 10).cloned().collect();
    by_cagr.sort_by(|a, b| b.2.cagr.partial_cmp(&a.2.cagr).unwrap());
    report.push_str("\n## Top 10 by CAGR (min 10 trades)\n\n");
    report.push_str("| # | Config | Trades | Win% | Sharpe | Max DD | CAGR | Return |\n");
    report.push_str("|---|--------|--------|------|--------|--------|------|--------|\n");
    for (i, (name, _, m)) in by_cagr.iter().take(10).enumerate() {
        report.push_str(&format!(
            "| {} | {} | {} | {:.1}% | {:.2} | {:.1}% | {:.1}% | {:.1}% |\n",
            i+1, name, m.trades, m.win_rate, m.sharpe, m.max_dd, m.cagr, m.total_return
        ));
    }
    
    // Best per pattern
    report.push_str("\n## Best Configuration Per Pattern\n\n");
    report.push_str("| Pattern | Best Config | Trades | Win% | Sharpe | PF | Return |\n");
    report.push_str("|---------|-------------|--------|------|--------|-----|--------|\n");
    for pname in &["V1", "V2", "V3", "V4", "V5", "V6", "V1V5", "V1V4", "V4V5"] {
        let best = results.iter()
            .filter(|(n, _, _)| n.starts_with(&format!("{}_", pname)))
            .max_by(|a, b| a.2.sharpe.partial_cmp(&b.2.sharpe).unwrap());
        if let Some((name, _, m)) = best {
            report.push_str(&format!(
                "| {} | {} | {} | {:.1}% | {:.2} | {:.2} | {:.1}% |\n",
                pname, name, m.trades, m.win_rate, m.sharpe, m.profit_factor, m.total_return
            ));
        }
    }
    
    // Worst configs (to know what to avoid)
    report.push_str("\n## Bottom 10 (Avoid These)\n\n");
    report.push_str("| Config | Trades | Win% | Sharpe | Max DD | Return |\n");
    report.push_str("|--------|--------|------|--------|--------|--------|\n");
    for (name, _, m) in results.iter().rev().take(10) {
        report.push_str(&format!(
            "| {} | {} | {:.1}% | {:.2} | {:.1}% | {:.1}% |\n",
            name, m.trades, m.win_rate, m.sharpe, m.max_dd, m.total_return
        ));
    }
    
    // Pattern frequency analysis
    report.push_str("\n## Pattern Frequency Analysis\n\n");
    report.push_str("| Pattern | Total Signals | Best Combo Hit Rate |\n");
    report.push_str("|---------|---------------|--------------------|\n");
    let bars_data: Vec<Bar> = raw_bars.iter().map(Bar::from).collect();
    let mut v_counts = [0usize; 6];
    for i in 30..bars_data.len() {
        let w = &bars_data[..=i];
        if detect_v1(w) { v_counts[0] += 1; }
        if detect_v2(w) { v_counts[1] += 1; }
        if detect_v3(w) { v_counts[2] += 1; }
        if detect_v4(w) { v_counts[3] += 1; }
        if detect_v5(w) { v_counts[4] += 1; }
        if detect_v6(w) { v_counts[5] += 1; }
    }
    for (name, count) in ["V1", "V2", "V3", "V4", "V5", "V6"].iter().zip(v_counts.iter()) {
        report.push_str(&format!("| {} | {} | — |\n", name, count));
    }
    
    fs::write(format!("{}/sweep_report.md", output_dir), &report).expect("write report");
    
    // Also write top configs as JSON for downstream use
    let top_json: Vec<serde_json::Value> = results.iter().take(50).map(|(name, cfg, m)| {
        serde_json::json!({
            "name": name,
            "trades": m.trades,
            "win_rate": m.win_rate,
            "avg_pnl": m.avg_pnl,
            "sharpe": m.sharpe,
            "sortino": m.sortino,
            "max_dd": m.max_dd,
            "profit_factor": m.profit_factor,
            "cagr": m.cagr,
            "total_return": m.total_return,
        })
    }).collect();
    fs::write(
        format!("{}/sweep_top50.json", output_dir),
        serde_json::to_string_pretty(&top_json).unwrap()
    ).expect("write json");
    
    println!("\n=== Sweep Complete ===");
    println!("Report: {}/sweep_report.md", output_dir);
    println!("Top 50: {}/sweep_top50.json", output_dir);
    println!("\nTop 5 by Sharpe:");
    for (i, (name, _, m)) in results.iter().take(5).enumerate() {
        println!("  {}. {} — Sharpe {:.2}, PF {:.2}, {} trades, {:.1}% return",
            i+1, name, m.sharpe, m.profit_factor, m.trades, m.total_return);
    }
}
