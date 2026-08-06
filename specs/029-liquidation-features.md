# 029 — Cross-Venue Liquidation Aggregation & Estimated Liq Bands

## Purpose

(a) De-sample liquidations by aggregating the throttled per-venue liq feeds
(Binance ~1/s, COL-8) across all recorded venues into a best-effort merged liq
tape; (b) estimate liquidation cascade bands from OI + funding + leverage-tier
assumptions, as features. Both honestly labeled: aggregated ≠ census; estimated
≠ ground truth. Estimated bands are validated against Hyperliquid real liq
prices (spec 028) via an event study (RES-4) before any strategy use.

## Scope

In: two new features in the 004 catalog (additive — NO new event variant; both
derive from existing `Liquidation` + `OpenInterest` + `Funding` events):
`liq.agg` (cross-venue merged liq tape, best-effort) and `liq.est_bands`
(estimated cascade levels from OI/funding/leverage tiers). Online + offline
(feature engine, 004). Out: a true liquidation feed (impossible — venues
throttle), strategy decisions (features feed strategies only via the funnel,
006), Hyperliquid real positions (spec 028), on-chain liq.

## Design

```
Liquidation events (all venues) ──┐
                                  ├─▶ liq.agg   (merge + dedup per window) ──▶ FeatureUpdate { sampled=true }
OpenInterest + Funding events ───┴─▶ liq.est_bands (OI·leverage tiers) ──────▶ FeatureUpdate { estimated=true }
                                                                                  │
                       spec 028 Hyperliquid real liq prices ──▶ validate (RES-4 event study, offline)
```

- `liq.agg`: merge `Liquidation` events across venues for a symbol within
  `dedup_window_ms` (default 250ms); emit aggregate `{count, notional, venues}`
  per `agg_window` (default 1s). Mark `sampled: true` (still NOT a census —
  venues throttle).
- `liq.est_bands`: from OI (per venue) + funding + a configurable leverage-tier
  distribution (default tiers `{1,2,3,5,10,20,50}×` with weights), compute
  estimated liq price bands per symbol; emit bands (upper/lower) +
  notional-at-risk at OI-update cadence. Mark `estimated: true`.

## Requirements

- **LIQ-1** `liq.agg` MUST merge `Liquidation` events across all recorded
  venues for a symbol within `dedup_window_ms` (default 250ms) and emit
  aggregate `{count, notional, venues}` per `agg_window` (default 1s). MUST
  mark `sampled: true` (venues throttle; NOT a census — COL-8).
- **LIQ-2** `liq.est_bands` MUST compute estimated liq price bands from OI +
  funding + configurable leverage-tier weights per symbol, emitted at
  OI-update cadence. MUST mark `estimated: true` (model output, not ground
  truth).
- **LIQ-3** Both features MUST be deterministic (CONV-9..12): pure function of
  (events, config, seed); venue/leverage iteration via BTreeMap/sorted
  (CONV-10); same inputs ⇒ byte-identical output (golden, MAT-5-style).
- **LIQ-4** Both MUST register in the 004 catalog with id, version, params,
  warmup; `TickFeature`/`BarFeature` locality per FEA-9 (`liq.est_bands`
  online+offline; `liq.agg` online); NaN-suppression per FEA-5 (CONV-8
  fail-closed).
- **LIQ-5** MUST NOT introduce a new event variant (derived from existing
  `Liquidation`/`OpenInterest`/`Funding` events) ⇒ no spec-001 amendment, no
  owner sign-off for schema. Additive to the 004 catalog.
- **LIQ-6** `liq.est_bands` MUST be validated against spec 028 Hyperliquid
  real liq prices as an offline event study (RES-4); report band accuracy.
  MUST NOT be strategy-consumed until that event study clears (funnel, 006).
- **LIQ-7** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16),
  entries in `features.toml.example`; leverage-tier weights + dedup/window
  params explicit with documented defaults.
- **LIQ-8** Tests MUST use recorded fixtures in `testdata/`, no network
  (CONV-23); requirement-ID test names (CONV-21); proptest for the band math
  (CONV-22).
