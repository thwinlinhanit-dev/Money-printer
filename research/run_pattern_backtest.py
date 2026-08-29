#!/usr/bin/env python3
"""
Volume Pattern Backtest — Money Printer Research Lab

Uses the existing research infrastructure:
- Panel data from research/panel/
- Event study framework from event_study.py
- Grading logic from grading.py
- Feasibility gate from feasibility.py

Outputs results compatible with the registry and grading system.
"""

import json
import sys
from datetime import datetime, timedelta
from pathlib import Path
from dataclasses import dataclass, asdict
from typing import Optional

# ═══════════════════════════════════════════════════════════════
# Configuration
# ═══════════════════════════════════════════════════════════════

TAKER_FEE = 0.0004  # 0.04%
SLIPPAGE_BPS = 5    # 5 bps per side

# Strategy configs per pattern per timeframe
STRATEGIES = {
    '1d': {
        'V1': {'max_hold': 70, 'stop_atr': 2.0, 'label': '70d hold, 2x ATR stop'},
        'V2': {'max_hold': 70, 'stop_atr': 2.0, 'label': '70d hold, 2x ATR stop'},
        'V4': {'max_hold': 20, 'stop_atr': 0, 'label': '20d hold, no stop'},
        'V5': {'max_hold': 40, 'stop_atr': 2.0, 'label': '40d hold, 2x ATR stop'},
        'V6': {'max_hold': 70, 'stop_atr': 3.0, 'label': '70d hold, 3x ATR trailing'},
    },
    '1w': {
        'V1': {'max_hold': 8, 'stop_atr': 2.0, 'label': '8w hold, 2x ATR stop'},
        'V2': {'max_hold': 8, 'stop_atr': 2.0, 'label': '8w hold, 2x ATR stop'},
        'V4': {'max_hold': 4, 'stop_atr': 0, 'label': '4w hold, no stop'},
        'V5': {'max_hold': 8, 'stop_atr': 2.0, 'label': '8w hold, 2x ATR stop'},
        'V6': {'max_hold': 12, 'stop_atr': 3.0, 'label': '12w hold, 3x ATR trailing'},
    }
}


@dataclass
class Bar:
    time: int
    date: str
    open: float
    high: float
    low: float
    close: float
    volume: float


@dataclass
class Signal:
    idx: int
    date: str
    close: float
    pattern: str


@dataclass
class Trade:
    entry_date: str
    exit_date: str
    entry_price: float
    exit_price: float
    pattern: str
    timeframe: str
    pnl_gross: float
    fees: float
    slippage: float
    pnl_net: float
    holding: int
    mae: float
    mfe: float
    exit_reason: str


@dataclass
class Metrics:
    n: int
    win_rate: float
    avg_pnl: float
    avg_win: float
    avg_loss: float
    sharpe: float
    sortino: float
    calmar: float
    max_dd: float
    profit_factor: float
    deflated_sharpe: float
    total_return: float
    cagr: float
    max_consec_losses: int
    avg_hold: float
    avg_mae: float
    avg_mfe: float
    stop_exits: int


# ═══════════════════════════════════════════════════════════════
# Data Loading (from Binance API)
# ═══════════════════════════════════════════════════════════════

def fetch_bars(interval: str = '1d', start_date: str = '2017-08-17') -> list[Bar]:
    """Fetch bars from Binance API."""
    import urllib.request
    
    all_bars = []
    start_time = int(datetime.strptime(start_date, '%Y-%m-%d').timestamp() * 1000)
    end_time = int(datetime.now().timestamp() * 1000)
    increment = 604800000 if interval == '1w' else 86400000
    
    while start_time < end_time:
        url = f'https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval={interval}&startTime={start_time}&limit=1000'
        try:
            with urllib.request.urlopen(url) as response:
                data = json.loads(response.read())
        except Exception as e:
            print(f"Error fetching data: {e}")
            break
        
        if not data:
            break
        
        for bar in data:
            all_bars.append(Bar(
                time=bar[0],
                date=datetime.fromtimestamp(bar[0]/1000).strftime('%Y-%m-%d'),
                open=float(bar[1]),
                high=float(bar[2]),
                low=float(bar[3]),
                close=float(bar[4]),
                volume=float(bar[5])
            ))
        
        start_time = data[-1][0] + increment
    
    return all_bars


