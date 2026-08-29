#!/usr/bin/env python3
"""V2 Range Breakout Backtest: min_streak=5 vs 3 on daily BTCUSDT"""

import json
import math
import urllib.request
from datetime import datetime
from pathlib import Path
from dataclasses import dataclass
from typing import List, Optional

TAKER_FEE = 0.0004
SLIPPAGE_BPS = 5

@dataclass
class Bar:
    time: int
    date: str
    open: float
    high: float
    low: float
    close: float
    volume: float

def fetch_bars(interval='1d', start_date='2017-08-17'):
    """Fetch bars from Binance API."""
    all_bars = []
    start_time = int(datetime.strptime(start_date, '%Y-%m-%d').timestamp() * 1000)
    end_time = int(datetime.now().timestamp() * 1000)
    increment = 86400000
    
    print(f"Fetching {interval} bars from Binance...")
    while start_time < end_time:
        url = f'https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval={interval}&startTime={start_time}&limit=1000'
        try:
            with urllib.request.urlopen(url) as response:
                data = json.loads(response.read())
        except Exception as e:
            print(f"  Error: {e}")
            break
        if not data:
            break
        for bar in data:
            all_bars.append(Bar(
                time=bar[0],
                date=datetime.fromtimestamp(bar[0]/1000).strftime('%Y-%m-%d'),
                open=float(bar[1]), high=float(bar[2]),
                low=float(bar[3]), close=float(bar[4]),
                volume=float(bar[5])
            ))
        start_time = data[-1][0] + increment
    
    print(f"  Got {len(all_bars)} bars")
    return all_bars

def calc_atr(bars, n=14):
    """Calculate ATR(n)."""
    if len(bars) < n + 1:
        return None
    trs = []
    for i in range(1, len(bars)):
        tr = max(
            bars[i].high - bars[i].low,
            abs(bars[i].high - bars[i-1].close),
            abs(bars[i].low - bars[i-1].close)
        )
        trs.append(tr)
    if len(trs) < n:
        return None
    return sum(trs[-n:]) / n

def calc_sma(closes, n):
    """Calculate SMA(n)."""
    if len(closes) < n:
        return None
    return sum(closes[-n:]) / n

def detect_v2_signals(bars, min_streak=3):
    """Detect V2 range breakout signals."""
    signals = []
    sma_period = 50
    range_lookback = 15  # Bars to form range
    breakout_lookforward = 15  # Bars to look for breakout
    
    for i in range(max(sma_period, 30), len(bars) - breakout_lookforward - range_lookback):
        c = bars[i]
        
        # Compute ATR
        atr14 = calc_atr(bars[:i], 14)
        if not atr14:
            continue
        
        # Count consecutive down days
        ds = 0
        for j in range(i, max(0, i-10), -1):
            if bars[j].close < bars[j-1].close:
                ds += 1
            else:
                break
        
        # Must meet minimum streak
        if ds < min_streak:
            continue
        
        # Compute trend
        closes = [b.close for b in bars[:i+1]]
        sma = calc_sma(closes, sma_period)
        in_uptrend = c.close > sma if sma else True
        
        # Look for range formation (15 bars AFTER the streak)
        range_start = i + 1
        range_end = min(i + range_lookback, len(bars))
        
        if range_end <= range_start:
            continue
            
        range_high = max(b.high for b in bars[range_start:range_end])
        range_low = min(b.low for b in bars[range_start:range_end])
        range_width = range_high - range_low
        
        # Check if range is reasonable
        if range_width > 6 * atr14:
            continue
        
        # Look for breakout AFTER the range (bars range_end to range_end+15)
        breakout_start = range_end
        breakout_end = min(breakout_start + breakout_lookforward, len(bars))
        
        for k in range(breakout_start, breakout_end):
            breakout_bar = bars[k]
            vol_ma = sum(b.volume for b in bars[max(0,k-20):k]) / min(20, k)
            
            # Breakout up
            if (breakout_bar.close > range_high and 
                breakout_bar.volume > vol_ma * 1.1):
                signals.append({
                    'idx': k,
                    'date': breakout_bar.date,
                    'close': breakout_bar.close,
                    'direction': 'long',
                    'in_uptrend': in_uptrend,
                    'down_streak': ds,
                    'range_high': range_high,
                    'range_low': range_low
                })
                break
            
            # Breakout down
            if (breakout_bar.close < range_low and
                breakout_bar.volume > vol_ma * 1.1):
                signals.append({
                    'idx': k,
                    'date': breakout_bar.date,
                    'close': breakout_bar.close,
                    'direction': 'short',
                    'in_uptrend': in_uptrend,
                    'down_streak': ds,
                    'range_high': range_high,
                    'range_low': range_low
                })
                break
    
    return signals

