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
- `src/options_greeks.rs` — options chain Greeks aggregation engine (spec 037 GRE): net GEX, max pain, implied probability, net delta/vega/theta, FD higher-order Greeks (vanna/volga/charm) over the live chain (record-now-analyze-later on spec 031 OptionTicker); shared chain plumbing (`ContractKey`/`ChainMap`/`TickerSnap`) reused by 038/039
- `src/options_iv.rs` — IV surface builder & vol analytics (spec 038 IVS): ATM IV interpolation, term structure with nearest-expiry fallback, 25Δ risk-reversal + wing richness, OI-weighted DVOL-style index, rolling-percentile regime classifier (reverse-ordinal 0=Rich/1=Fair/2=Cheap), pure `vrp()`
- `src/options_flow.rs` — cross-venue options flow aggregator (spec 039 OFI): windowed net premium / block / delta-adjusted notional / kind-aware moneyness buckets / tenor decomposition / call-put ratio / dynamic-threshold whale flow / acceleration; BS-delta fallback (Abramowitz–Stegun CDF); cross-venue divergence wired but None-suppressed until a second venue appears
- `src/signal_catalog.rs` — signal promotion ladder, decay re-testing (SIG-1..5)
- `src/swing.rs` — swing-horizon bar-aggregated features (spec 035 SWG-2 + spec 036 SLQ): pure bar-only computations (`realized_vol`, `trend_strength`, `value_area`, `volume_levels` POC/VA/HVN/LVN, `atr`, `compressed_range`/`sweep_of` sweep detector, `RollingVwap`) + their `BarFeature` adapters (`SwingRealizedVol`, `SwingTrendStrength`, `SwingValueArea` POC/high/low, `SwingRollingVwap`, `SwingAtr`, `SwingClose`, `SwingRange` high/low, `SwingSweep` extreme/stop per side, `SwingNearestLevel`), registered by `engine_from_config` under the `[swing]` config section (SWG-2: no L2/trade-tape dependency; SLQ family emits the sweep-reclaim event pair with its companion stop — spec 036 §2.3)
- `src/ibit_cross.rs` — IBIT ↔ Deribit cross-market global tick features (spec 040 IBI-5/6): OI-weighted daily closing IV + net-delta aggregates per venue, strict same-day pairing (a one-sided day pairs with nothing), IV divergence + flow lead-lag correlations at lags 0..2, single-venue suppression, min-overlap gate; registered ×4 under `[ibit_cross]` (enabled when `deriv_underlying` set). `IbitCrossDaily` is the PUBLIC offline builder (feed → next_completed → corr) that the IBI-10 materializer consumes
- `src/cohort.rs` — wallet cohort grading (spec 042 WCG): deterministic wallet scorer classifying addresses into SmartMoney/Whale/Retail/Dormant from WhalePosition history, weekly snapshot with journal; live `CohortFeature` family (WCG-7) emitting ALL FOUR aggregates — `cohort.whale_ratio`, `cohort.net_delta.{cohort}`, `cohort.smart_flow.{w}`, `cohort.concentration` (HHI) — as global ticks keyed per-symbol internally, last-wins census with stale eviction, fail-closed on missing/stale snapshot (`[cohort].snapshot_path` loads at engine build); wcg_1..11 tests incl. real proptest
- `src/netflow_flow.rs` — CEX flow velocity features (spec 043 CFV): endpoint velocity, acceleration, trapezoidal cumulative flow, midrank-percentile regime, z-score derived from spec 034 NetflowSnapshot events; per-field TickFeature instances registered under `[netflow_flow]`; addresses not polled within `stale_after_ns` (default 600s = 2× cadence, CFV-8) are evicted from the aggregate before totals recompute; cfv_1..10 incl. golden determinism + velocity proptest
- `src/accumulation.rs` — accumulation detector screener rule (spec 045 ACC): compound 3-condition AND signal (OI rising + smart money buying + exchange outflow) with dynamic percentile thresholds, 4h cooldown, ScreenerHit output; `AccumulationDetector` consumes streaming FeatureUpdates; acc_1..10 incl. offline/online identity golden (acc_7) and cooldown bar-boundary (acc_10); acc_5 forward-return study lives in `research/tests/test_accumulation.py`
- `src/bin/footprint.rs` — offline orderflow study runner (spec 017 grading)
- `src/bin/signals.rs` — signal catalog CLI (spec 025)
- `src/oi_regime.rs` — accumulation sub-signal input features (spec 045 §Sub-Signals): `oi.delta.{w}`/`oi.quadrant.{w}` tick features from spec-004 OpenInterest LEVELS + last-trade price with endpoint anchors (quadrants 1=new longs/2=short covering/3=long flush/4=new shorts; zero moves suppressed), `regime.trend` bar feature reusing swing's pure efficiency ratio (0=Trend/1=Chop, matching the detector's `trend_values=[0.0]`); registered under `[oi_regime]`
- `src/bin/cohort_score.rs` — weekly wallet-cohort scoring run (spec 042 WCG-9/10): replays recorded raw logs (the spec 028 `*_hyperliquid_positions.log` census) through WalletScorer, atomically writes `data/cohorts/{UTC date}.json`, journals the diff (with per-address score_breakdown) to `journal/cohort_changes.jsonl`; idempotent, optional `--config`/`--as-of-ns`, `--json` summary
- `src/bin/accumulation_replay.rs` — RES-4 feeder for SIG-ACC-1 (spec 045 ACC-5/7): merges raw logs by `(recv_ts_ns, seq)`, replays through engine_from_config with detector inputs forced active, journals ScreenerHits via HitJournal, and reports per-leg update counts (which of the 8 detector legs were alive) — the honest n=0 study record lives in `research/out/acc_study_2026-08-24.md`
- `tests/cohort_wiring.rs` — pins `features.toml.example` against `FeaturesConfig` (CONV-16 drift guard) + proves engine_from_config registers all seven cohort.* instances when `[cohort] enabled = true`, and that emissions stay fail-closed without a snapshot (WCG-8)
- `tests/options_analytics.rs` — acceptance tests for specs 037/038/039 (gre_*/ivs_*/ofi_*)
- `features.toml.example` — example config

## Verification

- `cargo test -p mp-features`
- `cargo test -p mp-features --test feature_engine` (incl. `swg_2_engine_from_config_registers_swing_bar_features` + `slq_engine_from_config_registers_sweep_and_profile_family`; also guards that the PRODUCTION `features.toml` always parses — fea_7)
- `cargo test -p mp-features --lib` (incl. 8 `swg_2_*` + 11 `slq_*` swing unit tests)
- `cargo test -p mp-features --test cohort_wiring` (example-config sync + WCG-7 registration + WCG-12 signal-catalog entry at Hypothesis)
- Weekly ops: `target\debug\cohort_score.exe --log data\raw\*_hyperliquid_positions.log --out-dir data\cohorts` (then point `[cohort.inner].snapshot_path` at the newest file)

## Child DOX Index

None.