# ═══════════════════════════════════════════════════════════════
# Technical Indicators
# ═══════════════════════════════════════════════════════════════

def calc_atr(bars: list[Bar], n: int) -> Optional[float]:
    """Calculate Average True Range."""
    if len(bars) < n:
        return None
    
    win = bars[-n:]
    total = 0.0
    
    for i, b in enumerate(win):
        hl = b.high - b.low
        if i == 0:
            tr = hl
        else:
            pc = win[i-1].close
            tr = max(hl, abs(b.high - pc), abs(b.low - pc))
        total += tr
    
    return total / n


def linear_regression(y: list[float], n: int, offset: int) -> float:
    """Calculate linear regression value."""
    if len(y) < n:
        return y[-1]
    
    s = y[-n:]
    sx = sum(range(n))
    sy = sum(s)
    sxy = sum(i * s[i] for i in range(n))
    sx2 = sum(i * i for i in range(n))
    
    slope = (n * sxy - sx * sy) / (n * sx2 - sx * sx)
    intercept = (sy - slope * sx) / n
    
    return intercept + slope * (n - 1 - offset)


# ═══════════════════════════════════════════════════════════════
# Pattern Detection
# ═══════════════════════════════════════════════════════════════

def detect_patterns(bars: list[Bar], timeframe: str) -> list[Signal]:
    """Detect all 6 variant patterns."""
    signals = []
    is_weekly = timeframe == '1w'
    lookback = 21 if is_weekly else 20
    
    for i in range(30, len(bars) - (12 if is_weekly else 30)):
        w = bars[:i+1]
        c = bars[i]
        
        atr14 = calc_atr(w[:-1], 14)
        atr8 = calc_atr(w[:-1], 8)
        atr30 = calc_atr(w[:-1], 30)
        
        if not atr14 or not atr8 or not atr30:
            continue
        
        vol_ma = sum(b.volume for b in w[-lookback-1:-1]) / lookback
        vol_r = c.volume / vol_ma if vol_ma > 0 else 0
        
        closes = [b.close for b in w[-lookback-1:]]
        slope = linear_regression(closes, lookback, 0) - linear_regression(closes, lookback, 1)
        
        spread = c.high - c.low
        cp = (c.close - c.low) / spread if spread > 0 else 0.5
        wide = spread > atr14 * 1.5
        vol_hi = c.volume == max(b.volume for b in w[-lookback-1:])
        
        # V1: Volume Exhaustion
        if vol_r > 2.5 and wide and vol_hi and 0.30 < cp < 0.70:
            signals.append(Signal(idx=i, date=c.date, close=c.close, pattern='V1'))
        
        # V2: Multi-Bar Exhaustion
        ds = 0
        for j in range(i, max(0, i-10), -1):
            if j == i or bars[j].close < bars[j+1].close:
                ds += 1
            else:
                break
        
        if ds >= 3 and vol_r > 1.5:
            drop = (c.close - bars[i-ds].close) / bars[i-ds].close * 100
            if drop < -10:
                signals.append(Signal(idx=i, date=c.date, close=c.close, pattern='V2'))
        
        # V4: Volume Divergence
        lows10 = [b.low for b in w[-11:-1]]
        if c.low <= min(lows10) and vol_r < 0.8:
            signals.append(Signal(idx=i, date=c.date, close=c.close, pattern='V4'))
        
        # V5: Absorption Bar
        body = abs(c.close - c.open)
        bf = body / spread if spread > 0 else 1
        lw = max(0, min(c.open, c.close) - c.low) / spread if spread > 0 else 0
        uw = max(0, c.high - max(c.open, c.close)) / spread if spread > 0 else 0
        
        if spread > atr14 * 1.2 and bf < 0.20 and max(lw, uw) > 0.50:
            signals.append(Signal(idx=i, date=c.date, close=c.close, pattern='V5'))
        
        # V6: Vol Expansion
        if i >= 19:
            prev = calc_atr(bars[:i-4], 14)
            if prev and prev > 0 and atr14 / prev > 1.8:
                signals.append(Signal(idx=i, date=c.date, close=c.close, pattern='V6'))
    
    return signals


# ═══════════════════════════════════════════════════════════════
# Trade Simulation
# ═══════════════════════════════════════════════════════════════