def simulate_trade(signal, bars, trail_atr=1.5, max_hold=21, use_trend=True):
    """Simulate a single trade."""
    i = signal['idx']
    entry = signal['close']
    direction = signal['direction']
    
    # Apply filters
    if use_trend:
        if direction == 'long' and not signal['in_uptrend']:
            return None
        if direction == 'short' and signal['in_uptrend']:
            return None
    
    slip = entry * SLIPPAGE_BPS / 10000
    fee = (entry + slip) * TAKER_FEE
    entry_act = entry + slip
    
    atr14 = calc_atr(bars[:i], 14) or entry * 0.05
    trail_price = entry_act - trail_atr * atr14 if direction == 'long' else entry_act + trail_atr * atr14
    
    exit_idx = min(i + max_hold, len(bars) - 1)
    reason = 'TIME'
    mae = 0
    mfe = 0
    
    for j in range(i + 1, exit_idx + 1):
        if direction == 'long':
            dd = (bars[j].low - entry_act) / entry_act
            rally = (bars[j].high - entry_act) / entry_act
        else:
            dd = (entry_act - bars[j].high) / entry_act
            rally = (entry_act - bars[j].low) / entry_act
        
        if dd < mae:
            mae = dd
        if rally > mfe:
            mfe = rally
        
        # Update trailing stop
        if direction == 'long':
            trail_price = max(trail_price, bars[j].close - trail_atr * atr14)
            if bars[j].low <= trail_price:
                exit_idx = j
                reason = 'TRAIL'
                break
        else:
            trail_price = min(trail_price, bars[j].close + trail_atr * atr14)
            if bars[j].high >= trail_price:
                exit_idx = j
                reason = 'TRAIL'
                break
    
    exit_p = bars[exit_idx].close
    slip2 = exit_p * SLIPPAGE_BPS / 10000
    fee2 = (exit_p - slip2) * TAKER_FEE
    
    if direction == 'long':
        pnl = ((exit_p - slip2) - entry_act) / entry_act * 100
    else:
        pnl = (entry_act - (exit_p - slip2)) / entry_act * 100
    
    pnl -= (fee + fee2) / entry * 100
    
    return {
        'entry_date': signal['date'],
        'exit_date': bars[exit_idx].date,
        'entry': entry,
        'exit': exit_p,
        'pnl_pct': pnl,
        'holding': exit_idx - i,
        'mae': mae * 100,
        'mfe': mfe * 100,
        'exit_type': reason,
        'direction': direction
    }

def compute_metrics(trades, label):
    """Compute strategy metrics."""
    if not trades:
        return {
            'strategy': label,
            'trades': 0,
            'win_rate': 0,
            'avg_pnl': 0,
            'sharpe': 0,
            'profit_factor': 0,
            'max_drawdown': 0
        }
    
    n = len(trades)
    wins = [t for t in trades if t['pnl_pct'] > 0]
    losses = [t for t in trades if t['pnl_pct'] <= 0]
    
    win_rate = len(wins) / n * 100
    avg_pnl = sum(t['pnl_pct'] for t in trades) / n
    
    # Sharpe
    if n > 1:
        avg = sum(t['pnl_pct'] for t in trades) / n
        var = sum((t['pnl_pct'] - avg) ** 2 for t in trades) / (n - 1)
        std = math.sqrt(var)
        sharpe = (avg / std) * math.sqrt(52) if std > 0 else 0
    else:
        sharpe = 0
    
    # Profit Factor
    gross_profit = sum(t['pnl_pct'] for t in wins) if wins else 0
    gross_loss = abs(sum(t['pnl_pct'] for t in losses)) if losses else 0.0001
    pf = min(gross_profit / gross_loss, 999.99) if gross_loss > 0 else 999.99
    
    # Max Drawdown
    equity = 100
    peak = 100
    max_dd = 0
    for t in trades:
        equity *= (1 + t['pnl_pct'] / 100)
        peak = max(peak, equity)
        dd = (peak - equity) / peak * 100
        max_dd = max(max_dd, dd)
    
    # Walk-forward
    if len(trades) > 1:
        calendar_years = max((datetime.strptime(trades[-1]['exit_date'], '%Y-%m-%d') - 
                             datetime.strptime(trades[0]['entry_date'], '%Y-%m-%d')).days / 365.25, 0.01)
        trades_per_year = n / calendar_years
    else:
        calendar_years = 0.01
        trades_per_year = 0
    
    return {
        'strategy': label,
        'trades': n,
        'win_rate': round(win_rate, 1),
        'avg_pnl': round(avg_pnl, 2),
        'sharpe': round(sharpe, 2),
        'profit_factor': round(pf, 2),
        'max_drawdown': round(max_dd, 1),
        'avg_holding': round(sum(t['holding'] for t in trades) / n, 1),
        'trades_per_year': round(trades_per_year, 2)
    }

