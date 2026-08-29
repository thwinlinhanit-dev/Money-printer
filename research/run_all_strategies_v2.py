#!/usr/bin/env python3
"""
Backtest All Strategy Variations — Money Printer Research Lab (v2 FIXED)

Tests strategy combinations across V1-V6 patterns on daily and weekly.
Fixes: Sharpe/CAGR/Sortino, trend filter, walk-forward, min trades, dedup.
"""

import json
import math
import sys
from datetime import datetime
from pathlib import Path
from dataclasses import dataclass, asdict, field
from typing import Optional, List, Dict, Any
import urllib.request

TAKER_FEE = 0.0004
SLIPPAGE_BPS = 5
MIN_TRADES = 10  # Minimum for statistical significance
IS_RATIO = 0.60  # 60% in-sample, 40% out-of-sample

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
    in_uptrend: bool = True  # Will be computed

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
    sortino: Optional[float]  # None if no losing trades
    max_dd: float
    profit_factor: float  # Capped at 999.99
    total_return: float
    cagr: float
    avg_hold: float
    avg_mae: float
    avg_mfe: float
    calendar_years: float  # Span of the backtest
    trades_per_year: float

def fetch_bars(interval, start_date='2017-08-17'):
    """Fetch bars from Binance API."""
    all_bars = []
    start_time = int(datetime.strptime(start_date, '%Y-%m-%d').timestamp() * 1000)
    end_time = int(datetime.now().timestamp() * 1000)
    increment = 604800000 if interval == '1w' else 86400000
    
    while start_time < end_time:
        url = f'https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval={interval}&startTime={start_time}&limit=1000'
        try:
            with urllib.request.urlopen(url) as response:
                data = json.loads(response.read())
        except Exception:
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
    """Calculate SMA(n) of close prices."""
    if len(closes) < n:
        return None
    return sum(closes[-n:]) / n

def detect_signals(bars, tf):
    """Detect all pattern signals with proper filters."""
    signals = []
    is_weekly = tf == '1w'
    lookback = 21 if is_weekly else 20
    sma_period = 20 if is_weekly else 50  # SMA for trend filter
    
    # Precompute SMA
    closes = [b.close for b in bars]
    
    for i in range(max(30, sma_period), len(bars) - (12 if is_weekly else 30)):
        w = bars[:i+1]
        c = bars[i]
        
        atr14 = calc_atr(w[:-1], 14)
        if not atr14:
            continue
        
        # Volume metrics
        volMA = sum(b.volume for b in w[-lookback-1:-1]) / lookback
        volR = c.volume / volMA if volMA > 0 else 0
        spread = c.high - c.low
        cp = (c.close - c.low) / spread if spread > 0 else 0.5
        volHi = c.volume == max(b.volume for b in w[-lookback-1:])
        wide = spread > atr14 * 1.5
        
        # Down streak
        ds = 0
        for j in range(i, max(0, i-10), -1):
            if j == i or bars[j].close < bars[j+1].close:
                ds += 1
            else:
                break
        cumDrop = (c.close - bars[i-ds].close) / bars[i-ds].close * 100 if ds > 0 else 0
        
        # Body fraction
        body = abs(c.close - c.open)
        bf = body / spread if spread > 0 else 1
        lw = max(0, min(c.open, c.close) - c.low) / spread if spread > 0 else 0
        uw = max(0, c.high - max(c.open, c.close)) / spread if spread > 0 else 0
        
        # Trend filter: close > SMA
        sma = calc_sma(closes[:i+1], sma_period)
        in_uptrend = c.close > sma if sma else True
        
        sig = Signal(
            idx=i, date=c.date, close=c.close, high=c.high, low=c.low,
            pattern='', vol_ratio=volR, close_pos=cp,
            down_streak=ds, cum_drop=cumDrop, in_uptrend=in_uptrend
        )
        
        # V1: Volume exhaustion (relaxed close position)
        if volR > 2.5 and wide and volHi and 0.30 < cp < 0.70:
            signals.append(Signal(
                idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low,
                pattern='V1', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos,
                down_streak=sig.down_streak, cum_drop=sig.cum_drop, in_uptrend=sig.in_uptrend
            ))
        
        # V2: Multi-bar exhaustion
        if ds >= 3 and volR > 1.5 and cumDrop < -10:
            signals.append(Signal(
                idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low,
                pattern='V2', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos,
                down_streak=sig.down_streak, cum_drop=sig.cum_drop, in_uptrend=sig.in_uptrend
            ))
        
        # V4: Volume divergence (new low + declining volume)
        lows10 = [b.low for b in w[-11:-1]]
        if c.low <= min(lows10) and volR < 0.8:
            signals.append(Signal(
                idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low,
                pattern='V4', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos,
                down_streak=sig.down_streak, cum_drop=sig.cum_drop, in_uptrend=sig.in_uptrend
            ))
        
        # V5: Absorption bar (long wick, small body)
        if spread > atr14 * 1.2 and bf < 0.20 and max(lw, uw) > 0.50:
            signals.append(Signal(
                idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low,
                pattern='V5', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos,
                down_streak=sig.down_streak, cum_drop=sig.cum_drop, in_uptrend=sig.in_uptrend
            ))
        
        # V6: Vol expansion (ATR jump)
        if i >= 19:
            prev_atr = calc_atr(bars[:i-4], 14)
            if prev_atr and prev_atr > 0 and atr14 / prev_atr > 1.8:
                signals.append(Signal(
                    idx=sig.idx, date=sig.date, close=sig.close, high=sig.high, low=sig.low,
                    pattern='V6', vol_ratio=sig.vol_ratio, close_pos=sig.close_pos,
                    down_streak=sig.down_streak, cum_drop=sig.cum_drop, in_uptrend=sig.in_uptrend
                ))
    
    return signals

