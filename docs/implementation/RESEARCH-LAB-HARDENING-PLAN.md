# RESEARCH-LAB-HARDENING-PLAN

**Status:** Implemented 2026-09-04 (Phases 0–6); Phase 7 anti-randomness
gates re-audited and closed 2026-09-07 (§10; see
`docs/research/RESEARCH-LAB-STATUS.md` and `specs/054` for the PASS/FAIL
report).
**Branch:** `feature/signal-catalog-footprint`
**Budget:** $0 / free-tier (`docs/ZERO_COST_MODE.md`)
**Specification:** this plan + `specs/054-research-lab-hardening.md`
**Requirement IDs referenced:** REL-1 … REL-34 (defined in spec 054)

## 1. Mission

Turn the existing Money-Printer into a deterministic, reproducible quantitative
research laboratory capable of measuring whether signals contain genuine
predictive value — by **hardening and extending what already exists**, not by
redesigning it. Live trading stays disabled (PD-1); capital at risk stays $0.

## 2. Current relevant architecture

- **mp-core** (`core/`): normalized event vocabulary (`EventEnvelope`,
  `MarketEvent`, `OrderIntent`), injected clock (`Clock`/`SimClock`),
  `SCHEMA_VER = 6`, deterministic hashing (`fnv1a_*`), symbol interning.
- **mp-features** (`features/`): streaming feature engine (spec 004) — one
  code path for live and offline; FEA-5 suppresses non-finite outputs with a
  counter; FEA-3 `warm()` gates warmup; the spec-025 signal catalog
  (`signal_catalog.rs`) has `SignalRecord { params_hash, grades, … }` and
  `GradeSnapshot { run_id, n, win_rate, avg_excess, horizon_ns }`; spec-045
  `AccumulationDetector` produces `ScreenerHit`; spec-049 `MarketProfilePoc/
  Vah/Val` (bar-based) in `catalog.rs`; `hit_journal.rs` persists hits as
  JSONL with 1h/4h/24h forward-return backfill slots.
- **mp-strategies** (`strategies/`): `Strategy` contract; promotion funnel
  (`funnel.rs`) — promotion needs evidence, demotion is automatic; frozen list.
- **mp-sim** (`sim/`): event-replay backtester; runs the production feature
  engine + strategies unmodified (SIM-5); deterministic `DecisionLog` with
  rolling FNV-1a hash (SIM-7); funding/coverage run-refusals; paper session.
- **mp-storage** (`storage/`): Parquet cold store (spec 003) — feature-store
  materialization (`feature_store.rs`) with FEA-6 `ver=N` resolution, footer
  KV metadata, W-6 no-overwrite content-hash guard; macro/trades/options
  Parquet writers. **This is the pattern any new Parquet artifact must mirror.**
- **mp-ops** (`ops/`): daily gate, status, health; `mp-research`
  (`research/`) is a separate tiny Rust crate + Python scripts (registry,
  grading) used for studies.

## 3. Existing modules that will be extended

| Phase | Module(s) extended | How |
|---|---|---|
| 1 | `features/src/engine.rs`, `features/src/accumulation.rs`, new `features/src/data_quality.rs` | first-class quality state; explicit insufficient-history blocking |
| 2 | `features/src/signal_catalog.rs`, new `features/src/signal_identity.rs` | full research identity; grade identity enforcement |
| 3 | new `features/src/observation.rs`, new `storage/src/observation_store.rs`, `sim/src/engine.rs` | immutable observations; Parquet store; backtester wiring |
| 4 | new `features/src/outcome.rs`, `sim/src/engine.rs` | forward outcomes from recorded mark series (post-run) |
| 5 | new `features/src/evaluation.rs`, `features/src/signal_catalog.rs` | sample gate, gross/net expectancy, distribution, structured promotion decision |
| 6 | `features/src/catalog.rs`, `features/src/accumulation.rs`, `specs/049`, `specs/045` | POC tie determinism + approximation docs; richer accumulation evidence |

## 4. Defects found against this specification (2026-09-04)

