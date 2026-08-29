#!/usr/bin/env python3
"""
Backtest All Volume Patterns on SOLUSDT
Tests if the edge transfers from BTC to altcoins
"""

import json
import math
import urllib.request
from datetime import datetime
from pathlib import Path
from dataclasses import dataclass, asdict
from typing import List, Optional

TAKER_FEE = 0.0004
SLIPPAGE_BPS = 5
MIN_TRADES = 5

@dataclass
class Bar:
    time: int
    date: str
    open: float
    high: float
    low: float
    close: float
    volume: float

def fetch_bars(symbol, interval='1d', start_date='2020-04-01'):
    """Fetch bars from Binance API."""
    all_bars = []
    start_time = int(datetime.strptime(start_date, '%Y-%m-%d').timestamp() * 1000)
    end_time = int(datetime.now().timestamp() * 1000)
    increment = 604800000 if interval == '1w' else 86400000
    
    print(f"Fetching {symbol} {interval} bars from Binance...")
    while start_time < end_time:
        url = f'https://api.binance.com/api/v3/klines?symbol={symbol}&interval={interval}&startTime={start_time}&limit=1000'
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

def detect_signals(bars, pattern, tf='1d'):
    """Detect pattern signals."""
    signals = []
    sma_period = 20 if tf == '1w' else 50
    lookback = 21 if tf == '1w' else 20
    
    for i in range(max(sma_period, 30), len(bars) - (12 if tf == '1w' else 30)):
        c = bars[i]
        
        atr14 = calc_atr(bars[:i], 14)
        if not atr14:
            continue
        
        # Volume metrics
        volMA = sum(b.volume for b in bars[max(0,i-lookback):i]) / lookback
        volR = c.volume / volMA if volMA > 0 else 0
        spread = c.high - c.low
        cp = (c.close - c.low) / spread if spread > 0 else 0.5
        wide = spread > atr14 * 1.5
        
        # Down streak
        ds = 0
        for j in range(i, max(0, i-10), -1):
            if bars[j].close < bars[j-1].close:
                ds += 1
            else:
                break
        cumDrop = (c.close - bars[i-ds].close) / bars[i-ds].close * 100 if ds > 0 else 0
        
        # Body fraction
        body = abs(c.close - c.open)
        bf = body / spread if spread > 0 else 1
        lw = max(0, min(c.open, c.close) - c.low) / spread if spread > 0 else 0
        uw = max(0, c.high - max(c.open, c.close)) / spread if spread > 0 else 0
        
        # Trend filter
        closes = [b.close for b in bars[:i+1]]
        sma = calc_sma(closes, sma_period)
        in_uptrend = c.close > sma if sma else True
        
        # V1: Volume exhaustion
        if pattern == 'V1':
            if volR > 2.5 and wide and 0.30 < cp < 0.70:
                signals.append({
                    'idx': i, 'date': c.date, 'close': c.close,
                    'in_uptrend': in_uptrend, 'pattern': 'V1'
                })
        
        # V2: Multi-bar exhaustion + range breakout
        elif pattern == 'V2':
            if ds >= 3:
                # Check for range
                range_high = max(b.high for b in bars[i:i+15]) if i+15 <= len(bars) else max(b.high for b in bars[i:])
                range_low = min(b.low for b in bars[i:i+15]) if i+15 <= len(bars) else min(b.low for b in bars[i:])
                range_width = range_high - range_low
                
                if range_width <= 6 * atr14:
                    # Look for breakout
                    for k in range(i+1, min(i+15, len(bars))):
                        breakout_bar = bars[k]
                        vol_ma = sum(b.volume for b in bars[max(0,k-20):k]) / min(20, k)
                        
                        if breakout_bar.close > range_high and breakout_bar.volume > vol_ma * 1.1:
                            signals.append({
                                'idx': k, 'date': breakout_bar.date, 'close': breakout_bar.close,
                                'in_uptrend': in_uptrend, 'pattern': 'V2', 'direction': 'long'
                            })
                            break
                        elif breakout_bar.close < range_low and breakout_bar.volume > vol_ma * 1.1:
                            signals.append({
                                'idx': k, 'date': breakout_bar.date, 'close': breakout_bar.close,
                                'in_uptrend': in_uptrend, 'pattern': 'V2', 'direction': 'short'
                            })
                            break
        
        # V4: Volume divergence
        elif pattern == 'V4':
            lows10 = [b.low for b in bars[max(0,i-11):i]]
            if lows10 and c.low <= min(lows10) and volR < 0.8:
                signals.append({
                    'idx': i, 'date': c.date, 'close': c.close,
                    'in_uptrend': in_uptrend, 'pattern': 'V4'
                })
        
        # V5: Absorption bar
        elif pattern == 'V5':
            if spread > atr14 * 1.2 and bf < 0.20 and max(lw, uw) > 0.50:
                signals.append({
                    'idx': i, 'date': c.date, 'close': c.close,
                    'in_uptrend': in_uptrend, 'pattern': 'V5'
                })
        
        # V6: Vol expansion
        elif pattern == 'V6':
            if i >= 19:
                prev_atr = calc_atr(bars[:i-4], 14)
                if prev_atr and prev_atr > 0 and atr14 / prev_atr > 1.8:
                    signals.append({
                        'idx': i, 'date': c.date, 'close': c.close,
                        'in_uptrend': in_uptrend, 'pattern': 'V6'
                    })
    
    return signals