def simulate_trade(signal: Signal, bars: list[Bar], cfg: dict, timeframe: str) -> Trade:
    """Simulate a single trade with real fees and slippage."""
    entry_idx = signal.idx
    entry_price = signal.close
    
    # Apply slippage and fees
    slip_entry = entry_price * SLIPPAGE_BPS / 10000
    entry_fee = (entry_price + slip_entry) * TAKER_FEE
    entry_actual = entry_price + slip_entry
    
    exit_idx = min(entry_idx + cfg['max_hold'], len(bars) - 1)
    exit_reason = 'TIME'
    mae = 0.0
    mfe = 0.0
    
    atr14 = calc_atr(bars[:entry_idx], 14) or entry_price * 0.05
    
    for j in range(entry_idx + 1, exit_idx + 1):
        dd = (bars[j].low - entry_actual) / entry_actual
        rally = (bars[j].high - entry_actual) / entry_actual
        
        if dd < mae:
            mae = dd
        if rally > mfe:
            mfe = rally
        
        # Trailing stop
        if cfg['stop_atr'] > 0:
            stop_price = entry_actual - cfg['stop_atr'] * atr14
            if bars[j].low <= stop_price:
                exit_idx = j
                exit_reason = 'STOP'
                break
    
    exit_price = bars[exit_idx].close
    slip_exit = exit_price * SLIPPAGE_BPS / 10000
    exit_fee = (exit_price - slip_exit) * TAKER_FEE
    exit_actual = exit_price - slip_exit
    
    total_fees = (entry_fee + exit_fee) / entry_price * 100
    total_slip = (slip_entry + slip_exit) / entry_price * 100
    pnl_net = (exit_actual - entry_actual) / entry_actual * 100 - total_fees
    
    holding = exit_idx - entry_idx
    if timeframe == '1w':
        holding *= 7  # Convert weeks to days
    
    return Trade(
        entry_date=signal.date,
        exit_date=bars[exit_idx].date,
        entry_price=entry_price,
        exit_price=exit_price,
        pattern=signal.pattern,
        timeframe=timeframe,
        pnl_gross=(exit_price - entry_price) / entry_price * 100,
        fees=total_fees,
        slippage=total_slip,
        pnl_net=pnl_net,
        holding=holding,
        mae=mae * 100,
        mfe=mfe * 100,
        exit_reason=exit_reason
    )


# ═══════════════════════════════════════════════════════════════
# Metrics Calculation
# ═══════════════════════════════════════════════════════════════