- **LIQ-9** No new network dependency (reads recorded data via the feature
  engine/Dataset). Freely implementable (no "ask first" — like spec 026).
- **LIQ-10** The RES-4 study MUST run weekly as a scheduled job (systemd
  timer, like the grading job GRD-3) once spec 028 mp-whale logs exist, and
  MUST journal a SIM-10 run record per run to `runs/index.jsonl` (RES-4,
  W-6). No positions log yet ⇒ the run is a skip, not a failure; a re-fired
  week never rewrites the weekly ledger (RES-2 idempotency).
- **LIQ-11** The leverage-tier weights MUST be calibratable from the recorded
  spec 028 real leverage distribution via a deterministic offline tool
  (notional-weighted per geometric-midpoint tier buckets); the calibrated
  `[liq_est_bands]` section MUST be consumable by `features.toml` (LIQ-7) and
  journaled as evidence (RES-4). The shipped defaults remain a documented
  assumption until a real calibration is applied.

## Acceptance criteria

- [ ] `liq_1_agg_merges_across_venues_and_marks_sampled`
- [ ] `liq_2_est_bands_compute_from_oi_funding_leverage_and_mark_estimated`
- [ ] `liq_3_features_are_deterministic` (golden)
- [ ] `liq_4_catalog_registration_and_locality` (FEA-9) + `nan_suppression` (FEA-5)
- [ ] `liq_5_no_new_event_variant` (assert no spec-001 schema change)
- [x] `liq_6_est_bands_validated_against_hyperliquid_ground_truth` (RES-4, offline) + `liq_6b_whale_study_binary_replays_recorded_logs` (end-to-end binary) + `liq_6c_whale_study_journals_sim10_run_record` (SIM-10 run-record journal, RES-4)
- [ ] `liq_7_check_config_rejects_unknown_fields`
- [ ] `liq_8_proptest_band_math` (CONV-22)
- [x] `liq_10_whale_study_timer_skips_without_logs_and_journals_runs` (weekly systemd timer + wrapper; skip-not-fail until spec 028 logs exist)
- [x] `liq_11_leverage_weights_calibrate_from_spec_028_distribution` (pure notional-weighted calibration math) + `liq_11b_whale_study_binary_reports_calibrated_leverage_tiers` (end-to-end binary) + Python job `run_calibrate_leverage.py`

## Decisions

- 2026-08-04: New spec (BACKLOG `liquidation-level estimator [v1.x]` +
  brainstorm de-sampling). Two features: `liq.agg` (de-sample) + `liq.est_bands`
  (estimate). Both honest about fidelity.
- 2026-08-04: NOT a census (LIQ-1) — Binance throttles liq ~1/s (COL-8
  `sampled` flag); cross-venue aggregation de-samples but venues differ in
  reporting. Best-effort, labeled.
- 2026-08-04: NOT ground truth (LIQ-2) — estimated bands are a model
  (OI + leverage-tier assumptions). Validated against spec 028 Hyperliquid real
  liq prices (the unique census) via RES-4 before any strategy use.
- 2026-08-04: No new event variant (LIQ-5) ⇒ no spec-001 amendment ⇒ no owner
  sign-off for schema. Additive to 004 catalog. No network (LIQ-9) ⇒ freely
  implementable (like 026). Leverage-tier weights are assumptions — calibrate
  from spec 028 real positions over time (replace assumptions with the observed
  distribution).