def simulate_trade(signal, bars, trail_atr=2.0, max_hold=70, use_trend=True):
    """Simulate a single trade."""
    i = signal['idx']
    entry = signal['close']
    
    # Apply trend filter
    if use_trend and not signal.get('in_uptrend', True):
        return None
    
    slip = entry * SLIPPAGE_BPS / 10000
    fee = (entry + slip) * TAKER_FEE
    entry_act = entry + slip
    
    atr14 = calc_atr(bars[:i], 14) or entry * 0.05
    trail_price = entry_act - trail_atr * atr14
    
    exit_idx = min(i + max_hold, len(bars) - 1)
    reason = 'TIME'
    mae = 0
    mfe = 0
    
    for j in range(i + 1, exit_idx + 1):
        dd = (bars[j].low - entry_act) / entry_act
        rally = (bars[j].high - entry_act) / entry_act
        
        if dd < mae:
            mae = dd
        if rally > mfe:
            mfe = rally
        
        trail_price = max(trail_price, bars[j].close - trail_atr * atr14)
        if bars[j].low <= trail_price:
            exit_idx = j
            reason = 'TRAIL'
            break
    
    exit_p = bars[exit_idx].close
    slip2 = exit_p * SLIPPAGE_BPS / 10000
    fee2 = (exit_p - slip2) * TAKER_FEE
    
    pnl = ((exit_p - slip2) - entry_act) / entry_act * 100
    pnl -= (fee + fee2) / entry * 100
    
    return {
        'entry_date': signal['date'],
        'exit_date': bars[exit_idx].date,
        'pnl_pct': pnl,
        'holding': exit_idx - i,
        'mae': mae * 100,
        'mfe': mfe * 100,
        'exit_type': reason
    }

def compute_metrics(trades, label):
    """Compute strategy metrics."""
    if not trades or len(trades) < MIN_TRADES:
        return None
    
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
    
    # CAGR
    if len(trades) > 1:
        calendar_years = max((datetime.strptime(trades[-1]['exit_date'], '%Y-%m-%d') - 
                             datetime.strptime(trades[0]['entry_date'], '%Y-%m-%d')).days / 365.25, 0.01)
        cagr = (equity / 100) ** (1 / calendar_years) - 1
    else:
        calendar_years = 0.01
        cagr = 0
    
    return {
        'strategy': label,
        'trades': n,
        'win_rate': round(win_rate, 1),
        'avg_pnl': round(avg_pnl, 2),
        'sharpe': round(sharpe, 2),
        'profit_factor': round(pf, 2),
        'max_drawdown': round(max_dd, 1),
        'total_return': round((equity / 100 - 1) * 100, 1),
        'cagr': round(cagr * 100, 1)
    }