def apply_filters(signals, cfg):
    """Apply trend and close position filters to signals."""
    filtered = signals
    
    # Trend filter
    if cfg.get('trend_filter', False):
        filtered = [s for s in filtered if s.in_uptrend]
    
    # Close position filter
    cp_filter = cfg.get('cp_filter', None)
    if cp_filter == 'low':
        filtered = [s for s in filtered if s.close_pos < 0.40]
    elif cp_filter == 'high':
        filtered = [s for s in filtered if s.close_pos > 0.60]
    
    # Min streak for V2
    min_streak = cfg.get('min_streak', 0)
    if min_streak > 0:
        filtered = [s for s in filtered if s.down_streak >= min_streak]
    
    return filtered

def simulate(signal, bars, cfg):
    """Simulate a single trade with stop loss and take profit."""
    i = signal.idx
    entry = signal.close
    slip = entry * SLIPPAGE_BPS / 10000
    fee = (entry + slip) * TAKER_FEE
    entry_act = entry + slip
    
    exit_idx = min(i + cfg['max_hold'], len(bars) - 1)
    reason = 'TIME'
    mae = 0
    mfe = 0
    atr14 = calc_atr(bars[:i], 14) or entry * 0.05
    
    for j in range(i + 1, exit_idx + 1):
        dd = (bars[j].low - entry_act) / entry_act
        rally = (bars[j].high - entry_act) / entry_act
        if dd < mae:
            mae = dd
        if rally > mfe:
            mfe = rally
        
        if cfg['stop_atr'] > 0:
            stop = entry_act - cfg['stop_atr'] * atr14
            if bars[j].low <= stop:
                exit_idx = j
                reason = 'STOP'
                break
        
        if cfg['tp_pct'] > 0:
            tp = entry_act * (1 + cfg['tp_pct'] / 100)
            if bars[j].high >= tp:
                exit_idx = j
                reason = 'TP'
                break
    
    exit_p = bars[exit_idx].close
    slip2 = exit_p * SLIPPAGE_BPS / 10000
    fee2 = (exit_p - slip2) * TAKER_FEE
    pnl = ((exit_p - slip2) - entry_act) / entry_act * 100 - (fee + fee2) / entry * 100
    
    return Trade(
        signal.date, bars[exit_idx].date, entry, exit_p, pnl,
        (fee + fee2) / entry * 100, exit_idx - i, mae * 100, mfe * 100, reason
    )

