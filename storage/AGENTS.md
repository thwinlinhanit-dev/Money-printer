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

## Verification

- `cargo test -p mp-storage`
- `cargo test -p mp-storage --test storage`

## Child DOX Index

None.