- 2026-08-04 (impl): LIQ-1..9 implemented — `features/src/liquidation.rs`
  (`LiqAgg`, `LiqEstBands`, `band_accuracy`), `features/src/config.rs`
  (`LiqAggParams`, `LiqEstBandsParams`, `LeverageTier` + `FeaturesConfig`
  wiring), `features/features.toml.example`, `features/tests/liquidation.rs`
  (liq_1..liq_8 all pass; clippy/fmt clean). Judgment calls (W-5): the
  `FeatureUpdate` scalar constraint means each feature emits ONE actionable
  value — `liq.agg` emits the deduped rolling signed notional (cross-venue
  echoes within `dedup_window_ns` at the same price collapse to one event), and
  `liq.est_bands` emits the fractional distance from mark to the nearest
  estimated long-side liq level. The richer outputs are exposed via accessors
  (`long_liq_level`, `short_liq_level`, `notional_at_risk`) and `band_accuracy`
  for offline RES-4 validation — the feature-consumable scalar is the KISS
  signal. `liq.agg` locality is `Both` (amends the draft's "online": the
  de-sampled tape must materialize for offline research/backtests; it uses only
  recorded Liquidation events, so replay is safe). The `sampled()`/`estimated()`
  flags are accessor methods (FeatureUpdate carries only the scalar; the
  feature id + catalog is the source of truth). All `liq.est_bands`  assumptions (entry ≈ mark proxy, mmr = 1/(leverage·buffer), tier weights, the ≥10×
  "imminent" cohort) are documented and MUST be validated against spec 028 real
  liq prices via `band_accuracy` (RES-4) before any strategy use (LIQ-6).
- 2026-08-05 (impl): LIQ-6 wired to real spec 028 positions — `WhaleBandStudy`
  in `features/src/liquidation.rs` is the RES-4 harness: it replays recorded
  Hyperliquid mark/OI into per-symbol `LiqEstBands` state (same venue as the
  positions, apples-to-apples) and pairs every `WhalePosition` real liq price
  with the SIGN-AWARE estimate — longs pair with `long_liq_level` (downside),
  shorts with `short_liq_level` (upside). Coverage direction mirrors per side:
  long `est <= real`, short `est >= real` ("the estimate was not dangerously on
  the wrong side of the actual liq price"). `long_accuracy()` is definitionally
  the standalone `band_accuracy` (one source of truth); `short_accuracy()` and
  `accuracy()` extend it. Observations are `(symbol, recv_ts_ns, is_long,
  estimate, realized)` with non-finite/zero-size/no-estimate positions skipped
  fail-closed (CONV-8). The `whale_study` binary (features) reads the recorded
  `mp-whale` positions log + the hyperliquid market log, remaps the two
  independent symbol-id spaces onto one canonical space (each collector run
  interns its own ids, EVT-8), merges in recv order (EVT-5), replays, and
  reports per-side + total `(n, mre, coverage)` — JSON with `--json`. Study
  output is evidence only (WHL-5/PD-4).  `whale_study --log <hyperliquid.log>
  --log <positions.log> [--config features.toml]`. With `--run-id` +
  `--runs-dir` (and optional `--git-sha`) the study journals a SIM-10-style
  run record to `<runs-dir>/index.jsonl` (append-only, W-6) — the RES-4
  "tracker-style run records" requirement (spec 010): run_id + git_sha +
  canonical params hash + data range + (n, mre, coverage), reproducible from
  the record alone (same contract as the sim tracker, spec 005).
- 2026-08-05 (impl): the research-side weekly trend job (RES-2 pattern) —
  `research/run_band_accuracy.py` shells out to `whale_study --json` over
  recorded logs (`--log` repeatable), parses the report fail-closed
  (`research/band_accuracy.py`, pure stdlib, unit-tested) and journals the
  idempotent weekly ledger `band_accuracy/{week}.json` + the append-only
  `band_accuracy/band_accuracy.jsonl` trend (W-6). Week defaults to the run's
  data range (ISO year-week of `data_from_ns`, UTC); `--week` pins it AND
  short-circuits an already-graded week before any shell-out. Judgment calls:
  unavailable data is a failed job (exit 2, never an optimistic n=0 row —
  same principle as run_grading.py); with an auto-derived week a re-fire has
  already journaled the binary's per-RUN record to `runs/index.jsonl` (the
  SIM-10 tracker records runs) while the weekly LEDGER stays idempotent —
  tracker vs ledger are distinct contracts, both append-only. `.py`
  binaries are run under the current interpreter (test shim).
