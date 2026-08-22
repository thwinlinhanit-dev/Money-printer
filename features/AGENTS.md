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
- `src/swing.rs` — swing-horizon bar-aggregated features (spec 035 SWG-2 + spec 036 SLQ): pure bar-only computations (`realized_vol`, `trend_strength`, `value_area`, `volume_levels` POC/VA/HVN/LVN, `atr`, `compressed_range`/`sweep_of` sweep detector, `RollingVwap`) + their `BarFeature` adapters (`SwingRealizedVol`, `SwingTrendStrength`, `SwingValueArea` POC/high/low, `SwingRollingVwap`, `SwingAtr`, `SwingClose`, `SwingRange` high/low, `SwingSweep` extreme/stop per side, `SwingNearestLevel`), registered by `engine_from_config` under the `[swing]` config section (SWG-2: no L2/trade-tape dependency; SLQ family emits the sweep-reclaim event pair with its companion stop — spec 036 §2.3)
- `src/bin/footprint.rs` — offline orderflow study runner (spec 017 grading)
- `src/bin/signals.rs` — signal catalog CLI (spec 025)
- `features.toml.example` — example config

## Verification

- `cargo test -p mp-features`
- `cargo test -p mp-features --test feature_engine` (incl. `swg_2_engine_from_config_registers_swing_bar_features` + `slq_engine_from_config_registers_sweep_and_profile_family`)
- `cargo test -p mp-features --lib` (incl. 8 `swg_2_*` + 11 `slq_*` swing unit tests)

## Child DOX Index

None.