def main():
    print("="*80)
    print("SOLUSDT — ALL VOLUME PATTERNS BACKTEST")
    print("="*80)
    
    # Fetch SOL data
    sol_daily = fetch_bars('SOLUSDT', '1d', '2020-04-01')
    sol_weekly = fetch_bars('SOLUSDT', '1w', '2020-04-01')
    
    # Patterns to test
    patterns = ['V1', 'V2', 'V4', 'V5', 'V6']
    
    results = []
    all_trades = {}
    
    for pattern in patterns:
        print(f"\n{'='*60}")
        print(f"Testing {pattern} on SOLUSDT")
        print(f"{'='*60}")
        
        # Daily
        daily_signals = detect_signals(sol_daily, pattern, '1d')
        daily_trades = [t for t in [simulate_trade(s, sol_daily) for s in daily_signals] if t]
        daily_m = compute_metrics(daily_trades, f"{pattern} Daily")
        
        # Weekly
        weekly_signals = detect_signals(sol_weekly, pattern, '1w')
        weekly_trades = [t for t in [simulate_trade(s, sol_weekly, trail_atr=3.0, max_hold=12) for s in weekly_signals] if t]
        weekly_m = compute_metrics(weekly_trades, f"{pattern} Weekly")
        
        # With trend filter
        daily_trend_trades = [t for t in [simulate_trade(s, sol_daily, use_trend=True) for s in daily_signals] if t]
        daily_trend_m = compute_metrics(daily_trend_trades, f"{pattern} Daily+Trend")
        
        weekly_trend_trades = [t for t in [simulate_trade(s, sol_weekly, trail_atr=3.0, max_hold=12, use_trend=True) for s in weekly_signals] if t]
        weekly_trend_m = compute_metrics(weekly_trend_trades, f"{pattern} Weekly+Trend")
        
        # Print results
        print(f"\nDaily: {len(daily_trades)} trades, {len(daily_signals)} signals")
        if daily_m:
            print(f"  Win%: {daily_m['win_rate']}%, Avg PnL: {daily_m['avg_pnl']:+.2f}%, Sharpe: {daily_m['sharpe']:.2f}, PF: {daily_m['profit_factor']:.2f}")
            results.append(daily_m)
        
        print(f"Weekly: {len(weekly_trades)} trades, {len(weekly_signals)} signals")
        if weekly_m:
            print(f"  Win%: {weekly_m['win_rate']}%, Avg PnL: {weekly_m['avg_pnl']:+.2f}%, Sharpe: {weekly_m['sharpe']:.2f}, PF: {weekly_m['profit_factor']:.2f}")
            results.append(weekly_m)
        
        print(f"Daily+Trend: {len(daily_trend_trades)} trades")
        if daily_trend_m:
            print(f"  Win%: {daily_trend_m['win_rate']}%, Avg PnL: {daily_trend_m['avg_pnl']:+.2f}%, Sharpe: {daily_trend_m['sharpe']:.2f}, PF: {daily_trend_m['profit_factor']:.2f}")
            results.append(daily_trend_m)
        
        print(f"Weekly+Trend: {len(weekly_trend_trades)} trades")
        if weekly_trend_m:
            print(f"  Win%: {weekly_trend_m['win_rate']}%, Avg PnL: {weekly_trend_m['avg_pnl']:+.2f}, Sharpe: {weekly_trend_m['sharpe']:.2f}, PF: {weekly_trend_m['profit_factor']:.2f}")
            results.append(weekly_trend_m)
        
        # Store trades
        all_trades[f"{pattern}_daily"] = daily_trades
        all_trades[f"{pattern}_weekly"] = weekly_trades
    
    # Print comparison table
    print("\n" + "="*100)
    print("SOLUSDT RESULTS SUMMARY")
    print("="*100)
    print(f"\n{'Strategy':<25} {'Trades':<8} {'Win%':<8} {'Avg PnL':<10} {'Sharpe':<8} {'PF':<8} {'MaxDD':<8} {'Return':<8}")
    print("-"*90)
    
    for r in results:
        print(f"{r['strategy']:<25} {r['trades']:<8} {r['win_rate']:<8.1f} {r['avg_pnl']:<+10.2f} {r['sharpe']:<8.2f} {r['profit_factor']:<8.2f} {r['max_drawdown']:<8.1f} {r['total_return']:<+8.1f}")
    
    # Compare with BTC
    print("\n" + "="*100)
    print("BTC vs SOL COMPARISON")
    print("="*100)
    print(f"\n{'Pattern':<15} {'BTC Sharpe':<12} {'SOL Sharpe':<12} {'BTC Win%':<10} {'SOL Win%':<10} {'Transfer?'}")
    print("-"*70)
    
    # Load BTC results if available
    btc_path = Path("research/out/all_strategies_backtest.json")
    if btc_path.exists():
        with open(btc_path) as f:
            btc_data = json.load(f)
        
        for pattern in patterns:
            btc_results = [r for r in btc_data if r['pattern'] == pattern and r['tf'] == '1d']
            sol_results = [r for r in results if pattern in r['strategy'] and 'Daily' in r['strategy'] and 'Trend' not in r['strategy']]
            
            btc_sharpe = btc_results[0]['sharpe'] if btc_results else 0
            btc_win = btc_results[0]['win_rate'] if btc_results else 0
            sol_sharpe = sol_results[0]['sharpe'] if sol_results else 0
            sol_win = sol_results[0]['win_rate'] if sol_results else 0
            
            # Edge transfer: both positive Sharpe
            transfers = btc_sharpe > 0 and sol_sharpe > 0
            
            print(f"{pattern:<15} {btc_sharpe:<12.2f} {sol_sharpe:<12.2f} {btc_win:<10.1f} {sol_win:<10.1f} {'✅ YES' if transfers else '❌ NO'}")
    
    # Save results
    out_path = Path("research/out/sol_backtest_results.json")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, 'w') as f:
        json.dump({
            'results': results,
            'trades': {k: v for k, v in all_trades.items()}
        }, f, indent=2)
    
    print(f"\nResults saved to {out_path}")
    
    # Print best configs for SOL
    print("\n" + "="*100)
    print("RECOMMENDED SOL STRATEGIES")
    print("="*100)
    
    best = sorted([r for r in results if r['trades'] >= 5], key=lambda x: x['sharpe'], reverse=True)
    
    for i, r in enumerate(best[:5], 1):
        print(f"\n{i}. {r['strategy']}")
        print(f"   Trades: {r['trades']}, Win Rate: {r['win_rate']}%")
        print(f"   Avg PnL: {r['avg_pnl']:+.2f}%, Sharpe: {r['sharpe']:.2f}")
        print(f"   Profit Factor: {r['profit_factor']:.2f}, Max DD: {r['max_drawdown']:.1f}%")

if __name__ == "__main__":
    main()