| # | Defect | Spec clause |
|---|---|---|
| D1 | No first-class `DataQualityState` anywhere; non-finite values are suppressed (FEA-5) but no explicit state reaches consumers; insufficient history is only implicit in `warm()` | Phase 1 — quality must be explicit |
| D2 | `AccumulationDetector::percentile_in_window` → `.unwrap_or(0.5)`: insufficient history is silently converted into a neutral percentile (missing ⇒ median) instead of **blocking** the sub-signal | Phase 1 — never missing ⇒ neutral/0.5 |
| D3 | `Screener` snapshot lookups return `None` ⇒ condition false with no explicit quality/insufficient-history marker on hits | Phase 1 — insufficient history explicit |
| D4 | `HitRecord` carries no quality state, so graded hits cannot distinguish clean from insufficient-history observations | Phase 1/4 |
| D5 | `SignalRecord` has only `params_hash`; no `feature_version`, `data_schema_version`, `cost_model_hash` — old grades are not invalidated when any of these change | Phase 2 — identity invalidation |
| D6 | `GradeSnapshot` carries no identity; `apply_grade` cannot refuse stale-identity evidence | Phase 2 |
| D7 | Sim cost parameters (taker/maker fee, slip) are never hashed, so a cost-model change cannot invalidate evidence | Phase 2 — cost_model_hash |
| D8 | No immutable observation record; the closest artifact (`HitRecord`) lacks identity, direction, quality | Phase 3 |
| D9 | Hit journal is JSONL; new research artifacts must be Parquet (no new DB) | Phase 3 |
| D10 | Forward returns are 1h/4h/24h hardcoded, gross-only, no MFE/MAE, no net (fee-adjusted) return | Phase 4 |
| D11 | `apply_grade` checks min_n + positive edge but produces no gross-vs-net split, no p25/p50/p75 distribution, and no structured promotion decision artifact with explicit reject reasons | Phase 5 |
| D12 | Market profile is a **bar-based approximation** (bar volume spread by mid, not trade-by-trade) — undocumented as such | Phase 6 — document |
| D13 | VAH/VAL POC selection uses `partial_cmp(…).unwrap_or(Equal)` — deterministic only by virtue of the preceding sort; not NaN-hardened, tie behavior untested | Phase 6 — POC tie determinism |
| D14 | Accumulation evidence snapshot exposes raw values but not per-leg percentile/sample-count evidence, and insufficient-history state is invisible | Phase 6 — evidence representation |

## 5. Risks

1. **Changing `ScreenerHit`/`HitRecord`** breaks existing tests and the JSONL
   journal format — mitigate with `#[serde(default)]` additive fields and
   `From`-conversion defaults; never remove existing fields.
2. **Parquet schema drift** — observations are a NEW artifact (no existing
   files), so the schema is fixed at birth; footer KV carries the identity so
   future schema bumps can be versioned like FEA-6.
3. **Backtester determinism** — adding observation recording must not change
   the existing decision-log hash; the recorder is write-only side state
   (never feeds back into dispatch), and the golden hash test protects us.
4. **No-lookahead** — outcomes must be computed post-run from the recorded
   mark series only; entry uses the mark at signal time (≤ ts), exit/MFE/MAE
   use marks in `(ts, ts+horizon]`; an open window at end-of-series yields
   `None`, never a fabricated return.
5. **Catalog backward-compat** — `signals --file catalog.json` files in the
   wild lack the new identity fields; all additions are `#[serde(default)]`
   so old files load, and `apply_grade` refuses identity-mismatched grades
   (old grades lack identity ⇒ they already have no matching identity ⇒
   refused — correct per Phase 2, surfaced loudly).
6. **$0 budget** — no new network, no new databases, no paid services; all
   work is pure Rust + the existing Parquet stack + existing collectors.

## 6. Exact planned changes per phase

### Phase 0 — Pre-condition (done 2026-09-04)
Operational health confirmed: drain re-pinned to 01:00 UTC (fires
07:30+06:30 local), scorecards present through 09-03 (day + determinism
artifacts), all scheduled tasks Ready except the in-flight off-host backup
validation run (`Running` — expected), trading mode `sleep` (live disabled).
Nothing critically broken ⇒ proceed.

### Phase 1 — Data Integrity
- `features/src/data_quality.rs` (new): `DataQualityState { Healthy,
  InsufficientHistory, Stale, Invalid }` with `reason()`; a pure
  `QualityTracker` (per symbol: min_samples, staleness window) updated from
  `FeatureUpdate`s; non-finite ⇒ `Invalid` (counted, never emitted).