def metrics(trades: List[Trade]) -> Optional[Metrics]:
    """Compute corrected metrics with calendar-based annualization."""
    if not trades:
        return None
    
    n = len(trades)
    pnls = [t.pnl_net for t in trades]
    wins = [p for p in pnls if p > 0]
    losses = [p for p in pnls if p <= 0]
    
    def avg(a):
        return sum(a) / len(a) if a else 0
    
    def std(a):
        if len(a) < 2:
            return 0
        m = avg(a)
        return math.sqrt(sum((x - m) ** 2 for x in a) / (len(a) - 1))
    
    # Calendar span: first entry to last exit
    first_entry = min(datetime.strptime(t.entry_date, '%Y-%m-%d') for t in trades)
    last_exit = max(datetime.strptime(t.exit_date, '%Y-%m-%d') for t in trades)
    calendar_years = max((last_exit - first_entry).days / 365.25, 0.01)
    trades_per_year = n / calendar_years
    
    # Basic stats
    wr = len(wins) / n * 100
    avg_pnl = avg(pnls)
    
    # Sharpe: annualized by trades_per_year
    pnl_std = std(pnls)
    if pnl_std > 0:
        sharpe = (avg_pnl / pnl_std) * math.sqrt(trades_per_year)
    else:
        sharpe = 0
    
    # Sortino: only downside deviation
    down_returns = [p for p in pnls if p < 0]
    if len(down_returns) > 1 and std(down_returns) > 0:
        sortino = (avg(pnls) / std(down_returns)) * math.sqrt(trades_per_year)
    else:
        sortino = None  # No losing trades or insufficient data
    
    # Max drawdown
    eq = 100.0
    pk = 100.0
    mdd = 0.0
    for p in pnls:
        eq *= 1 + p / 100
        if eq > pk:
            pk = eq
        dd = (pk - eq) / pk * 100
        if dd > mdd:
            mdd = dd
    
    # Profit factor (capped at 999.99)
    gp = sum(wins)
    gl = sum(abs(l) for l in losses)
    pf = min(gp / gl, 999.99) if gl > 0 else 999.99
    
    # CAGR: based on calendar span
    cagr = (eq / 100) ** (1 / calendar_years) - 1 if calendar_years > 0 else 0
    
    return Metrics(
        n=n,
        win_rate=wr,
        avg_pnl=avg_pnl,
        sharpe=sharpe,
        sortino=sortino,
        max_dd=mdd,
        profit_factor=pf,
        total_return=(eq / 100 - 1) * 100,
        cagr=cagr * 100,
        avg_hold=avg([t.holding for t in trades]),
        avg_mae=avg([t.mae for t in trades]),
        avg_mfe=avg([t.mfe for t in trades]),
        calendar_years=calendar_years,
        trades_per_year=trades_per_year
    )

def walk_forward(bars, signals, cfg, is_ratio=IS_RATIO):
    """60/40 in-sample / out-of-sample walk-forward validation."""
    n_bars = len(bars)
    split = int(n_bars * is_ratio)
    
    is_bars = bars[:split]
    oos_bars = bars[split:]
    
    # Signals in each period
    is_signals = [s for s in signals if s.idx < split]
    oos_signals = [s for s in signals if s.idx >= split]
    
    # Adjust indices for OOS
    oos_adjusted = []
    for s in oos_signals:
        oos_adjusted.append(Signal(
            idx=s.idx - split, date=s.date, close=s.close,
            high=s.high, low=s.low, pattern=s.pattern,
            vol_ratio=s.vol_ratio, close_pos=s.close_pos,
            down_streak=s.down_streak, cum_drop=s.cum_drop,
            in_uptrend=s.in_uptrend
        ))
    
    # Simulate IS
    is_trades = [simulate(s, is_bars, cfg) for s in is_signals]
    is_m = metrics(is_trades)
    
    # Simulate OOS
    oos_trades = [simulate(s, oos_bars, cfg) for s in oos_adjusted]
    oos_m = metrics(oos_trades)
    
    return is_m, oos_m

