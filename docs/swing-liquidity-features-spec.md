# Spec: Volume Profile / Liquidity Structure Features + Range-Reclaim Swing Strategy

**Status:** Draft — renumber and slot into your existing 000–011 spec sequence; this is written as an addendum to your feature-engine, strategy-API, and risk specs, not a replacement for them.

## 0. Context & Scope

Covers the quantifiable half of the KillaXBT-style confluence framework: HTF volume profile / POC, market structure, and liquidity-sweep reclaim, expressed as deterministic features (`features` crate) and one composable strategy (`strategies` crate) that plugs into your existing risk/promotion pipeline.

**Explicit non-goals — not implemented here, and shouldn't be:**
- Fractal / cycle pattern-matching ("this looks like 2021"). No rigorous stopping rule, high hindsight-bias risk.
- Sentiment extremes, astrology, or any unquantified narrative input.
- The "God Mode" leveraged re-entry ladder. Structurally a martingale — bet that a support level holds within N attempts, sized bigger each time, no bounded invalidation. Cannot pass an EDGE gate that requires a defined max loss per trade (see §6). Not to be implemented as strategy-crate code under any configuration. If you want to run it, do it manually, outside money-printer, with capital you've deliberately walled off from the system.

## 1. Data Requirements

- Needs volume-at-price granularity, not just OHLCV close.
  - Preferred: trade-tick stream aggregated into price buckets. Confirm each venue collector actually emits trade events, not just candle snapshots — this determines which path below you're on.
  - Fallback: OHLCV candles with volume, approximated via uniform/triangular distribution across each bar's high-low range. Tag output `approx: true` whenever this path is used — never blend real tick-derived volume and approximated volume in the same POC calculation.
- Timeframes: Daily primary, 4H for POC-flip confirmation and entry timing (matches your swing pivot).
- Lookback window: configurable. Default 90 daily bars (~1 quarter) for the HTF profile, 20 bars for range detection.

## 2. Feature: Volume Profile / POC / VAH / VAL

### 2.1 Definition
- Bucket width = (window_high − window_low) / N_buckets. N_buckets default 100, or ATR-scaled if you want adaptive resolution.
- POC = price bucket with max cumulative volume in the window.
- Value Area = smallest contiguous set of buckets around POC containing 70% of total window volume (standard convention). VAH/VAL = its upper/lower boundary.
- HVN = local volume maxima above a threshold (default: >70% of POC volume). LVN = local minima below a threshold (default: <30% of POC volume).

### 2.2 Output schema
```rust
struct VolumeProfile {
    window_start: Timestamp,
    window_end: Timestamp,
    timeframe: Timeframe,
    poc_price: Decimal,
    vah_price: Decimal,
    val_price: Decimal,
    hvn_levels: Vec<Decimal>,
    lvn_levels: Vec<Decimal>,
    approx: bool,
}
```

### 2.3 POC Flip State Machine
States: `BelowPoc`, `AbovePoc`, `Flipping`.
Transition rule: N consecutive closes (default N=3, configurable) on the far side of POC, with acceptance defined as no more than 1 close back across POC within that window, confirms a flip. Emits:
```rust
struct PocFlipEvent { direction: Direction, confirmed_at: Timestamp, poc_price: Decimal }
```

### 2.4 Accumulation/Distribution Classifier
Deterministic, rule-based (no ML — keep it backtestable and auditable):
- Accumulation: volume near VAL trending up over the last K windows (default K=3) AND range compressing (ATR(20) declining).
- Distribution: mirror condition at VAH.
- Output: `enum { Accumulating, Distributing, Neutral }` + a ratio-based confidence score in [0,1], not a learned probability.

## 3. Feature: Liquidity Sweep Detector

### 3.1 Range Definition
- A "range" = N bars (default 20 daily) where high-low compression < X% of the trailing ATR(20) average (default X=60%).
- Range high = max(high) in window; range low = min(low).

### 3.2 Sweep Event
- Sweep-high: bar high exceeds range high by ≥ Y·ATR (default Y=0.1).
- Sweep-low: mirror condition.
- Reclaim confirmation: within Z bars (default Z=2), close moves back inside `[range_low, range_high]`.
- Volume filter: sweep-bar volume ≥ 1.5× the window's average volume, to filter out low-liquidity wicks.
```rust
struct LiquiditySweepEvent {
    direction: Direction,
    sweep_price: Decimal,
    range_high: Decimal,
    range_low: Decimal,
    reclaim_bar_index: u32,
    volume_ratio: f64,
}
```

## 4. Strategy: `swing-range-reclaim-v1`

Diff this against `liq-fade-v1` before writing new code — there's likely real overlap; this may just be liq-fade-v1's entry logic made explicit, or a variant worth merging rather than duplicating.

### 4.1 Entry
- Long: `LiquiditySweepEvent(direction=low)` AND reclaim confirmed AND close > range_low AND (optional) `PocFlipEvent(direction=up)` within lookback W bars.
- Short: mirror.
- Session filter: optional, default off pending backtest — restrict entries to NY/London overlap hours if data supports it.

### 4.2 Invalidation / Stop
- Stop = sweep_price − buffer (buffer default 0.5×ATR beyond the sweep extreme).
- Hard invalidation on close beyond stop, evaluated on the entry timeframe only — no intrabar stops on HTF setups, to avoid noise-driven stopouts.

### 4.3 Target / Exit
- Primary target: opposite range boundary or nearest LVN beyond it, whichever is closer (bias toward the more conservative target for realistic backtest expectancy, not the best-case one).
- After target 1: move stop to breakeven, trail the remainder with a 2×ATR trailing stop toward the next LVN/HVN.

### 4.4 Sizing
- Position size = your risk crate's standard formula: `risk_pct_of_equity / (entry − stop)`. Not ladder/martingale sizing — one entry, one defined risk.
- Starting assumption for backtest: 0.5–1% equity risk per trade, configurable. This is a backtest default, not a recommendation.

## 5. Regime Tag (manual input only — not automated)

- Add optional `regime_tag: Option<CycleRegime>` with `enum { EarlyAccumulation, MidCycle, Distribution, Unknown }`, settable via ops/config only — never derived by fractal-matching code.
- Strategies may condition on it if set, but must define behavior for `Unknown` (default: no regime-based filtering), so the system never silently depends on someone keeping a manual tag current.

## 6. EDGE Validation / Promotion Criteria

Align with whatever thresholds your existing promotion pipeline already enforces; proposed defaults if not yet set:
- Minimum out-of-sample trade count before promotion review: 30.
- Required: positive expectancy after fees/slippage, max drawdown within defined risk budget, no single trade > X% of total backtest PnL (guards against promoting on a lucky tail trade).
- Hard gate: strategy must have a bounded, defined max loss per trade at entry time. This single criterion is what permanently disqualifies any ladder/martingale sizing model — there's no version of "God Mode" that can satisfy it.

## 7. Open Assumptions — confirm before merging

- Actual spec numbering and file location in your repo.
- Whether your collectors currently expose trade-level data or only OHLCV — this determines whether §2's `approx: true` fallback path is the one you're actually on.
- `liq-fade-v1`'s existing internals — §3/§4 may already be partially built; diff first, don't duplicate.