- `accumulation.rs`: replace `unwrap_or(0.5)` percentile fallback with
  explicit insufficient-history blocking (`percentile_in_window` → `Option`,
  sub-signal inactive when `None`), and surface per-leg evidence
  (`percentile`, `samples`) in the hit snapshot.
- `screener.rs`: `ScreenerHit` gains `quality: DataQualityState` (serde
  default `Healthy` for the existing journal format); a rule condition whose
  feature was never observed sets the state to `InsufficientHistory` on the
  hit instead of silently evaluating false.
- Tests: non-finite ⇒ Invalid; missing history ⇒ InsufficientHistory and
  blocks; stale ⇒ Stale; nothing neutral-izes to 0.5.

### Phase 2 — Signal Research Identity
- `features/src/signal_identity.rs` (new): immutable `SignalResearchIdentity
  { signal_id, feature_version, data_schema_version, params_hash,
  cost_model_hash }` + `fn matches(&self, &Self) -> bool` + `fn fingerprint()`
  (FNV-1a over all fields) + constructors (`from_config(…, FeaturesConfig,
  cost_model_hash)` uses `params_hash()`).
- `signal_catalog.rs`: `SignalRecord` gains `feature_version: u16` (default
  1), `data_schema_version: u16` (default `mp_core::SCHEMA_VER`),
  `cost_model_hash: String` (default "") — all `#[serde(default)]`; add
  `identity()` accessor; `GradeSnapshot` gains `identity: String` (a
  fingerprint); `apply_grade` verifies `grade.identity == record identity`
  else `SignalError::IdentityMismatch`.
- `sim/src/engine.rs`: `SimConfig` gains `cost_model_hash` (default from
  taker/maker fee + slip via fnv) so sim evidence is identity-complete.
- Tests: changing params / feature version / data schema / cost model each
  invalidates old evidence (`apply_grade` refuses).

### Phase 3 — Signal Observation Engine
- `features/src/observation.rs` (new): `Direction { Long, Short }`;
  immutable `SignalObservation { observation_id: u64, identity:
  SignalResearchIdentity, timestamp_ns, symbol, venue, direction,
  feature_snapshot: BTreeMap<String, f64>, quality: DataQualityState,
  created_at_ns, outcomes: Option<ObservationOutcomes> }`; deterministic
  `observation_id` = FNV-1a(identity.fingerprint ‖ ts ‖ seq); constructor
  `from_screener_hit(hit, identity, quality)`; pure `SignalObservationEngine`
  (per-symbol quality tracking + fire recording, replayable).
- `storage/src/observation_store.rs` (new): Parquet write/read for
  observations mirroring `feature_store.rs` — columns `observation_id u64,
  signal_id utf8, feature_version u16, data_schema_version u16, params_hash
  utf8, cost_model_hash utf8, ts_ns i64, symbol u32, venue u16, direction i8,
  quality u8, created_at i64, snapshot_json utf8, plus nullable outcome
  columns (horizon_ns, gross, net, mfe, mae, hit)`; footer KV =
  `identity_fingerprint`; zstd-6; `{date}-{content_hash}.parquet` suffix +
  W-6 no-overwrite guard. Read side decodes back to `SignalObservation`.
- `sim/src/engine.rs` wiring: `Backtester::enable_observations(identity_cfg)`
  records one observation per dispatched intent (direction = intent side,
  snapshot = the triggering feature update(s), quality = tracker state),
  buffered and retrievable via `observations()`; decision-log hash untouched
  (recorder is write-only).
- Tests: observation creation; Parquet round-trip; determinism (same inputs
  ⇒ same observation bytes).

### Phase 4 — Forward Outcomes
- `features/src/outcome.rs` (new): `ObservationOutcome { horizon_ns,
  entry_price, exit_price, gross_return, net_return, mfe, mae, hit }`;
  `compute_outcomes(observations, mark_series: &[(i64, f64)], horizons:
  &[i64], round_trip_cost: f64) -> Vec<ObservationOutcomes>` — entry = last
  mark ≤ ts; exit = last mark ≤ ts+horizon; MFE/MAE over `(ts, ts+horizon]`;
  `hit = gross > 0`; incomplete window ⇒ `None` (never fabricated).
- `sim/src/engine.rs`: record per-symbol mark history during `stream()`;
  `Backtester::attach_outcomes(horizons)` fills `outcomes` post-run using
  `round_trip_cost = taker_fee + maker_fee`.