# Strategy configurations
def gen_strategies():
    """Generate all strategy configurations."""
    strategies = []
    
    # V1 strategies
    for hold in [14, 30, 70]:
        for stop in [0, 2.0, 3.0]:
            for tp in [0, 10, 20]:
                for trend in [False, True]:
                    for cp_filter in [None, 'low', 'high']:
                        name = f"V1_h{hold}_s{stop}_tp{tp}"
                        if trend:
                            name += "_trend"
                        if cp_filter:
                            name += f"_{cp_filter}"
                        strategies.append({
                            'name': name, 'pattern': 'V1', 'tf': '1d',
                            'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp,
                            'trend_filter': trend, 'cp_filter': cp_filter
                        })
    
    # V2 strategies
    for hold in [14, 30, 70]:
        for stop in [0, 2.0]:
            for streak in [3, 4, 5]:
                for trend in [False, True]:
                    name = f"V2_h{hold}_s{stop}_ds{streak}"
                    if trend:
                        name += "_trend"
                    strategies.append({
                        'name': name, 'pattern': 'V2', 'tf': '1d',
                        'max_hold': hold, 'stop_atr': stop, 'tp_pct': 0,
                        'min_streak': streak, 'trend_filter': trend
                    })
    
    # V4 strategies
    for hold in [7, 14, 20]:
        for stop in [0, 3.0, 5.0]:
            for tp in [0, 5, 10]:
                name = f"V4_h{hold}_s{stop}_tp{tp}"
                strategies.append({
                    'name': name, 'pattern': 'V4', 'tf': '1d',
                    'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp
                })
    
    # V5 strategies
    for hold in [14, 30, 70]:
        for stop in [0, 2.0, 3.0]:
            for tp in [0, 10, 20]:
                for trend in [False, True]:
                    name = f"V5_h{hold}_s{stop}_tp{tp}"
                    if trend:
                        name += "_trend"
                    strategies.append({
                        'name': name, 'pattern': 'V5', 'tf': '1d',
                        'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp,
                        'trend_filter': trend
                    })
    
    # V6 strategies
    for hold in [14, 30, 70]:
        for stop in [0, 2.0, 3.0, 5.0]:
            for tp in [0, 20, 30]:
                name = f"V6_h{hold}_s{stop}_tp{tp}"
                strategies.append({
                    'name': name, 'pattern': 'V6', 'tf': '1d',
                    'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp
                })
    
    # Weekly versions (all patterns)
    for pat in ['V1', 'V2', 'V4', 'V5', 'V6']:
        for hold in [4, 8, 12]:
            for stop in [0, 2.0, 3.0]:
                for tp in [0, 10, 20]:
                    for trend in [False, True]:
                        name = f"{pat}_w{hold}_s{stop}_tp{tp}"
                        if trend:
                            name += "_trend"
                        strategies.append({
                            'name': name, 'pattern': pat, 'tf': '1w',
                            'max_hold': hold, 'stop_atr': stop, 'tp_pct': tp,
                            'trend_filter': trend
                        })
    
    return strategies

def deduplicate(results):
    """Remove duplicate results (same metrics, different configs)."""
    seen = set()
    unique = []
    for r in results:
        key = (r['pattern'], r['tf'], r['n'], round(r['win_rate'], 1),
               round(r['avg_pnl'], 2), round(r['total_return'], 1))
        if key not in seen:
            seen.add(key)
            unique.append(r)
    return unique

