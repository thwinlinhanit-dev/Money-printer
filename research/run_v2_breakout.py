#!/usr/bin/env python3
"""
V2 Breakout Strategy — Trade the range breakout after multi-bar exhaustion

Pattern: Multi-bar selling > Range forms > Buy breakout
"""

import json
from datetime import datetime
from pathlib import Path
from dataclasses import dataclass, asdict

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
        except:
            break
        if not data: break
        for bar in data:
            all_bars.append(Bar(
                time=bar[0],
                date=datetime.fromtimestamp(bar[0]/1000).strftime('%Y-%m-%d'),
                open=float(bar[1]), high=float(bar[2]),
                low=float(bar[3]), close=float(bar[4]),
                volume=float(bar[5])
            ))
        start_time = data[-1][0] + increment
    return all_bars

def calc_atr(bars, n):
    if len(bars) < n: return None
    win = bars[-n:]
    return sum(max(win[i].high-win[i].low,
                   abs(win[i].high-win[i-1].close) if i>0 else win[i].high-win[i].low,
                   abs(win[i].low-win[i-1].close) if i>0 else 0) for i in range(len(win))) / n

def detect_v2_breakouts(bars):
    """Detect V2 pattern: multi-bar exhaustion > range > breakout"""
    signals = []
    
    for i in range(30, len(bars) - 30):
        # Step 1: Check for multi-bar exhaustion (3+ consecutive down days)
        down_streak = 0
        for j in range(i, max(0, i-10), -1):
            if j == i or bars[j].close < bars[j+1].close:
                down_streak += 1
            else:
                break
        
        if down_streak < 3:
            continue
        
        cum_drop = (bars[i].close - bars[i-down_streak].close) / bars[i-down_streak].close * 100
        if cum_drop > -5:  # Need at least 5% drop
            continue
        
        exhaustion_idx = i
        exhaustion_date = bars[i].date
        exhaustion_low = min(bars[j].low for j in range(i-down_streak, i+1))
        exhaustion_high = max(bars[j].high for j in range(i-down_streak, i+1))
        
        # Step 2: Look for range formation (8-60 bars after exhaustion)
        range_high = exhaustion_high
        range_low = exhaustion_low
        range_bars = 0
        breakout_idx = None
        
        for k in range(i+1, min(i+61, len(bars))):
            # Update range boundaries
            range_high = max(range_high, bars[k].high)
            range_low = min(range_low, bars[k].low)
            range_bars += 1
            
            # Check for breakout (close above range high + buffer)
            atr14 = calc_atr(bars[:k], 14)
            if not atr14:
                continue
            
            buffer = atr14 * 0.15
            
            # Breakout UP
            if bars[k].close > range_high + buffer:
                breakout_idx = k
                break
            
            # Breakout DOWN
            if bars[k].close < range_low - buffer:
                breakout_idx = k
                break
        
        if breakout_idx is None:
            continue
        
        # Step 3: Generate signal on breakout
        breakout_bar = bars[breakout_idx]
        direction = 'LONG' if breakout_bar.close > range_high else 'SHORT'
        
        # Check volume confirmation
        vol_ma = sum(b.volume for b in bars[max(0,breakout_idx-21):breakout_idx]) / 20
        vol_confirm = breakout_bar.volume > vol_ma * 1.15
        
        if not vol_confirm:
            continue
        
        signals.append({
            'idx': breakout_idx,
            'date': breakout_bar.date,
            'close': breakout_bar.close,
            'direction': direction,
            'exhaustion_date': exhaustion_date,
            'exhaustion_drop': cum_drop,
            'range_bars': range_bars,
            'range_high': range_high,
            'range_low': range_low
        })
    
    return signals

def simulate_trade(signal, bars, max_hold, stop_atr):
    i = signal['idx']
    entry = signal['close']
    direction = signal['direction']
    
    slip = entry * SLIPPAGE_BPS / 10000
    fee = (entry + slip) * TAKER_FEE
    entry_act = entry + slip
    
    exit_idx = min(i + max_hold, len(bars) - 1)
    reason = 'TIME'
    mae = 0; mfe = 0
    atr14 = calc_atr(bars[:i], 14) or entry * 0.05
    
    for j in range(i+1, exit_idx+1):
        if direction == 'LONG':
            dd = (bars[j].low - entry_act) / entry_act
            rally = (bars[j].high - entry_act) / entry_act
        else:  # SHORT
            dd = (entry_act - bars[j].high) / entry_act
            rally = (entry_act - bars[j].low) / entry_act
        
        if dd < mae: mae = dd
        if rally > mfe: mfe = rally
        
        if stop_atr > 0:
            if direction == 'LONG':
                stop = entry_act - stop_atr * atr14
                if bars[j].low <= stop:
                    exit_idx = j; reason = 'STOP'; break
            else:
                stop = entry_act + stop_atr * atr14
                if bars[j].high >= stop:
                    exit_idx = j; reason = 'STOP'; break
    
    exit_p = bars[exit_idx].close
    slip2 = exit_p * SLIPPAGE_BPS / 10000
    fee2 = (exit_p - slip2) * TAKER_FEE
    
    if direction == 'LONG':
        pnl = ((exit_p - slip2) - entry_act) / entry_act * 100
    else:
        pnl = (entry_act - (exit_p + slip2)) / entry_act * 100
    
    pnl -= (fee + fee2) / entry * 100
    
    return {
        'entry_date': signal['date'],
        'exit_date': bars[exit_idx].date,
        'direction': direction,
        'entry_price': entry,
        'exit_price': exit_p,
        'pnl_net': pnl,
        'fees': (fee+fee2)/entry*100,
        'holding': exit_idx - i,
        'mae': mae*100,
        'mfe': mfe*100,
        'exit_reason': reason,
        'exhaustion_date': signal['exhaustion_date'],
        'exhaustion_drop': signal['exhaustion_drop'],
        'range_bars': signal['range_bars']
    }

