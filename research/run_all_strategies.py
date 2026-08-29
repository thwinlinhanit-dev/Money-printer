#!/usr/bin/env python3
"""
Backtest All Strategy Variations — Money Printer Research Lab

Tests 92+ strategy combinations across V1-V6 patterns on daily and weekly.
"""

import json
import sys
from datetime import datetime
from pathlib import Path
from dataclasses import dataclass, asdict
from typing import Optional
import itertools

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

@dataclass
class Signal:
    idx: int
    date: str
    close: float
    high: float
    low: float
    pattern: str
    vol_ratio: float
    close_pos: float
    down_streak: int = 0
    cum_drop: float = 0.0

@dataclass
class Trade:
    entry_date: str
    exit_date: str
    entry_price: float
    exit_price: float
    pnl_net: float
    fees: float
    holding: int
    mae: float
    mfe: float
    exit_reason: str

@dataclass
class Metrics:
    n: int
    win_rate: float
    avg_pnl: float
    sharpe: float
    sortino: float
    max_dd: float
    profit_factor: float
    total_return: float
    cagr: float
    avg_hold: float
    avg_mae: float
    avg_mfe: float

def fetch_bars(interval, start_date='2017-08-17'):
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
    return all_bars

def calc_atr(bars, n):
    if len(bars) < n: return None
    win = bars[-n:]
    return sum(max(win[i].high-win[i].low, 
                   abs(win[i].high-win[i-1].close) if i>0 else win[i].high-win[i].low,
                   abs(win[i].low-win[i-1].close) if i>0 else 0) for i in range(len(win))) / n

def linreg(y, n, offset):
    if len(y) < n: return y[-1]
    s = y[-n:]
    sx = sum(range(n)); sy = sum(s); sxy = sum(i*s[i] for i in range(n)); sx2 = sum(i*i for i in range(n))
    slope = (n*sxy - sx*sy) / (n*sx2 - sx*sx)
    return (sy - slope*sx)/n + slope*(n-1-offset)

def detect_signals(bars, tf):
    signals = []
    is_weekly = tf == '1w'
    lookback = 21 if is_weekly else 20
    
    for i in range(30, len(bars) - (12 if is_weekly else 30)):
        w = bars[:i+1]; c = bars[i]
        atr14 = calc_atr(w[:-1], 14)
        if not atr14: continue
        
        volMA = sum(b.volume for b in w[-lookback-1:-1]) / lookback
        volR = c.volume / volMA if volMA > 0 else 0
        spread = c.high - c.low
        cp = (c.close - c.low) / spread if spread > 0 else 0.5
        volHi = c.volume == max(b.volume for b in w[-lookback-1:])
        wide = spread > atr14 * 1.5
        
        ds = 0
        for j in range(i, max(0,i-10), -1):
            if j==i or bars[j].close < bars[j+1].close: ds+=1
            else: break
        cumDrop = (c.close - bars[i-ds].close) / bars[i-ds].close * 100 if ds > 0 else 0
        
        body = abs(c.close - c.open)
        bf = body / spread if spread > 0 else 1
        lw = max(0, min(c.open,c.close) - c.low) / spread if spread > 0 else 0
        uw = max(0, c.high - max(c.open,c.close)) / spread if spread > 0 else 0
        
        sig = Signal(idx=i, date=c.date, close=c.close, high=c.high, low=c.low,
                     pattern='', vol_ratio=volR, close_pos=cp, down_streak=ds, cum_drop=cumDrop)
        
        if volR > 2.5 and wide and volHi and 0.30 < cp < 0.70:
            signals.append(Signal(idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low, pattern='V1', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos, down_streak=sig.down_streak, cum_drop=sig.cum_drop))
        if ds >= 3 and volR > 1.5 and cumDrop < -10:
            signals.append(Signal(idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low, pattern='V2', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos, down_streak=sig.down_streak, cum_drop=sig.cum_drop))
        lows10 = [b.low for b in w[-11:-1]]
        if c.low <= min(lows10) and volR < 0.8:
            signals.append(Signal(idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low, pattern='V4', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos, down_streak=sig.down_streak, cum_drop=sig.cum_drop))
        if spread > atr14*1.2 and bf < 0.20 and max(lw,uw) > 0.50:
            signals.append(Signal(idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low, pattern='V5', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos, down_streak=sig.down_streak, cum_drop=sig.cum_drop))
        if i >= 19:
            prev = calc_atr(bars[:i-4], 14)
            if prev and prev > 0 and atr14/prev > 1.8:
                signals.append(Signal(idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low, pattern='V6', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos, down_streak=sig.down_streak, cum_drop=sig.cum_drop))
    return signals

