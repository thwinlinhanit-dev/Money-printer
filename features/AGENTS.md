# features

## Purpose

Feature engineering pipeline: computes bars, technical indicators, and derived features from market data. Manages feature catalog, screener/grading, and hit journal for strategy feedback.

## Ownership

- `src/engine.rs` — feature computation engine
- `src/bar.rs` — bar construction (time, tick, volume, dollar)
- `src/catalog.rs` — feature catalog/registry
- `src/config.rs` — feature configuration
- `src/screener.rs` — screener/grading logic
- `src/hit_journal.rs` — strategy hit/miss journal
- `features.toml.example` — example config

## Verification

- `cargo test -p mp-features`
- `cargo test -p mp-features --test feature_engine`

## Child DOX Index

None.
