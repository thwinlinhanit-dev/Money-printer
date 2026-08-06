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
- `src/feature_store.rs` — feature store materialization
- `src/prune.rs` — data pruning
- `src/audit.rs` — audit logging
- `src/migrate.rs` — legacy raw-log migration (schema-1 → current, W-6 write-new + verify)
- `src/promotion.rs` — seven-day promotion gate (INT-5 / Phase 0)
- `src/cross_venue.rs` — cross-venue gap detector (spec 026, CVG-1..12)
- `src/historical.rs` — historical bootstrap core (spec 027, HBS-2..7/HBS-9/HBS-10)
- `src/historical_download.rs` — live Binance-archive download, `live-http` feature-gated (spec 027, HBS-1/HBS-8; owner-approved 2026-08-05)
- `src/bin/mp-migrate.rs` — migration CLI
- `src/bin/mp-audit.rs` — data-integrity audit CLI (INT-3); also surfaces the promotion gate
- `src/bin/mp-cross-venue.rs` — cross-venue gap detector CLI (spec 026, CVG-12)
- `src/bin/mp-bootstrap.rs` — historical bootstrap CLI (spec 027, HBS-1/HBS-9)

## Verification

- `cargo test -p mp-storage`
- `cargo test -p mp-storage --features live-http` (spec 027 HBS-1/HBS-8 live-download mock-server suite)
- `cargo test -p mp-storage --test storage`

## Child DOX Index

None.
