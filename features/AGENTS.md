# features

## Purpose

Feature engineering pipeline: computes bars, technical indicators, orderflow
(footprint/CVD by size bucket), and derived features from market data. Manages
feature catalog, screener/grading (dead rules are rejected at setup, FEA-13),
signal catalog with promotion ladder + decay re-testing (spec 025), and hit
journal for strategy feedback.

## Ownership

- `src/engine.rs` — feature computation engine
- `src/bar.rs` — bar construction (time, tick, volume, dollar)
- `src/catalog.rs` — feature catalog/registry (footprint buckets, CVD, funding)
- `src/config.rs` — feature configuration
- `src/screener.rs` — screener/grading logic (validate_features: FEA-13)
- `src/hit_journal.rs` — strategy hit/miss journal
- `src/signal_catalog.rs` — signal promotion ladder, decay re-testing (SIG-1..5)
- `src/bin/footprint.rs` — offline orderflow study runner (spec 017 grading)
- `src/bin/signals.rs` — signal catalog CLI (spec 025)
- `features.toml.example` — example config

## Verification

- `cargo test -p mp-features`
- `cargo test -p mp-features --test feature_engine`

## Child DOX Index

None.
