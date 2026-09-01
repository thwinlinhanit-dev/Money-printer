# Zero-Cost Mode

**Status:** Active (2026-08-31)
**Owner constraint:** $0 budget forever — free-tier only.

## Purpose

Adapt the system to run usefully and indefinitely under strict zero-cost
constraints while preserving the core philosophy: one deterministic event
core, signal catalog lifecycle discipline, automatic demotion / human-only
promotion, honest expectancy after costs, and survival first.

## Constraints

| Constraint | Value |
|---|---|
| Budget | $0 forever |
| VPS | Google Cloud e2-micro (1 GB RAM / 30 GB disk) or Oracle Always Free (2 OCPU / 12 GB / 200 GB) |
| Personal PC | Secondary / cold storage only |
| Capital at risk | $0 (PD-1 absolute) |
| Live trading | Forbidden (PD-1) |
| Full L2 book | Deferred indefinitely |
| Multi-symbol expansion | Deferred until storage proven stable for months |
| Options, macro, heavy analytics | Deferred |

## New Phase-0 Definition (Zero-Cost)

**Success** = continuous, usable recording of the minimal viable streams for
BTC + ETH on Hyperliquid that fits in free storage for >= 60 days and feeds
the signal catalog + accumulation detector.

### Required streams (only these)

| Stream | Source | Why |
|---|---|---|
| `trade` | Hyperliquid WS | OHLCV bars, volume, VWAP, CVD, footprint |
| `funding` | Hyperliquid `activeAssetCtx` | Funding regime, carry research |
| `open_interest` | Hyperliquid `activeAssetCtx` | OI regime, accumulation detector |
| `mark_price` | Hyperliquid `activeAssetCtx` | Mark-vs-index basis |

### Optional streams

| Stream | Source | Notes |
|---|---|---|
| `liquidation` | Derived/estimated later | Not available cheaply from Hyperliquid; estimate from whale census (spec 028) |
| BBO / top-of-book (5 levels max) | Hyperliquid `l2Book` | Off by default; behind config flag |

### Explicitly dropped from Phase-0 gate

- Full L2 book (20+ levels) — **not a Phase-0 requirement under Zero-Cost Mode**
- Bybit, Binance, OKX, Coinbase, Kraken streams — **not required for promotion**
- Options, macro, whale census (as required streams — kept as optional context only)
- Any path requiring paid storage or paid compute

### Research context (swing path — optional, recommended)

The swing collector path (`swing_collectors.ps1` / `deploy_swing.sh`) is the
recommended research setup under Zero-Cost Mode. It adds cross-asset breadth
and correlation context beyond the Phase-0 gate:

| Stream | Source | Purpose |
|---|---|---|
| Bybit BTC/ETH/SOL trades | Bybit WS (credential-free) | Cross-venue correlation, breadth |
| Whale positions | Hyperliquid on-chain | Liquidation estimation, whale bands |
| FRED macro | FRED API (if key available) | Regime context |

These are **research data**, not gate-required. The VPS runs both the
Phase-0 Hyperliquid collectors AND the swing collectors. The daily gate
only audits `hyperliquid:BTC` and `hyperliquid:ETH`.

## Storage Budget

### Daily targets — Phase-0 gate (compressed Parquet + ZSTD level >= 6)

| Data | Estimated daily (compressed) |
|---|---|
| Trades (BTC + ETH) | ~200 MB |
| Funding + OI + mark | ~10 MB |
| Liquidations (if added) | ~5 MB |
| **Total tick-level** | **~215 MB/day** |
| Bar aggregates (1m/5m/15m/1h/4h) | ~20 MB/day (after 7-14 days) |

### Daily targets — Swing research context (VPS only, additional)

| Data | Estimated daily (compressed) |
|---|---|
| Bybit BTC/ETH/SOL trades | ~300 MB |
| Whale positions | ~10 MB |
| FRED macro | < 1 MB |
| **Total swing context** | **~311 MB/day** |

### 60-day target

| Tier | Contents | Size |
|---|---|---|
| Hot (VPS, last 7-14 days) | Phase-0 tick-level + swing context | ~7 GB |
| Warm (VPS or PC, last 30-60 days) | 1m/5m bars + features | ~1.2 GB |
| Cold (PC only, older) | Daily/4h bars or feature snapshots | < 500 MB |

**Total 60-day footprint: ~8.7 GB** — fits in 30 GB free-tier with room to spare.

## Retention Policy

Enforced in daily pipeline:

1. **Hot (VPS):** last 7-14 days of tick/trade-level data
2. **Warm (VPS or PC):** last 30-60 days of 1m/5m bars + features
3. **Cold (PC only):** older data as daily/4h bars or feature snapshots only
4. Automatic deletion or archival of anything exceeding the budget
5. Never store full L2 history

## Lowered Promotion Criteria

Under Zero-Cost Mode:

| Criterion | Full Mode | Zero-Cost Mode |
|---|---|---|
| Required streams | trade, book, funding, mark_price, open_interest | trade, funding, open_interest, mark_price |
| Coverage threshold | >= 0.995 | >= 0.95 |
| Stale bursts | Blocking (must be 0 for promotion) | Warning only |
| Full book absence | DIRTY | Expected, not a finding |
| Promotion streak | 7 consecutive clean days | 14 consecutive clean days |
| Determinism check | Required | Required (on decision path) |

### Rationale for 14-day streak

With lower coverage threshold and no book requirement, we need a longer
window to build confidence. 14 days = ~3 GB of data, proving the system
can sustain recording under real free-tier conditions.

## Collector Configuration

### Phase-0 gate collectors (required)

Zero-Cost Mode uses `swing_only = true` on Hyperliquid collectors:

```toml
# collectors/zero_cost/hyperliquid-btc.toml
venue = "hyperliquid"
symbol = "BTC"
data_dir = "data"
channel_capacity = 10000
backpressure = "drop_oldest"
swing_only = true
```

The `swing_only` flag already:
- Drops `l2Book` (full order book)
- Keeps `trades` (via `trade` subscription)
- Keeps `activeAssetCtx` (funding + mark + OI)

This is the exact stream set needed for the Phase-0 gate.

### Swing research collectors (recommended, optional)

The swing path adds cross-asset context for research:

```powershell
# Windows
.\swing_collectors.ps1

# VPS
bash ~/mp-build/ops/scripts/deploy_swing.sh
```

These run alongside the Phase-0 collectors and are NOT gate-required.

## Signal Compatibility

All signals registered in the catalog carry a `zero_cost_compatible` flag:

- **Compatible:** signals computable from trades + bars only (CVD, delta,
  imbalance, volume bubble, market profile, realized vol, trend, value area,
  VWAP, ATR, sweep)
- **Incompatible:** signals requiring full L2 book depth (book imbalance at
  multiple levels, order book imbalance features)
- **Degrades:** accumulation detector works with OI + smart-flow proxies;
  exchange outflow leg may be absent

## What Is Explicitly Deferred

- Full L2 book recording and any features requiring it
- Multi-symbol expansion beyond BTC + ETH
- Options, macro, heavy whale census, cross-asset analytics
- Any path requiring paid storage or paid compute
- Live trading (PD-1)

## References

- `ROADMAP.md` — Phase-0 Zero-Cost section
- `specs/024-market-data-integrity.md` — Zero-Cost scoring amendment
- `specs/025-signal-catalog.md` — zero_cost_compatible flag
- `specs/045-accumulation-detector.md` — graceful degradation
- `ops/core_symbols.txt` — core recording set
- `docs/SWING_DATA_PLAN.md` — swing collector topology (Zero-Cost base)
