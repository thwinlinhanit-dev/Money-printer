# 016 — Feature Materialization Pipeline

## Purpose
Persist `FeatureUpdate` events to Parquet so the screener can grade historical hits, backtests can replay identical feature state, and research can analyze feature behavior offline.

## Scope
In: `FeatureStore` struct, Arrow/Parquet schema, partitioned file layout, flush logic, deterministic output guarantee, offline replay. Out: feature computation logic (spec 004), feature registration, schema evolution beyond version bumps.

## Design

### FeatureStore
```rust
use parquet::arrow::ArrowWriter;  // parquet crate

pub struct FeatureStore {
    writer: ArrowWriter,        // parquet writer
    schema: Schema,             // fixed arrow schema (see spec 023: feature SymbolId)
    buffer: Vec<FeatureUpdate>, // accumulates before flush
    flush_threshold: usize,     // default 10,000
    last_flush_ns: i64,
    flush_interval_ns: i64,     // default 60s
}
```

### Schema
After string interning (spec 023), `feature` uses `SymbolId` rather than a raw string:

| column | type | notes |
|--------|------|-------|
| `feature_id` | `UInt32` | interned feature name (`SymbolId`, per spec 023) |
| `venue_id` | `UInt16` | interned venue (`Venue` enum as repr) |
| `symbol_id` | `UInt32` | interned symbol |
| `ts_ns` | `Int64` | feature timestamp |
| `value` | `Float64` | feature value |
| `ver` | `UInt16` | feature version |
| `config_hash` | `FixedSizeBinary(32)` | SHA-256 hash of feature config |

### Partition layout
```
data/features/{feature}/{venue}/{symbol}/{date}.parquet
```
Example: `data/features/funding_rate/hyperliquid/BTC/2026-07-19.parquet`

### Flush
- When buffer reaches `flush_threshold` (10,000 updates) or `flush_interval_ns` (60s) has elapsed since last flush.
- On explicit `FeatureStore::flush()` call (e.g., shutdown).
- On each flush: sort by `(ts_ns, config_hash)` for deterministic output, write Arrow record batch, clear buffer.

### Offline replay
- `FeatureStore::replay(parquet_dir)` reads all Parquet files for a given `(feature, venue, symbol)` range.
- Returns `Vec<FeatureUpdate>` in chronological order.
- Guaranteed byte-identical to live run if same feature config (MAT-5).

## Requirements
- **MAT-1** `FeatureStore` MUST be defined in `features/src/store.rs`.
- **MAT-2** Schema MUST include: feature, venue, symbol_id, ts_ns, value, ver, config_hash.
- **MAT-3** Partition layout MUST be: `feature/venue/symbol/date.parquet`.
- **MAT-4** Flush MUST occur when threshold crossed or on explicit `flush()`.
- **MAT-5** Output MUST be deterministic: same feature updates → same Parquet bytes (golden test).
- **MAT-6** Offline replay MUST produce identical `FeatureUpdate` sequence as live run.

## Acceptance criteria
- [ ] `FeatureStore` compiles and writes valid Parquet
- [ ] Test: `mat_1_write_feature_updates` — write 1000 updates, read back identical
- [ ] Test: `mat_2_partition_layout_correct` — verify directory structure
- [ ] Test: `mat_3_deterministic_output` — two runs, compare file hashes
- [ ] Test: `mat_4_offline_replay_matches_live` — live run vs Parquet replay, identical screener hits
- [ ] Test: `mat_5_flush_threshold_respected` — verify flush at boundary
- [ ] Integration: run feature engine + store for 1 hour, verify files appear

## Decisions
- 2026-07-19: Use `arrow` + `parquet` crates (Rust native, no Python dependency).
- 2026-07-19: Buffer size: 10,000 updates or 60 seconds, whichever first.
- 2026-07-19: Compression: zstd (good ratio, fast decompression for replay).

## Open questions
- None.
