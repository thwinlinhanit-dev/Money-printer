# storage

## Purpose

Cold storage layer: transforms raw event logs into Hive-partitioned Parquet tables, manages compaction, SCD2 tracking, feature store materialization, and audit trails.

## Ownership

- `src/parquet_trades.rs` — Parquet trade writer
- `src/compactor.rs` — log compaction
- `src/layout.rs` — Hive partition layout
- `src/dataset.rs` — dataset management
- `src/manifest.rs` — manifest tracking
- `src/scd2.rs` — slowly changing dimension tracking
- `src/feature_store.rs` — feature store materialization (W-6 `rows_content_hash` excludes `engine_git_sha` — provenance, not content; FEA-6 `resolve_version` keys on `params_hash:feature_ver`, legacy bare-`params_hash` markers match when `ver` equals the running `feature_ver`)
- `src/prune.rs` — data pruning
- `src/audit.rs` — audit logging (`audit_raw_log`: missing day-file = blocking `recording_missing`, distinct from `unreadable_log` — spec 024 decision 2026-08-25)
- `src/migrate.rs` — legacy raw-log migration (schema-1 → current, W-6 write-new + verify)
- `src/promotion.rs` — seven-day promotion gate (INT-5 / Phase 0); adjacency gate requires consecutive scorecards to be exactly 1 UTC calendar day apart
- `src/cross_venue.rs` — cross-venue gap detector (spec 026, CVG-1..12)
- `src/historical.rs` — historical bootstrap core (spec 027, HBS-2..7/HBS-9/HBS-10)
- `src/historical_download.rs` — live Binance-archive download, `live-http` feature-gated (spec 027, HBS-1/HBS-8; owner-approved 2026-08-05)
- `src/analytics.rs` — in-memory series builders over merged event streams: footprint buckets, OIWA, carry (hourly mark/OI/funding basis), liq (signed liquidation notional per interval; CONV-8 non-finite skip, same floor-bucket rule as carry so legs join on `interval_ts_ns`), `dom_series` (top-N order-book ladder sampled at fixed boundaries via `BookMirror`; boundaries strictly before an event settle pre-apply, trailing boundaries repeat the final state, `stale` flags surface seq holes — EVT-9; capped by `max_samples`)
- `src/ibit_leadlag.rs` — IBIT ↔ Deribit daily cross-market table (spec 040 IBI-10): flat Parquet at `cold/ibit_cross/`, one row per UTC day (closing IV/net-delta per venue, IV divergence, flow correlations lag 0..2); below-overlap correlations are NULL, never zero. `build_rows(events, params)` replays a merged event stream through `mp_features::ibit_cross::IbitCrossDaily`; wired into `mp-materialize --cross-out <file.parquet>` (fail-closed: errors when the config's `[ibit_cross]` family is disabled)
- `src/bin/mp-query.rs` — raw-log query CLI: `carry` (research leg for event studies), `liq` (bybit liquidation stress leg, spec 029 COL-29, symbol resolution via symbol table, JSON out), `bars` (OHLCV+order-flow bars from ONE log via `footprint_bars` — terminal Slice 1 price pane), `dom` (sampled top-N ladders from ONE log via `dom_series` — terminal Slice 1 DOM pane), `merge` (multi-day logs into ONE event log in MAT-5/EVT-5/EVT-8 canonical order via the EAGER `load_logs_merged`; the streaming `stream_logs_merged` fails on legacy schema-1 logs whose symbol frame arrives after the first event, e.g. 2026-07-18..07-29)
- `src/bin/mp-migrate.rs` — migration CLI
- `src/bin/mp-audit.rs` — data-integrity audit CLI (INT-3); also surfaces the promotion gate
- `src/bin/mp-cross-venue.rs` — cross-venue gap detector CLI (spec 026, CVG-12)
- `src/bin/mp-bootstrap.rs` — historical bootstrap CLI (spec 027, HBS-1/HBS-9)
- `src/materialize.rs` — log→FeatureStore materialization pipeline (spec 016; mp-features engine, EVT-5 merge, canonical log ordering, symbols snapshot, `MP_MATERIALIZE_MAX_BYTES` RAM guard); `stream_logs_merged` — streaming k-way merge (same MAT-5/EVT-5/EVT-8 canonical order, frame-by-frame) for the daily determinism replay's RAM guard, and `load_logs_merged` — the eager loader the materializer uses
- `src/bin/mp-materialize.rs` — materialization CLI (spec 016, `--log --config --out --git-sha`; prints `symbols_hash`/snapshot path)

## Verification

- `cargo test -p mp-storage`
- `cargo test -p mp-storage --features live-http` (spec 027 HBS-1/HBS-8 live-download mock-server suite)
- `cargo test -p mp-storage --test storage`
- `cargo test -p mp-storage --test materialize` (spec 016 pipeline)

## Child DOX Index

None.