def simulate(signal, bars, cfg):
    i = signal.idx; entry = signal.close
    slip = entry * SLIPPAGE_BPS / 10000
    fee = (entry + slip) * TAKER_FEE
    entry_act = entry + slip
    
    exit_idx = min(i + cfg['max_hold'], len(bars) - 1)
    reason = 'TIME'; mae = 0; mfe = 0
    atr14 = calc_atr(bars[:i], 14) or entry * 0.05
    
    for j in range(i+1, exit_idx+1):
        dd = (bars[j].low - entry_act) / entry_act
        rally = (bars[j].high - entry_act) / entry_act
        if dd < mae: mae = dd
        if rally > mfe: mfe = rally
        if cfg['stop_atr'] > 0:
            stop = entry_act - cfg['stop_atr'] * atr14
            if bars[j].low <= stop:
                exit_idx = j; reason = 'STOP'; break
        if cfg['tp_pct'] > 0:
            tp = entry_act * (1 + cfg['tp_pct']/100)
            if bars[j].high >= tp:
                exit_idx = j; reason = 'TP'; break
    
    exit_p = bars[exit_idx].close
    slip2 = exit_p * SLIPPAGE_BPS / 10000
    fee2 = (exit_p - slip2) * TAKER_FEE
    pnl = ((exit_p - slip2) - entry_act) / entry_act * 100 - (fee+fee2)/entry*100
    
    return Trade(signal.date, bars[exit_idx].date, entry, exit_p, pnl,
                 (fee+fee2)/entry*100, exit_idx-i, mae*100, mfe*100, reason)

def metrics(trades):
    if not trades: return None
    n = len(trades)
    pnls = [t.pnl_net for t in trades]
    wins = [p for p in pnls if p > 0]; losses = [p for p in pnls if p <= 0]
    avg = lambda a: sum(a)/len(a) if a else 0
    std = lambda a: (sum((x-avg(a))**2 for x in a)/(len(a)-1))**0.5 if len(a)>1 else 0
    
    mret = avg(pnls)/100
    wr = len(wins)/n*100
    sharpe = mret/std(pnls)/100*(52**0.5) if std(pnls)>0 else 0
    down = [p/100 for p in pnls if p<0]
    sortino = mret/std(down)*(52**0.5) if len(down)>1 and std(down)>0 else 0
    
    eq=100; pk=100; mdd=0
    for p in pnls:
        eq*=1+p/100
        if eq>pk: pk=eq
        dd=(pk-eq)/pk*100
        if dd>mdd: mdd=dd
    
    gp=sum(wins); gl=sum(abs(l) for l in losses)
    pf = gp/gl if gl>0 else float('inf')
    years = sum(t.holding for t in trades)/365
    cagr = (eq/100)**(1/years)-1 if years>0 else 0
    
    return Metrics(n, wr, avg(pnls), sharpe, sortino, mdd, pf, (eq/100-1)*100, cagr*100,
                   avg([t.holding for t in trades]), avg([t.mae for t in trades]), avg([t.mfe for t in trades]))