def calculate_metrics(trades):
    if not trades: return None
    n = len(trades)
    pnls = [t['pnl_net'] for t in trades]
    wins = [p for p in pnls if p > 0]
    losses = [p for p in pnls if p <= 0]
    
    avg = lambda a: sum(a)/len(a) if a else 0
    std = lambda a: (sum((x-avg(a))**2 for x in a)/(len(a)-1))**0.5 if len(a)>1 else 0
    
    wr = len(wins)/n*100
    avg_win = avg(wins)
    avg_loss = avg(losses)
    
    eq=100; pk=100; mdd=0
    for p in pnls:
        eq*=1+p/100
        if eq>pk: pk=eq
        dd=(pk-eq)/pk*100
        if dd>mdd: mdd=dd
    
    gp=sum(wins); gl=sum(abs(l) for l in losses)
    pf = gp/gl if gl>0 else float('inf')
    
    return {
        'n': n,
        'win_rate': wr,
        'avg_pnl': avg(pnls),
        'avg_win': avg_win,
        'avg_loss': avg_loss,
        'max_dd': mdd,
        'profit_factor': pf,
        'total_return': (eq/100-1)*100,
        'avg_hold': avg([t['holding'] for t in trades]),
        'avg_mae': avg([t['mae'] for t in trades]),
        'avg_mfe': avg([t['mfe'] for t in trades])
    }

def run_backtest():
    print("=" * 80)
    print("V2 BREAKOUT STRATEGY — Multi-Bar Exhaustion > Range > Breakout")
    print("=" * 80)
    print()
    
    # Fetch data
    print("Fetching daily bars...")
    bars = fetch_bars('1d')
    print(f"Got {len(bars)} daily bars")
    
    # Detect signals
    print("\nDetecting V2 breakout signals...")
    signals = detect_v2_breakouts(bars)
    print(f"Found {len(signals)} signals")
    
    # Test different configs
    configs = [
        {'max_hold': 30, 'stop_atr': 0, 'label': '30d, no stop'},
        {'max_hold': 30, 'stop_atr': 2.0, 'label': '30d, 2x ATR stop'},
        {'max_hold': 30, 'stop_atr': 3.0, 'label': '30d, 3x ATR stop'},
        {'max_hold': 60, 'stop_atr': 0, 'label': '60d, no stop'},
        {'max_hold': 60, 'stop_atr': 2.0, 'label': '60d, 2x ATR stop'},
        {'max_hold': 60, 'stop_atr': 3.0, 'label': '60d, 3x ATR stop'},
    ]
    
    print("\n" + "=" * 80)
    print("RESULTS")
    print("=" * 80)
    
    for cfg in configs:
        trades = [simulate_trade(s, bars, cfg['max_hold'], cfg['stop_atr']) for s in signals]
        m = calculate_metrics(trades)
        
        if m:
            pf = 'INF' if m['profit_factor'] == float('inf') else f"{m['profit_factor']:.2f}"
            print(f"\n{cfg['label']}:")
            print(f"  Trades: {m['n']}, Win: {m['win_rate']:.1f}%, PnL: {m['avg_pnl']:.2f}%")
            print(f"  PF: {pf}, MaxDD: {m['max_dd']:.1f}%, Return: {m['total_return']:.1f}%")
            print(f"  Avg Win: {m['avg_win']:.2f}%, Avg Loss: {m['avg_loss']:.2f}%")
            print(f"  Avg Hold: {m['avg_hold']:.1f}d, MAE: {m['avg_mae']:.1f}%, MFE: {m['avg_mfe']:.1f}%")
    
    # Show all signals
    print("\n" + "=" * 80)
    print("ALL SIGNALS")
    print("=" * 80)
    
    for s in signals:
        print(f"\n{s['date']} ({s['direction']}) @ ${s['close']:.0f}")
        print(f"  Exhaustion: {s['exhaustion_date']} ({s['exhaustion_drop']:.1f}% drop)")
        print(f"  Range: {s['range_bars']} bars, High: ${s['range_high']:.0f}, Low: ${s['range_low']:.0f}")

if __name__ == '__main__':
    run_backtest()