- Tests: horizon math; direction symmetry; incomplete-window ⇒ None;
  no-lookahead regression (outcomes depend only on marks ≥ ts).

### Phase 5 — Evaluation & Promotion
- `features/src/evaluation.rs` (new): `EvaluationReport { n, min_n, gate_passed,
  gross_expectancy, net_expectancy, win_rate, p25, p50, p75, reject_reasons:
  Vec<String> }`; `evaluate(observations_with_outcomes, horizon, min_n)` with
  sample-size gating and explicit reject reasons (too few samples, no
  outcomes, non-positive net expectancy, non-finite); `PromotionDecision {
  promote: bool, reasons: Vec<String> }` built from the report.
- `signal_catalog.rs`: `SignalRecord::grade_from_evaluation(report, run_id,
  now_ns)` — builds a `GradeSnapshot` with `avg_excess = net_expectancy`,
  refuses when `!gate_passed`; existing `apply_grade` unchanged (strengthened
  by identity checks).
- Tests: sample gate; gross vs net expectancy; p25/p50/p75 correctness;
  promotion refused with explicit reasons.

### Phase 6 — Footprint / Accumulation cleanup
- `features/src/catalog.rs`: document the bar-based approximation on the
  market-profile module (`volume spread by bar mid, not trade-by-trade`);
  harden VAH/VAL POC selection to `total_cmp`; add tie tests (equal-volume
  buckets ⇒ lowest bucket, deterministic).
- `features/src/accumulation.rs`: evidence representation — snapshot gains
  `evidence.oi_delta.percentile`, `evidence.oi_delta.samples`,
  `evidence.smart_flow.percentile`, `evidence.smart_flow.samples`,
  `evidence.insufficient_history` flags; strict AND logic untouched.
- `specs/045`, `specs/049`: amend with the approximation note + evidence
  fields.

## 7. Files expected to change

**New:**
- `docs/implementation/RESEARCH-LAB-HARDENING-PLAN.md` (this file)
- `docs/research/RESEARCH-LAB-STATUS.md`
- `specs/054-research-lab-hardening.md`
- `features/src/data_quality.rs`, `features/src/signal_identity.rs`,
  `features/src/observation.rs`, `features/src/outcome.rs`,
  `features/src/evaluation.rs`
- `storage/src/observation_store.rs`
- `sim/tests/observation_engine.rs` (integration + reproducibility +
  no-lookahead + golden); unit tests live in-module (data_quality,
  signal_identity, observation, outcome, evaluation) and in
  `features/tests/feature_engine.rs` (VAH/VAL tie determinism)

**Modified:**
- `features/src/lib.rs` (module exports), `features/src/signal_catalog.rs`,
  `features/src/accumulation.rs`, `features/src/screener.rs`,
  `features/src/engine.rs` (quality counter exposure, if needed)
- `features/src/catalog.rs` (docs + tie hardening)
- `storage/src/lib.rs` (export), `storage/Cargo.toml` (no new deps expected)
- `sim/src/engine.rs`, `sim/src/lib.rs` (exports)
- `specs/045-accumulation-detector.md`, `specs/049-footprint-signal-catalog.md`
- `docs/ROADMAP.md` (research-lab row), `docs/STATUS.md` (regenerate at end)

## 8. Test plan

| Test | Level | Verifies |
|---|---|---|
| `rel_1_non_finite_is_invalid` | unit | Phase 1 rejection |
| `rel_2_insufficient_history_blocks_fire` | unit | Phase 1 blocking (no 0.5) |
| `rel_3_identity_mismatch_invalidates` (×4: params/ver/schema/cost) | unit | Phase 2 |
| `rel_4_observation_creation_deterministic_id` | unit | Phase 3 |
| `rel_5_observation_parquet_roundtrip` | unit (storage) | Phase 3 |
| `rel_6_outcome_math_and_no_lookahead` | unit | Phase 4 |
| `rel_7_evaluation_gates_and_distribution` | unit | Phase 5 |
| `rel_8_raw_to_outcome_integration` | integration (sim) | raw → feature → signal → observation → outcome |
| `rel_9_reproducibility_same_inputs_same_bytes` | integration | deterministic observations + outcomes |
| `rel_10_golden_fixture` | integration | stable observation/outcome hash on a fixed fixture |
| Existing suite | all | no regression (esp. `sim_14_golden_hash_is_stable`, decision-log hash) |