def walk_forward_validation(bars, min_streak, trail_atr=1.5, max_hold=21, use_trend=True, is_ratio=0.6):
    """Walk-forward validation."""
    n_bars = len(bars)
    split = int(n_bars * is_ratio)
    
    is_bars = bars[:split]
    oos_bars = bars[split:]
    
    # Detect signals in each period
    is_signals = detect_v2_signals(is_bars, min_streak)
    oos_signals = detect_v2_signals(oos_bars, min_streak)
    
    # Adjust OOS indices
    for s in oos_signals:
        s['idx'] += split
    
    # Simulate
    is_trades = [t for t in [simulate_trade(s, is_bars, trail_atr, max_hold, use_trend) for s in is_signals] if t]
    oos_trades = [t for t in [simulate_trade(s, oos_bars, trail_atr, max_hold, use_trend) for s in oos_signals] if t]
    
    is_m = compute_metrics(is_trades, 'IS')
    oos_m = compute_metrics(oos_trades, 'OOS')
    
    return is_m, oos_m

def main():
    print("="*80)
    print("V2 RANGE BREAKOUT: min_streak=5 vs 3")
    print("="*80)
    
    # Fetch data
    bars = fetch_bars('1d')
    
    # Test different min_streak values
    configs = [
        (3, True, "min_streak=3, trend"),
        (5, True, "min_streak=5, trend"),
        (7, True, "min_streak=7, trend"),
        (3, False, "min_streak=3, no trend"),
        (5, False, "min_streak=5, no trend"),
    ]
    
    results = []
    all_trades = {}
    
    for min_streak, use_trend, label in configs:
        print(f"\nTesting {label}...")
        
        # Detect signals
        signals = detect_v2_signals(bars, min_streak)
        print(f"  Signals: {len(signals)}")
        
        # Simulate trades
        trades = [t for t in [simulate_trade(s, bars, use_trend=use_trend) for s in signals] if t]
        
        # Compute metrics
        m = compute_metrics(trades, label)
        results.append(m)
        all_trades[label] = trades
        
        # Walk-forward
        is_m, oos_m = walk_forward_validation(bars, min_streak, use_trend=use_trend)
        
        print(f"  Trades: {m['trades']}")
        print(f"  Win Rate: {m['win_rate']}%")
        print(f"  Avg PnL: {m['avg_pnl']:+.2f}%")
        print(f"  Sharpe: {m['sharpe']:.2f}")
        print(f"  PF: {m['profit_factor']:.2f}")
        print(f"  Max DD: {m['max_drawdown']:.1f}%")
        print(f"  Walk-Forward: IS={is_m['sharpe']:.2f}, OOS={oos_m['sharpe']:.2f}")
        
        # Show sample trades
        if trades:
            print(f"\n  Sample trades:")
            for t in trades[:5]:
                print(f"    {t['entry_date']} -> {t['exit_date']}: {t['pnl_pct']:+.2f}% ({t['holding']}d, {t['exit_type']}, {t['direction']})")
    
    # Print comparison
    print("\n" + "="*80)
    print("COMPARISON TABLE")
    print("="*80)
    print(f"\n{'Config':<25} {'Trades':<10} {'Win%':<10} {'Avg PnL':<12} {'Sharpe':<10} {'PF':<10} {'MaxDD':<10}")
    print("-"*85)
    
    for m in results:
        print(f"{m['strategy']:<25} {m['trades']:<10} {m['win_rate']:<10.1f} {m['avg_pnl']:<+12.2f} {m['sharpe']:<10.2f} {m['profit_factor']:<10.2f} {m['max_drawdown']:<10.1f}")
    
    # Save results
    out_path = Path("research/out/v2_strict_backtest.json")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, 'w') as f:
        json.dump({
            'results': results,
            'trades': {k: v for k, v in all_trades.items()}
        }, f, indent=2)
    
    print(f"\nResults saved to {out_path}")

if __name__ == "__main__":
    main()