- 2026-08-05 (impl): LIQ-10 scheduled — `ops/systemd/whale-study.timer`
  fires the `whale-study.service` oneshot weekly (Tue 06:30 UTC, clear of
  the Mon 06:00 grading run; `Persistent=true`). The service is
  `ConditionPathExists`-gated on `data/raw/*_hyperliquid_positions.log`
  (WHL-6 pattern) so it is a SKIP, never a failure, until spec 028 census
  data exists; `ProtectSystem=strict` with `ReadWritePaths` limited to
  `runs/` + the band-accuracy ledger and `ReadOnlyPaths` on the data dir
  (RES-7 posture — the study can never alter recorded data, W-6).
  `ops/scripts/run_whale_study_weekly.sh` (env-overridable paths for
  testability) discovers the last 7 days of `*_hyperliquid*.log` files,
  gates again on the positions log, stamps the repo git SHA onto the run
  record (reproducible-from-itself, SIM-10), and calls
  `run_band_accuracy.py` with `--runs-dir /opt/money-printer/runs` so the
  SIM-10 record lands in the shared tracker. Stale-but-present logs (collector
  down > 1 week) also skip — flagging a dead collector is the dead-man's job
  (OPS-2), not this passive consumer's.
- 2026-08-05 (impl): LIQ-11 calibrated — the tier weights (a documented
  assumption since the spec was written) now have a deterministic calibration
  path. `features/src/leverage.rs` (`calibrate_leverage_weights`) buckets
  `(leverage, |size|·entry)` samples from recorded spec 028 positions onto the
  CONFIGURED tier set via geometric-midpoint boundaries
  (`sqrt(L_i·L_{i+1})`; a sample exactly on a boundary goes to the higher-
  leverage tier — conservative, closer to liquidation); each weight is the
  bucket's NOTIONAL share, because the model spreads OI across tiers
  (`notional_at_risk += oi × weight`). The math is pure/order-independent
  (CONV-9/10) and fail-closed (non-finite/zero samples skipped, CONV-8).
  `whale_study --leverage-calibration` replays the positions log (reusing
  the load/remap/merge machinery) and reports `n, positions_seen,
  total_notional, tiers[{leverage,weight,count,notional}], sum_weights` plus
  the canonical params hash + data range; `--run-id/--runs-dir` journals a
  SIM-10 record (`study: "leverage_calibration"`). Judgment calls: notional
  (not count) weighting — a count histogram would misrepresent where the OI
  sits; `liq_price` is deliberately NOT a sample filter (a NaN sentinel must
  not exclude a valid leverage); zero-weight tiers stay in the emitted TOML
  (a tier's level still shapes the nearest cascade level even at weight 0).
  The research job `run_calibrate_leverage.py` shells out to the binary,
  fails closed on an empty census or `Σ weights ≠ 1`, and renders the
  `[liq_est_bands]` section (with `--print-toml`) to paste into
  `features.toml`; each run is journaled append-only (`calibrations.jsonl`).
  Calibration is deliberately NOT idempotent — it is a cumulative estimate
  that improves as the census grows, unlike a fixed-week grade. Evidence
  records are immutable (`<run_id>.json` is never overwritten; a re-run needs
  a fresh run id, W-6). Note: `runs/index.jsonl` now carries TWO record
  shapes — `study: "whale_study"` (band accuracy) and `study:
  "leverage_calibration"` — consumers must filter by `study`.
- 2026-08-06: the weekly trend journal is now consumed downstream —
  `ops::report::load_band_accuracy_trend` grounds the monthly report's RES-4
  section on `band_accuracy.jsonl` (spec 009 OPS-6: missing journal = "no
  data" month, corrupt line = fail closed), and `band_accuracy_decay_alert`
  watches the same journal for drift/decay (spec 009 OPS-13, RES-3
  semantics: ≥ 12 graded weeks, trailing 4-wk mean coverage < half the 12-wk
  baseline, or MRE > double). Both are alert/report-only (W-6) — the journal
  stays append-only.


## Open questions

- ~~Leverage-tier distribution defaults — calibrate from spec 028 Hyperliquid
  real leverage once recorded; until then, document assumptions honestly.~~
  RESOLVED (2026-08-05): the calibration tooling is implemented (LIQ-11) and
  wired end-to-end; the shipped default weights remain the documented
  assumption until a real calibration run over recorded positions replaces
  them in `features.toml`.
- `liq.agg` cross-venue dedup window — venues report liqs at different
  latencies; 250ms default is a guess, calibrate from data.