# Strategy configurations
def gen_strategies():
    strategies = []
    
    # V1 strategies
    for hold in [70]:
        for stop in [0, 2.0, 3.0]:
            for tp in [0, 10, 20, 30]:
                for trend in [False, True]:
                    for cp_filter in [None, 'low', 'high']:
                        name = f"V1_h{hold}_s{stop}_tp{tp}"
                        if trend: name += "_trend"
                        if cp_filter: name += f"_{cp_filter}"
                        strategies.append({
                            'name': name, 'pattern': 'V1', 'tf': '1d',
                            'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp,
                            'trend_filter': trend, 'cp_filter': cp_filter
                        })
    
    # V2 strategies
    for hold in [70]:
        for stop in [0, 2.0]:
            for streak in [3, 4, 5]:
                name = f"V2_h{hold}_s{stop}_ds{streak}"
                strategies.append({
                    'name': name, 'pattern': 'V2', 'tf': '1d',
                    'max_hold': hold, 'stop_atr': stop, 'tp_pct': 0,
                    'min_streak': streak
                })
    
    # V4 strategies
    for hold in [20, 14, 7]:
        for stop in [0, 3.0, 5.0]:
            for tp in [0, 5, 10]:
                name = f"V4_h{hold}_s{stop}_tp{tp}"
                strategies.append({
                    'name': name, 'pattern': 'V4', 'tf': '1d',
                    'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp
                })
    
    # V5 strategies
    for hold in [40, 70]:
        for stop in [0, 2.0, 3.0]:
            for tp in [0, 10, 20, 30]:
                for trend in [False, True]:
                    name = f"V5_h{hold}_s{stop}_tp{tp}"
                    if trend: name += "_trend"
                    strategies.append({
                        'name': name, 'pattern': 'V5', 'tf': '1d',
                        'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp,
                        'trend_filter': trend
                    })
    
    # V6 strategies
    for hold in [70]:
        for stop in [0, 2.0, 3.0, 5.0]:
            for tp in [0, 20, 30, 50]:
                name = f"V6_h{hold}_s{stop}_tp{tp}"
                strategies.append({
                    'name': name, 'pattern': 'V6', 'tf': '1d',
                    'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp
                })
    
    # Weekly versions
    for pat in ['V1', 'V4', 'V5', 'V6']:
        for hold in [8]:
            for stop in [0, 2.0, 3.0]:
                for tp in [0, 10, 20]:
                    name = f"{pat}_w{hold}_s{stop}_tp{tp}"
                    strategies.append({
                        'name': name, 'pattern': pat, 'tf': '1w',
                        'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp
                    })
    
    return strategies

def run_backtest():
    print("Fetching data...")
    daily = fetch_bars('1d')
    weekly = fetch_bars('1w')
    print(f"Daily: {len(daily)}, Weekly: {len(weekly)}")
    
    daily_sigs = detect_signals(daily, '1d')
    weekly_sigs = detect_signals(weekly, '1w')
    print(f"Daily signals: {len(daily_sigs)}, Weekly: {len(weekly_sigs)}")
    
    strategies = gen_strategies()
    print(f"\nTesting {len(strategies)} strategies...\n")
    
    results = []
    for cfg in strategies:
        bars = daily if cfg['tf'] == '1d' else weekly
        sigs = daily_sigs if cfg['tf'] == '1d' else weekly_sigs
        pat_sigs = [s for s in sigs if s.pattern == cfg['pattern']]
        
        if not pat_sigs:
            continue
        
        trades = [simulate(s, bars, cfg) for s in pat_sigs]
        m = metrics(trades)
        if m and m.n >= 3:
            results.append({
                'name': cfg['name'],
                'pattern': cfg['pattern'],
                'tf': cfg['tf'],
                'max_hold': cfg['max_hold'],
                'stop_atr': cfg['stop_atr'],
                'tp_pct': cfg['tp_pct'],
                **asdict(m)
            })
    
    # Sort by Sharpe
    results.sort(key=lambda x: x['sharpe'], reverse=True)
    
    print("=" * 100)
    print(f"{'Rank':<5} {'Strategy':<35} {'Trades':<8} {'Win%':<8} {'Sharpe':<10} {'PF':<10} {'MaxDD':<10} {'CAGR':<10}")
    print("=" * 100)
    
    for i, r in enumerate(results[:50], 1):
        pf = 'INF' if r['profit_factor'] == float('inf') else f"{r['profit_factor']:.2f}"
        print(f"{i:<5} {r['name']:<35} {r['n']:<8} {r['win_rate']:<8.1f} {r['sharpe']:<10.2f} {pf:<10} {r['max_dd']:<10.1f} {r['cagr']:<10.1f}")
    
    # Save results
    out_path = Path('out/all_strategies_backtest.json')
    out_path.write_text(json.dumps(results, indent=2), encoding='utf-8')
    print(f"\nSaved {len(results)} results to {out_path}")

if __name__ == '__main__':
    run_backtest()
