# Retention Policy — Zero-Cost Mode

**Status:** Active (2026-08-31)
**Applies to:** Zero-Cost Mode only. Full-mode retention is managed separately.

## Purpose

Enforce strict data retention to fit within free-tier storage budgets
(30 GB on Google e2-micro, 200 GB on Oracle Always Free) while preserving
enough history for signal research and backtesting.

## Storage Tiers

### Hot (VPS — last 7-14 days)

- **Contents:** Raw tick-level data
  - **Phase-0 gate:** trades, funding, OI, mark_price for BTC + ETH (Hyperliquid)
  - **Swing research:** trades for BTC/ETH/SOL (Bybit), whale positions, FRED macro
- **Format:** Daily event logs (`{YYYYMMDD}_hyperliquid_{SYMBOL}.log`, `{YYYYMMDD}_bybit_{SYMBOL}.log`)
- **Compression:** ZSTD level >= 6 on Parquet compaction
- **Target size:** ~526 MB/day uncompressed (Phase-0 ~215 MB + swing ~311 MB)
- **Retention:** 7 days minimum, 14 days if disk allows
- **Cleanup:** Automatic via daily pipeline — oldest days deleted when hot tier exceeds budget

### Warm (VPS or PC — last 30-60 days)

- **Contents:** 1m/5m/15m/1h/4h bar aggregates + materialized features
- **Format:** Hive-partitioned Parquet in `data/features/`
- **Compression:** ZSTD level >= 6
- **Target size:** ~20 MB/day of bar aggregates
- **Retention:** 30 days minimum, 60 days target
- **Cleanup:** Automatic — bars older than 60 days downsampled to daily/4h

### Cold (PC only — older than 60 days)

- **Contents:** Daily/4h bar aggregates, feature snapshots, promotion history
- **Format:** Parquet in `data/cold/`
- **Compression:** ZSTD level >= 8
- **Target size:** < 500 MB total
- **Retention:** Indefinite (PC has more storage)
- **Cleanup:** Manual only (W-6: never delete without human instruction)

## Daily Pipeline Enforcement

The daily pipeline (`ops/scripts/daily_pipeline.ps1` and
`ops/scripts/daily_maintenance.sh`) enforces retention:

1. **After compaction:** Check hot tier size. If > budget, delete oldest days.
2. **After materialization:** Check warm tier size. If > budget, downsample
   oldest bars to daily/4h resolution.
3. **Monthly:** Check cold tier. Report to operator. Never auto-delete.

## Compression Settings

All Parquet files use ZSTD compression:

| Tier | ZSTD Level | Rationale |
|---|---|---|
| Hot | >= 6 | Fast compression, good ratio for tick data |
| Warm | >= 6 | Bar data compresses well |
| Cold | >= 8 | Max compression for archival |

## Bar Aggregation Strategy

When tick data ages beyond the hot tier:

1. Aggregate trades into 1m bars (OHLCV + volume delta)
2. Aggregate 1m bars into 5m bars after 14 days
3. Aggregate 5m bars into 1h bars after 30 days
4. Aggregate 1h bars into 4h bars after 60 days
5. Keep daily bars indefinitely (cold tier)

This preserves the information content (open, high, low, close, volume,
VWAP) while reducing storage by ~100x from tick-level.

## Monitoring

- `mp-ops storage-budget` reports current usage and days-to-cap
- P2 alert fires when projected usage exceeds budget within 14 days
- `disk-high` P2 fires at 85% disk usage

## References

- `docs/ZERO_COST_MODE.md` — Zero-Cost Mode overview
- `specs/003-storage.md` — storage layer design
- `ops/src/storage.rs` — storage budget implementation
- `ops/runbooks/storage-budget.md` — storage budget runbook
- `ops/runbooks/disk-high.md` — disk high runbook
