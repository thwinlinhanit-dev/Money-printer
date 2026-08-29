# Volume Pattern Pine Script Strategies

Based on corrected backtest research (walk-forward validated, minimum 10 trades).

## Strategies Overview

### Core Strategies

| Strategy | File | Best TF | Trades | Win% | Sharpe | PF | Purpose |
|----------|------|---------|--------|------|--------|-----|---------|
| **V5 Absorption Bar** | `v5_absorption_strategy.pine` | Weekly | 14 | 64% | 1.81 | 6.39 | Best performer |
| **V1 Volume Exhaustion** | `v1_volume_exhaustion_strategy.pine` | Weekly | 21 | 67% | 1.20 | 2.10 | Catches missed moves |
| **V6 Vol Expansion** | `v6_vol_expansion_strategy.pine` | Weekly | 5 | 100% | 16.01 | ∞ | Strongest but rare |
| **V2 Range Breakout** | `v2_range_breakout_strategy.pine` | Daily | 129 | 52% | 0.32 | 2.96 | Most tradeable |
| **V1V5 Composite** | `v1v5_composite_strategy.pine` | Weekly | 5 | 100% | 41 | ∞ | Sniper entries |

### Position Sizing Variants

| Strategy | File | Sizing Method | Purpose |
|----------|------|---------------|---------|
| **V5 Kelly** | `v5_kelly_strategy.pine` | Kelly Criterion | Optimal growth rate |
| **V5 Vol-Adjusted** | `v5_vol_adjusted_strategy.pine` | Inverse Volatility | Consistent risk per trade |

### Multi-Timeframe Portfolios

| Strategy | File | Timeframes | Purpose |
|----------|------|------------|---------|
| **V5V2 Portfolio** | `v5v2_multitimeframe_portfolio.pine` | Weekly + Daily | Uncorrelated signals |

## Key Findings

### Weekly vs Daily
- **Weekly is 4-42× better** than daily for all patterns
- Less noise, higher win rates, larger moves

### Trend Filter is THE Edge Multiplier
- V1 without trend: 52.8% win → V1 with uptrend: **90% win** (+37%)
- V5 without trend: 50% win → V5 with uptrend: **77.8% win** (+28%)
- Every pattern improves with trend alignment

### Pattern Characteristics

**V5 (Absorption Bar):**
- Long wick + small body
- Buyers/sellers absorbing opposite pressure
- Most reliable pattern (14 signals)

**V1 (Volume Exhaustion):**
- High volume + wide bar + mid close
- Missed by standard detector
- Catches continuation signals

**V6 (Vol Expansion):**
- ATR jumps > 1.8× long ATR
- Marks trend starts
- 100% win rate but rare (5 signals)

**V2 (Range Breakout):**
- Multi-bar selling → range → breakout
- 94% form ranges after exhaustion
- 64% break UP

**V1V5 Composite:**
- Both patterns fire together
- 100% win rate (5 trades)
- Extremely selective

## Usage

### Installation
1. Open TradingView
2. Go to Pine Editor
3. Paste the strategy code
4. Click "Add to Chart"
5. Configure inputs as needed

### Recommended Settings

**For Weekly (BTCUSDT):**
- V5: wickRatio=2.0, bodyPctMax=0.15, trailATR=2.0, smaLen=20
- V1: volMult=2.0, spreadMult=1.5, trailATR=2.0, smaLen=20
- V6: expansionThresh=1.8, trailATR=3.0, smaLen=20

**For Daily (BTCUSDT):**
- V2: minStreak=3, containWin=15, trailATR=1.5, smaLen=50

### Risk Management
- **Position size:** 2-3% per trade
- **Max exposure:** 15% across all strategies
- **Drawdown limit:** 10% → reduce positions 50%

## Important Notes

### Statistical Honesty
- V6 and V1V5 Composite have <10 trades → exploratory, not proven
- V5 and V2 have 14-129 trades → statistically significant
- Always use walk-forward validation

### Backtest Limitations
- No slippage modeling beyond fixed 5 bps
- No market impact
- assumes perfect execution
- Past performance ≠ future results

### When to Skip
- **V3 (Squeeze):** Too rare, random direction
- **V4 Daily:** No edge (PF < 1.25)
- **V6 without trend:** 50% win rate, coin flip

## Position Sizing Explained

### Kelly Criterion (`v5_kelly_strategy.pine`)

The Kelly Criterion determines the optimal bet size to maximize long-term growth:

```
f* = (W × B - L) / B

Where:
  f* = Kelly fraction (optimal % of equity)
  W   = Win rate (default: 64%)
  B   = Avg win / Avg loss (default: 20.7% / 8.5% = 2.44)
  L   = 1 - W (loss rate)
```

**Parameters:**
- `kellyWinRate`: Your observed win rate (default: 0.64)
- `kellyAvgWin`: Average winning trade return (default: 0.207)
- `kellyAvgLoss`: Average losing trade return (default: 0.085)
- `kellyFrac`: Fraction of full Kelly (default: 0.25 = quarter-Kelly)

**Why quarter-Kelly?**
- Full Kelly is aggressive and assumes perfect estimates
- Half-Kelly (0.5) is common for professional traders
- Quarter-Kelly (0.25) is conservative and accounts for estimation error

**Example:**
- Full Kelly: f* = (0.64 × 2.44 - 0.36) / 2.44 = 49.5%
- Quarter-Kelly: 49.5% × 0.25 = 12.4% per trade
- Clamped to 5% max = 5% position size

---

### Volatility-Adjusted (`v5_vol_adjusted_strategy.pine`)

Sizes positions inversely to volatility, maintaining consistent risk:

```
position_size = (target_risk / atr%) × (1 / vol_ratio)

Where:
  target_risk = Desired risk per trade (default: 2%)
  atr%        = ATR as % of price
  vol_ratio   = current_ATR / median_ATR
```

**Parameters:**
- `targetRisk`: Risk per trade in % (default: 2.0%)
- `atrLookback`: Period for ATR percentile (default: 100)
- `maxPosPct`: Maximum position size (default: 5%)

**How it works:**
- Low volatility (ratio < 1): Larger position (more risk-efficient)
- High volatility (ratio > 1): Smaller position (less risk)
- Same dollar risk regardless of volatility regime

**Example:**
- BTC ATR: $2,000 (10% of price)
- Median ATR: $3,000
- Vol ratio: 0.67 (low vol)
- Base size: 2% / 10% = 20%
- Adjusted: 20% / 0.67 = 30%
- Clamped to 5% max = 5% position size

---

### Which to Use?

| Method | Best For | Pros | Cons |
|--------|----------|------|------|
| **Kelly** | Maximizing long-term growth | Theoretically optimal | Requires accurate estimates |
| **Vol-Adjusted** | Consistent risk management | Adapts to market conditions | Doesn't maximize growth |
| **Fixed** | Simple backtesting | Easy to understand | Ignores edge and volatility |

**Recommendation:** Start with Vol-Adjusted for safety, then move to Kelly when you have 50+ trades to validate your win rate.

## Multi-Timeframe Portfolio

### V5V2 Portfolio (`v5v2_multitimeframe_portfolio.pine`)

Combines two uncorrelated signals with dynamic sizing:

**Components:**
- **V5 Weekly**: Swing trades (8-12 week hold, 60% allocation)
- **V2 Daily**: Range breakouts (21 day hold, 40% allocation)

**Correlation-Aware Sizing:**
```
When V5 and V2 fire together:
  - High correlation (>0.7): Reduce total size by 50%
  - Low correlation (<0.7): Full size
  
When only one signal fires:
  - Use 100% allocation for that strategy
```

**Why Correlation Matters:**
- If V5 and V2 are highly correlated, they're essentially the same bet
- Reducing size when correlated limits drawdown
- Uncorrelated signals provide diversification

**Example Scenario:**
```
V5 fires: 60% allocation → $60K position
V2 fires: 40% allocation → $40K position
Correlation: 0.8 (high)

Adjusted:
  V5: $60K × 0.5 = $30K
  V2: $40K × 0.5 = $20K
  Total: $50K (50% of equity)
```

**When Both Fire:**
- Yellow diamond appears on chart
- Allocations are automatically reduced
- Risk is controlled regardless of correlation

---

## Files

```
strategies/pine/
├── README.md                           # This file
├── v5_absorption_strategy.pine         # Base strategy (fixed sizing)
├── v5_kelly_strategy.pine              # Kelly Criterion sizing
├── v5_vol_adjusted_strategy.pine       # Volatility-Adjusted sizing
├── v5v2_multitimeframe_portfolio.pine  # Multi-TF portfolio
├── v1_volume_exhaustion_strategy.pine
├── v6_vol_expansion_strategy.pine
├── v2_range_breakout_strategy.pine
└── v1v5_composite_strategy.pine        # Sniper entries
```

## References

- `research/out/all_patterns_characteristics_v2.md` - Corrected backtest results
- `research/run_all_strategies_v2.py` - Fixed backtest engine
- `research/out/all_strategies_backtest.json` - Raw results
