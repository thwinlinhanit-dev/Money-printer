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
- `src/bin/mp-migrate.rs` — migration CLI
- `src/bin/mp-audit.rs` — data-integrity audit CLI (INT-3); also surfaces the promotion gate

## Verification

- `cargo test -p mp-storage`
- `cargo test -p mp-storage --test storage`

## Child DOX Index

None.