## 9. Acceptance criteria

1. All Phase 1–6 changes implemented (or explicitly deferred with reason) —
   nothing deferred.
2. `cargo test` green across the workspace; the sim decision-log golden hash
   is unchanged.
3. New research artifacts (observations, outcomes, evaluations) are
   deterministic: same inputs ⇒ byte-identical outputs; stored as Parquet
   with the W-6 no-overwrite guard; footer carries the identity fingerprint.
4. No lookahead: outcomes attach post-run from recorded series only; open
   windows are `None`, never imputed.
5. Identity enforcement: any params/version/schema/cost-model change
   invalidates old grade evidence (tests prove it).
6. Live trading remains disabled; no new database; $0 budget respected.
7. `docs/research/RESEARCH-LAB-STATUS.md` created with PASS/FAIL per
   capability; `specs/054` + this plan updated in the same commits.
8. Guardrails pass and a self-review is recorded before push.

## 10. Re-audit against the 2026-09-07 anti-randomness task spec (Phase 7)

The 2026-09-04 implementation satisfied spec 054 (REL-1..REL-23), but the
target task spec is **stricter** in four confirmed places. Re-audit found:

| # | Defect | Rule | Resolution |
|---|---|---|---|
| D15 | Sample sizing was a single binary gate (`DEFAULT_MIN_N`) — no tier vocabulary, so 30 samples could promote exactly like 3,000 | R-5 | `SampleTier { Insufficient, Preliminary, Research }` + `RESEARCH_MIN_N = 100`; promotion requires `Research`; `SAMPLE_TIER_PRELIMINARY` reject (`features/src/evaluation.rs`) |
| D16 | No regime tagging — a signal that only works in one regime looked "generally effective" | R-6 | Regime buckets tagged from the fire snapshot's existing `regime.trend` feature (0.0=TREND/1.0=CHOP); `ONLY_WORKS_IN_*` refusal, `REGIME_COVERAGE_SINGLE_*` / `REGIME_SAMPLE_TOO_SMALL_*` flags (`REGIME_MIN_TAGGED = 10`) |
| D17 | No decay detection — evidence never aged | R-7 | Chronological-window net expectancy (`DEFAULT_DECAY_WINDOWS = 3`, `DECAY_MIN_PER_WINDOW = 5`); two consecutive recent non-positive windows ⇒ `DECAY_SUSPECT` refusal |
| D18 | Quality vocabulary had 4 states (Healthy/InsufficientHistory/Stale/Invalid); missing data was implicit and gaps were only staleness | R-1 | `DataQualityState` extended with `Missing` (never observed) and `Gap` (hole through the staleness window); screener/accumulation "never observed" ⇒ `Missing` (never neutral/0.5); gap heals only after fresh `min_samples` observations; Parquet store codes 4/5 with schema-version bump |
| D19 | Golden fixtures covered only clean feeds — no fixture exercised gaps, invalid values, or insufficient history | Mandatory tests | `rel_28_golden_dirty_fixture_gaps_invalid_insufficient_history` (sim/tests): fixed dirty feed walks Missing→Insufficient→Healthy→Gap→Healthy→Invalid with a frozen FNV hash |

Also closed against the task's structured-rejection format (R-8, kill-bias):
all reject reasons carry machine-readable codes (`INSUFFICIENT_SAMPLE`,
`SAMPLE_TIER_PRELIMINARY`, `NET_EXPECTANCY_NEGATIVE`, `NON_FINITE`,
`DECAY_SUSPECT`, `ONLY_WORKS_IN_*`, …) and `PromotionDecision::to_json`
emits `{"decision":"REJECT","reasons":["INSUFFICIENT_SAMPLE",…]}`.

Spec 054 amended with REL-24..REL-29 (PD-6: spec before code). Tests:
`rel_15/16/17/18/19` regression green, `rel_24` (tiers ×4), `rel_25`
(regime refusals ×3 + flags), `rel_26` (decay windows), `rel_27`
(Missing/Gap semantics ×4), `rel_28` (golden dirty fixture), `rel_29`
(reason codes + JSON shape), `rel_24` per-horizon rollup; full workspace
green including the unchanged sim decision-log golden hash.