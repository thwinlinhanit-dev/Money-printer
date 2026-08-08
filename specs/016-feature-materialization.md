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
- [x] `FeatureStore` compiles and writes valid Parquet
- [x] Test: `mat_6_materialize_writes_layout_and_roundtrips` — writes `feature/venue/symbol/date.parquet`, read back identical values
- [x] Test: `mat_5_materialize_is_deterministic_and_idempotent` — two runs, identical file bytes and file set
- [x] Test: `fea_6_params_change_allocates_new_version_and_never_overwrites` — config params change → new version, prior files untouched
- [x] Test: `mat_6_multi_log_symbols_remap_and_merge` — symbols shared across logs resolve to one id (EVT-5), events merge in ts order
- [x] Test: `mat_6_cli_materializes_and_exits_zero` — `mp-materialize` end-to-end via `CARGO_BIN_EXE`
- [x] Test: `mat_6_materialize_skips_missing_config_file_cleanly` — clean non-zero exit, no partial store
- [x] Test: `mat_5_log_argument_order_does_not_change_symbol_ids_or_bytes` — `--log` order is canonicalized; reversed argument order produces byte-identical layout + bytes
- [x] Test: `mat_5_symbols_snapshot_written_and_resolves_ids` — shared table persisted as `{root}/symbols/{hash}.json`, hash in every Parquet footer; `symbol_id` resolvable
- [x] Test: `mat_5_ram_guard_rejects_oversized_corpus_before_reading` — corpus over `MP_MATERIALIZE_MAX_BYTES` fails closed with guidance, writes nothing
- [ ] Test: `mat_4_offline_replay_matches_live` — live run vs Parquet replay, identical screener hits (research-level gate, still open)
- [ ] Test: `mat_5_flush_threshold_respected` — verify live flush at boundary
- [ ] Integration: run feature engine + store for 1 hour, verify files appear

## Decisions
- 2026-07-19: Use `arrow` + `parquet` crates (Rust native, no Python dependency).
- 2026-07-19: Buffer size: 10,000 updates or 60 seconds, whichever first.
- 2026-07-19: Compression: zstd (good ratio, fast decompression for replay).
- 2026-08-06: `mp-storage` gains an `mp-features` dependency; materialization lives in
  `storage/src/materialize.rs` (logs → shared symbol remap → EVT-5 merge → engine →
  `FeatureStore`) behind `mp-materialize` CLI. `features::engine_from_config` is the
  one-code-path config→engine wiring reused by the binary (previously the engine was
  library-tested only).
- 2026-08-06: batch `materialize` reuses the streaming store's `write_features_no_overwrite`
  guard — identical re-materialization is a byte-level no-op, divergent content on the
  same version is a hard error (W-6). MAT-5 determinism proven by raw-byte comparison of
  two independent runs, not row-equality.
- 2026-08-06 (hardening, audit 08-06): (a) **Canonical input order** — `--log` paths are
  sorted byte-wise and exact duplicates dropped before symbol interning, so symbol-id
  assignment and merge order depend only on the input SET; `--log a --log b` ≡
  `--log b --log a` down to the byte. (b) **Self-describing symbols** — every run
  persists the shared table as an immutable content-addressed snapshot
  `{root}/symbols/{hash}.json` (id/venue/venue_symbol rows in id order) and records the hash
  in every Parquet footer (`symbols_hash` KV); rows carry numeric ids only, so without
  the snapshot the store was unresolvable. (c) **RAM guard** — total on-disk log bytes
  are checked against `MP_MATERIALIZE_MAX_BYTES` (default 16 GiB) BEFORE any log is
  opened; a run over the cap fails closed with per-day-slicing guidance instead of OOMing
  (streaming merge remains future work). The symbols snapshot is written only after all
  feature files succeed (no orphan on failure) and skipped for an empty table. The W-6
  content hash now includes `symbols_hash`, so re-running over a store written by the
  pre-snapshot binary hard-errors naming the mismatching dimension — the materializer
  shipped the same day, so such stores are non-resumable (fresh `--out` or a params
  change allocates a new `ver=N`).

## Open questions
- None.