def run_backtest():
    """Main backtest runner."""
    print("Fetching data...")
    daily = fetch_bars('1d')
    weekly = fetch_bars('1w')
    print(f"Daily: {len(daily)} bars, Weekly: {len(weekly)} bars")
    
    daily_sigs = detect_signals(daily, '1d')
    weekly_sigs = detect_signals(weekly, '1w')
    print(f"Daily signals: {len(daily_sigs)}, Weekly: {len(weekly_sigs)}")
    
    strategies = gen_strategies()
    print(f"\nTesting {len(strategies)} strategies...\n")
    
    results = []
    for cfg in strategies:
        bars = daily if cfg['tf'] == '1d' else weekly
        all_sigs = daily_sigs if cfg['tf'] == '1d' else weekly_sigs
        pat_sigs = [s for s in all_sigs if s.pattern == cfg['pattern']]
        
        # Apply filters
        filtered = apply_filters(pat_sigs, cfg)
        
        if not filtered:
            continue
        
        # Simulate all trades
        trades = [simulate(s, bars, cfg) for s in filtered]
        m = metrics(trades)
        
        if m and m.n >= MIN_TRADES:
            # Walk-forward validation
            is_m, oos_m = walk_forward(bars, filtered, cfg)
            
            # Compute degradation
            if is_m and is_m.sharpe > 0 and oos_m:
                degradation = ((is_m.sharpe - oos_m.sharpe) / is_m.sharpe) * 100
            else:
                degradation = None
            
            # Confidence level
            if m.n >= 30:
                confidence = 'high'
            elif m.n >= 20:
                confidence = 'medium'
            else:
                confidence = 'low'
            
            result = {
                'name': cfg['name'],
                'pattern': cfg['pattern'],
                'tf': cfg['tf'],
                'max_hold': cfg['max_hold'],
                'stop_atr': cfg['stop_atr'],
                'tp_pct': cfg['tp_pct'],
                'trend_filter': cfg.get('trend_filter', False),
                'cp_filter': cfg.get('cp_filter'),
                **asdict(m),
                'sharpe_is': is_m.sharpe if is_m else None,
                'sharpe_oos': oos_m.sharpe if oos_m else None,
                'degradation_pct': degradation,
                'confidence': confidence
            }
            results.append(result)
    
    # Deduplicate
    results = deduplicate(results)
    
    # Sort by Sharpe
    results.sort(key=lambda x: x['sharpe'], reverse=True)
    
    # Print top 30
    print("=" * 120)
    print(f"{'Rank':<5} {'Strategy':<35} {'N':<6} {'Win%':<8} {'Sharpe':<8} {'Sharpe_OOS':<10} {'PF':<8} {'MaxDD':<8} {'CAGR':<8} {'Conf':<8}")
    print("=" * 120)
    
    for i, r in enumerate(results[:30], 1):
        pf = f"{r['profit_factor']:.1f}" if r['profit_factor'] < 999 else "999+"
        oos = f"{r['sharpe_oos']:.2f}" if r['sharpe_oos'] is not None else "N/A"
        print(f"{i:<5} {r['name']:<35} {r['n']:<6} {r['win_rate']:<8.1f} {r['sharpe']:<8.2f} {oos:<10} {pf:<8} {r['max_dd']:<8.1f} {r['cagr']:<8.1f} {r['confidence']:<8}")
    
    # Save results
    out_path = Path('out/all_strategies_backtest.json')
    out_path.write_text(json.dumps(results, indent=2, default=str), encoding='utf-8')
    print(f"\nSaved {len(results)} results to {out_path}")
    
    # Print summary by pattern
    print("\n" + "=" * 80)
    print("SUMMARY BY PATTERN")
    print("=" * 80)
    
    for pat in ['V1', 'V2', 'V4', 'V5', 'V6']:
        pat_results = [r for r in results if r['pattern'] == pat]
        if pat_results:
            best = pat_results[0]
            print(f"\n{pat}: {len(pat_results)} strategies tested")
            print(f"  Best: {best['name']}")
            print(f"    N={best['n']}, Win%={best['win_rate']:.1f}, Sharpe={best['sharpe']:.2f}")
            print(f"    Sharpe_OOS={best['sharpe_oos']}, MaxDD={best['max_dd']:.1f}%")
            print(f"    Confidence: {best['confidence']}")

if __name__ == '__main__':
    run_backtest()