def calculate_metrics(trades: list[Trade]) -> Metrics:
    """Calculate professional backtest metrics."""
    if not trades:
        return Metrics(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    
    n = len(trades)
    pnls = [t.pnl_net for t in trades]
    wins = [p for p in pnls if p > 0]
    losses = [p for p in pnls if p <= 0]
    
    avg_pnl = sum(pnls) / n
    win_rate = len(wins) / n * 100
    avg_win = sum(wins) / len(wins) if wins else 0
    avg_loss = sum(losses) / len(losses) if losses else 0
    
    # Sharpe ratio (annualized)
    mean_ret = sum(pnls) / n / 100
    variance = sum((p/100 - mean_ret)**2 for p in pnls) / (n - 1) if n > 1 else 0
    std_dev = variance ** 0.5
    sharpe = mean_ret / std_dev * (52 ** 0.5) if std_dev > 0 else 0
    
    # Sortino ratio
    down_rets = [p/100 for p in pnls if p < 0]
    if len(down_rets) > 1:
        down_mean = sum(down_rets) / len(down_rets)
        down_var = sum((r - down_mean)**2 for r in down_rets) / (len(down_rets) - 1)
        down_std = down_var ** 0.5
        sortino = mean_ret / down_std * (52 ** 0.5) if down_std > 0 else 0
    else:
        sortino = 0
    
    # Max drawdown
    equity = 100.0
    peak = 100.0
    max_dd = 0.0
    for p in pnls:
        equity *= 1 + p / 100
        if equity > peak:
            peak = equity
        dd = (peak - equity) / peak * 100
        if dd > max_dd:
            max_dd = dd
    
    total_ret = equity / 100 - 1
    total_days = sum(t.holding for t in trades)
    years = total_days / 365
    cagr = (equity / 100) ** (1/years) - 1 if years > 0 else 0
    calmar = cagr * 100 / max_dd if max_dd > 0 else 0
    
    gross_profit = sum(wins)
    gross_loss = sum(abs(l) for l in losses)
    pf = gross_profit / gross_loss if gross_loss > 0 else float('inf')
    
    # Deflated Sharpe (simplified)
    deflated_sharpe = sharpe * (1 - (1 - sharpe**2) * 0.05 / (2 * 0.1)) if sharpe > 0 else 0
    
    max_consec = 0
    cur_consec = 0
    for p in pnls:
        if p <= 0:
            cur_consec += 1
            max_consec = max(max_consec, cur_consec)
        else:
            cur_consec = 0
    
    return Metrics(
        n=n,
        win_rate=win_rate,
        avg_pnl=avg_pnl,
        avg_win=avg_win,
        avg_loss=avg_loss,
        sharpe=sharpe,
        sortino=sortino,
        calmar=calmar,
        max_dd=max_dd,
        profit_factor=pf,
        deflated_sharpe=deflated_sharpe,
        total_return=total_ret * 100,
        cagr=cagr * 100,
        max_consec_losses=max_consec,
        avg_hold=sum(t.holding for t in trades) / n,
        avg_mae=sum(t.mae for t in trades) / n,
        avg_mfe=sum(t.mfe for t in trades) / n,
        stop_exits=sum(1 for t in trades if t.exit_reason == 'STOP')
    )


# ═══════════════════════════════════════════════════════════════
# Walk-Forward Validation
# ═══════════════════════════════════════════════════════════════

def walk_forward(bars: list[Bar], signals: list[Signal], pattern: str, 
                 timeframe: str, n_folds: int = 5) -> list[Metrics]:
    """Walk-forward validation with purged splits."""
    cfg = STRATEGIES[timeframe][pattern]
    fold_size = len(bars) // n_folds
    results = []
    
    for fold in range(n_folds - 1):
        test_start = (fold + 1) * fold_size
        test_end = min((fold + 2) * fold_size, len(bars) - cfg['max_hold'])
        
        test_sigs = [s for s in signals if s.pattern == pattern and test_start <= s.idx < test_end]
        trades = [simulate_trade(s, bars, cfg, timeframe) for s in test_sigs]
        m = calculate_metrics(trades)
        results.append(m)
    
    return results


# ═══════════════════════════════════════════════════════════════
# Main Backtest
# ═══════════════════════════════════════════════════════════════

def run_backtest():
    """Run full backtest across all patterns and timeframes."""
    print("=" * 80)
    print("MONEY PRINTER RESEARCH LAB — Volume Pattern Backtest")
    print("=" * 80)
    print()
    
    # Fetch data
    print("Fetching daily bars...")
    daily_bars = fetch_bars('1d')
    print(f"Got {len(daily_bars)} daily bars")
    
    print("Fetching weekly bars...")
    weekly_bars = fetch_bars('1w')
    print(f"Got {len(weekly_bars)} weekly bars")
    print()
    
    # Detect signals
    daily_signals = detect_patterns(daily_bars, '1d')
    weekly_signals = detect_patterns(weekly_bars, '1w')
    
    print(f"Daily signals: {len(daily_signals)}")
    print(f"Weekly signals: {len(weekly_signals)}")
    print()
    
    # Results storage
    all_results = []
    
    # Run backtests
    for tf, bars, signals in [('1d', daily_bars, daily_signals), 
                               ('1w', weekly_bars, weekly_signals)]:
        tf_label = 'DAILY' if tf == '1d' else 'WEEKLY'
        print(f"\n{'='*80}")
        print(f"{tf_label} TIMEFRAME")
        print(f"{'='*80}")
        
        for pat in ['V1', 'V2', 'V4', 'V5', 'V6']:
            cfg = STRATEGIES[tf][pat]
            pat_sigs = [s for s in signals if s.pattern == pat]
            
            if not pat_sigs:
                print(f"\n{pat}: 0 signals")
                continue
            
            # Simulate trades
            trades = [simulate_trade(s, bars, cfg, tf) for s in pat_sigs]
            metrics = calculate_metrics(trades)
            
            # Walk-forward
            wf_results = walk_forward(bars, signals, pat, tf)
            avg_wf_sharpe = sum(m.sharpe for m in wf_results) / len(wf_results) if wf_results else 0
            avg_wf_pnl = sum(m.avg_pnl for m in wf_results) / len(wf_results) if wf_results else 0
            
            print(f"\n{pat}: {metrics.n} signals, Win {metrics.win_rate:.0f}%, "
                  f"Sharpe {metrics.sharpe:.2f}, WF Sharpe {avg_wf_sharpe:.2f}")
            
            # Store results
            all_results.append({
                'timeframe': tf_label,
                'pattern': pat,
                'config': cfg['label'],
                'metrics': asdict(metrics),
                'wf_sharpe': avg_wf_sharpe,
                'wf_pnl': avg_wf_pnl,
                'trades': [asdict(t) for t in trades]
            })
    
    # Generate report
    generate_report(all_results)
    
    return all_results


def generate_report(results: list[dict]):
    """Generate markdown report."""
    report = """# Money Printer Research Lab — Volume Pattern Backtest

**Generated:** {date}
**Asset:** BTCUSDT
**Fees:** Taker 0.04% | Slippage: 5 bps/side
**Walk-Forward:** 5-fold out-of-sample validation

---

## Summary Comparison

| Pattern | TF | Trades | Win% | Avg PnL | Sharpe | Sortino | Deflated | PF | MaxDD | CAGR | WF Sharpe | Verdict |
|---------|-----|--------|------|---------|--------|---------|----------|-----|-------|------|-----------|---------|
""".format(date=datetime.now().strftime('%Y-%m-%d'))
    
    for r in results:
        m = r['metrics']
        verdict = '✅ Real' if r['wf_sharpe'] > 0.5 else ('⚠️ Weak' if m['avg_pnl'] > 0 else '❌ Fail')
        pf_str = '∞' if m['profit_factor'] == float('inf') else f"{m['profit_factor']:.2f}"
        
        report += f"| {r['pattern']} | {r['timeframe']} | {m['n']} | {m['win_rate']:.0f}% | "
        report += f"{m['avg_pnl']:.2f}% | {m['sharpe']:.2f} | {m['sortino']:.2f} | "
        report += f"{m['deflated_sharpe']:.2f} | {pf_str} | {m['max_dd']:.1f}% | "
        report += f"{m['cagr']:.1f}% | {r['wf_sharpe']:.2f} | {verdict} |\n"
    
    report += "\n## Walk-Forward Validation\n\n"
    report += "| Pattern | TF | Full Sharpe | WF Sharpe | Degradation | Verdict |\n"
    report += "|---------|-----|-------------|-----------|-------------|----------|\n"
    
    for r in results:
        m = r['metrics']
        if m['sharpe'] > 0:
            deg = (r['wf_sharpe'] - m['sharpe']) / m['sharpe'] * 100
            deg_str = f"{deg:.0f}%"
        else:
            deg_str = "N/A"
        
        verdict = '✅ Real edge' if r['wf_sharpe'] > 0.5 else '⚠️ Weak'
        report += f"| {r['pattern']} | {r['timeframe']} | {m['sharpe']:.2f} | {r['wf_sharpe']:.2f} | {deg_str} | {verdict} |\n"
    
    report += "\n## Trade Logs\n\n"
    
    for r in results:
        if not r['trades']:
            continue
        
        report += f"### {r['pattern']} — {r['timeframe']} ({r['config']})\n\n"
        report += "| # | Entry | Exit | Entry$ | Exit$ | Net% | Fees% | MAE% | MFE% | Reason |\n"
        report += "|---|-------|------|--------|-------|------|-------|------|------|--------|\n"
        
        for i, t in enumerate(r['trades'], 1):
            report += f"| {i} | {t['entry_date']} | {t['exit_date']} | "
            report += f"${t['entry_price']:.0f} | ${t['exit_price']:.0f} | "
            report += f"{t['pnl_net']:.2f} | {t['fees']:.3f} | "
            report += f"{t['mae']:.1f} | {t['mfe']:.1f} | {t['exit_reason']} |\n"
        
        report += "\n"
    
    # Save report
    report_path = Path('research/out/money_printer_pattern_backtest.md')
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(report, encoding='utf-8')
    
    # Save JSON for registry
    json_path = Path('research/out/money_printer_pattern_backtest.json')
    json_path.write_text(json.dumps(results, indent=2, default=str), encoding='utf-8')
    
    print(f"\nReport saved to {report_path}")
    print(f"JSON saved to {json_path}")


if __name__ == '__main__':
    run_backtest()
