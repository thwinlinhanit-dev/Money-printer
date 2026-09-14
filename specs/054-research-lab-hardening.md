# 054 — Research Lab Hardening

**Status:** Active (2026-09-04; extended 2026-09-07 — Phase 7 anti-randomness gates)
**Budget:** $0 / free-tier (`docs/ZERO_COST_MODE.md`)
**Plan:** `docs/implementation/RESEARCH-LAB-HARDENING-PLAN.md`
**Status report:** `docs/research/RESEARCH-LAB-STATUS.md`

**Purpose**
Turn the existing Money-Printer into a deterministic, reproducible quantitative
research laboratory capable of measuring whether signals contain genuine
predictive value. This is a hardening/extension spec: it does NOT redesign the
system, adds NO new database, keeps live trading disabled (PD-1), and preserves
determinism (PD-3). New research artifacts are stored as Parquet (W-6
no-overwrite guard) and are deterministic and replayable.

## Phase 1 — Data Integrity

- **REL-1** — Non-finite values are REJECTED: a `DataQualityState::Invalid`
  is recorded per symbol and counted (never silently dropped, never emitted).
- **REL-2** — Insufficient history is EXPLICIT and BLOCKS signal firing:
  fewer than the minimum samples ⇒ `InsufficientHistory`; the signal does not
  fire. Missing/stale/invalid data is NEVER converted into neutral/0/0.5 —
  the accumulation detector's legacy `unwrap_or(0.5)` percentile fallback is
  removed.
- **REL-3** — Staleness is EXPLICIT: data older than the staleness window ⇒
  `DataQualityState::Stale`, which blocks firing. The screener exposes
  `InsufficientHistory` per (symbol, rule) when a condition's feature was
  never observed, instead of a silent "never satisfied".

## Phase 2 — Signal Research Identity

- **REL-4** — A canonical immutable `SignalResearchIdentity`:
  `signal_id`, `feature_version`, `data_schema_version`, `params_hash`
  (FEA-6/7), `cost_model_hash`. A deterministic fingerprint (FNV-1a) is the
  compact form stored on grades and in Parquet footers.
- **REL-5** — Evidence is only valid under the identity that produced it:
  `SignalRecord::apply_grade` REFUSES any grade whose identity fingerprint
  does not match the record's current identity (`IdentityMismatch`). Changing
  params, feature version, data schema, or cost model invalidates old grades —
  proven by tests. Pre-hardening grades (no identity) are refused loudly.
- **REL-6** — Hit/evidence snapshots carry the SUPPORT of the decision: the
  accumulation detector's hits include per-leg percentile + in-window sample
  counts (`evidence.oi_delta.percentile`, `evidence.oi_delta.samples`,
  `evidence.smart_flow.percentile`, `evidence.smart_flow.samples`,
  `evidence.insufficient_history`).
- **REL-7** — The cost model is part of the identity: `cost_model_hash`
  (taker fee, maker fee, slippage) hashes into the identity; a cost-model
  change invalidates evidence. `SimConfig` carries `cost_model_hash`
  (default = hash of its fee fields).

## Phase 3 — Signal Observation Engine

- **REL-8** — When a (zero-cost-compatible) signal fires, an IMMUTABLE
  `SignalObservation` is persisted: `observation_id`, identity, `timestamp_ns`,
  `symbol`, `venue`, `direction`, `feature_snapshot` (only the features that
  caused the fire), `quality`, `created_at_ns`. Stored as Parquet (REL-23).
- **REL-9** — Recording is quality-gated: a non-`Healthy` state BLOCKS the
  observation (insufficient history / stale / invalid never become a recorded
  signal); blocked fires are counted and observable, never silent.
- **REL-10** — Observations are deterministic and replayable: ids derive from
  FNV-1a(identity fingerprint ‖ timestamp ‖ per-engine sequence); identical
  replays ⇒ byte-identical observations (golden hash test).
- **REL-11** — The recorder is WRITE-ONLY side state in the backtester: the
  decision log's rolling hash is byte-identical with or without observation
  recording (regression-tested).

## Phase 4 — Forward Outcomes

- **REL-12** — Outcomes attach AFTER the fact with configurable horizons
  (default 15m / 1h / 4h / 1d): gross return, net return (gross − round-trip
  cost), MFE, MAE, hit (gross > 0).
- **REL-13** — An outcome exists ONLY when the recorded series extends to
  `ts + horizon` AND a mark exists strictly after the entry within the
  window; a shortened/empty window is NEVER fabricated into a full-horizon
  return.
- **REL-14** — No lookahead: entry is the last mark at-or-before the signal
  time (exactly what the signal could see); MFE/MAE use only post-signal
  marks; outcomes are computed post-run from the recorded mark series and
  cannot influence signal generation.

## Phase 5 — Evaluation & Promotion

- **REL-15** — Sample-size gate: fewer closed outcomes than `min_n` (default
  30) ⇒ refused with an explicit reason.
- **REL-16** — Gross vs Net expectancy: promotion requires POSITIVE NET
  (post-cost) expectancy; gross-only edge is refused with an explicit reason.
- **REL-17** — Basic distribution: p25 / p50 / p75 of net returns (nearest-rank).
- **REL-18** — Structured promotion decision artifact: `promote` + explicit
  reject reasons (a refusal is a valid result, PD-5).
- **REL-19** — The bridge to the existing catalog lifecycle:
  `grade_from_report` builds a `GradeSnapshot` (avg_excess = net expectancy,
  identity-stamped) ONLY from a passed gate; the catalog's stage/human/
  staleness/identity rules are unchanged and strengthened by REL-5.

## Phase 6 — Footprint / Accumulation cleanup

- **REL-20** — The market profile is DOCUMENTED as a bar-based approximation
  (whole bar volume placed at the bar midpoint, no intra-bar distribution) —
  never presented as an L2-equivalent footprint.
- **REL-21** — POC selection is deterministic on ties (lowest bucket wins,
  `total_cmp` — no silent NaN-equal fallback) across POC/VAH/VAL, tested
  across fresh engines.
- **REL-22** — Accumulation evidence representation improved (REL-6) WITHOUT
  breaking the existing strict AND logic (ACC-6) or the cooldown (ACC-4).

## Phase 7 — Anti-randomness gates (added 2026-09-07)

Extends Phase 5 with the task-mandated anti-randomness rules (R-5 tiers,
R-6 regime, R-7 decay) and hardens Phase 1's quality vocabulary. These gates
are deliberately KILL-BIASED: most ideas must die (R-8), so every gate below
refuses promotion when its evidence is absent — never silently passes.

- **REL-24 (R-5)** — Sample-size TIERS are mandatory:
  `Insufficient` (n < `min_n`, default 30) / `Preliminary` (min_n ≤ n <
  `RESEARCH_MIN_N` = 100) / `Research` (n ≥ 100). The evaluation report
  carries the tier; PROMOTION requires the Research tier — a Preliminary
  sample is refused with `SAMPLE_TIER_PRELIMINARY` (below min_n stays
  `INSUFFICIENT_SAMPLE`).
- **REL-25 (R-6)** — Regime tagging and reporting: observations whose fire
  snapshot carries the regime feature (default `regime.trend`: 0.0 = TREND,
  1.0 = CHOP, per the existing `TrendRegime` feature) are bucketed per regime
  with n / net-expectancy / win-rate in the report. When the overall gate
  would pass but the tagged evidence is regime-limited, promotion is REFUSED
  with `ONLY_WORKS_IN_<REGIME>` (a positive and a negative regime coexist),
  `REGIME_SAMPLE_TOO_SMALL_<REGIME>` (a regime has 0 < n < 10), or
  `REGIME_COVERAGE_SINGLE_<REGIME>` (every tagged observation falls in one
  regime — general effectiveness unproven). Untagged observations are covered
  by the overall gates and are not regime-gated.
- **REL-26 (R-7)** — Sustained-decay detection: the closed-outcome sample is
  split chronologically into `DEFAULT_DECAY_WINDOWS` = 3 equal chunks (each
  ≥ `DECAY_MIN_PER_WINDOW` = 5, else decay is not judged). If the TWO most
  recent chunks both have net expectancy ≤ 0, promotion is refused with
  `DECAY_SUSPECT` — an edge that existed once but recently died forces a
  retest, never a promotion.
- **REL-27 (R-1)** — `DataQualityState` gains the two missing states of the
  task vocabulary: `Missing` (the symbol/stream was NEVER observed —
  distinct from `InsufficientHistory`, which means some-but-fewer-than-min
  samples) and `Gap` (an inter-observation hole larger than the staleness
  window was detected; the state blocks until `min_samples` fresh post-gap
  observations arrive). Both block signal firing. Screener/detector
  "feature never observed" paths now report `Missing`. Parquet quality
  codes: legacy 0..3 unchanged, Missing = 4, Gap = 5 (old files decode
  byte-identically).
- **REL-28** — The golden dataset fixture covers the DIRTY cases, not just
  the happy path: insufficient history (cold start), a >staleness-window
  gap, and a NaN (invalid) value, plus a healthy segment — asserting blocked
  fires are counted, non-Healthy observations never exist, and the whole
  state hash is stable.
- **REL-29** — Reject reasons are machine-readable codes followed by human
  detail: `INSUFFICIENT_SAMPLE`, `SAMPLE_TIER_PRELIMINARY`,
  `NET_EXPECTANCY_NEGATIVE`, `DECAY_SUSPECT`, `ONLY_WORKS_IN_*`,
  `REGIME_SAMPLE_TOO_SMALL_*`, `REGIME_COVERAGE_SINGLE_*`, `NON_FINITE` —
  matching the task's structured decision format.
- **REL-30** — Paper-mode observation recording: the paper path (`sim paper`,
  `sim paper-tail`) exposes the SAME write-only `ObservationRecorder` the
  backtest path uses — `--params-hash` enables, `--obs-dir` persists Parquet,
  `--horizons` configures post-close outcome attachment. Recording is
  observational only: it must never perturb the decision path (the batched
  paper stream hash with recording on equals the hash with it off — G3/SIM-15
  holds), outcomes attach only after `close()` from the recorded mark  series (open windows stay empty, REL-14), and identities are stamped exactly as
  in backtest. Live trading remains disabled (PD-1).
- **REL-31** — Hit-journal migration: the legacy JSONL forward-return
  backfill (spec 017 GRD-2/GRD-4, gross-only, no identity) is retired as a
  RESEARCH artifact. `ScreenerHit`s gain a bridge onto the identity-stamped
  observation flow: each hit becomes a `SignalObservation` (params-hash
  identity, `regime.trend`-style snapshot passthrough, per-hit direction
  default Long) whose outcomes (gross AND net, MFE/MAE) come from the Phase-4
  outcome engine and persist as Parquet. The JSONL hit journal remains the
  WRITE-side fire log (append-only, W-6), but grading/evaluation reads the
  observation store. Legacy `HitRecord` JSONL keeps loading (serde defaults)
  so historical files never break; new backfill JSONL is no longer produced.

## Storage

- **REL-23** — Observations persist as Parquet (`mp-storage`), one row per
  (observation, outcome), zstd-6, footer KV = identity fingerprint + schema
  version, `{date}-{content_hash}.parquet` partitioning, W-6 no-overwrite
  guard, deterministic content hash. No new database.

## Decisions (W-5)

- **2026-09-07, REL-24 thresholds:** 30 / 100 are fixed consts
  (`DEFAULT_MIN_N`, `RESEARCH_MIN_N`), not config plumbing — the smallest
  change that satisfies R-5; config wiring is deferred.
- **2026-09-07, REL-25 regime source:** the fire snapshot already carries
  `regime.trend` for accumulation fires (the only strategy family with a
  regime input today), so tagging reads the snapshot instead of adding a
  new series. The task's example labels (LOW_VOL) map onto whichever regime
  feature is configured; TREND/CHOP are the only bands that exist.
- **2026-09-07, conflict resolution (task process rule):** existing tests
  treated n=30 as promotable; the task's R-5 makes Research (≥100) the
  promotion floor. Tests `rel_15/18/19` were STRENGTHENED to the stricter
  gate (a tightening, never a loosening — PD-5).
- **2026-09-07, REL-30 paper wiring:** the flag rides the existing
  `Backtester::enable_observations` + `PaperSession` — no new recorder code
  path; the binary extracts the research-report printer shared with the
  backtest arm. `paper-tail` enables recording before the poll loop with
  `created_at` = the first frame's `recv_ts_ns` (deterministic per replay,
  PD-3). Run `config_text` records `observations=<bool>` for the tracker.
- **2026-09-09, REL-30 tail-path incremental property:** `paper-tail` must
  record observations INCREMENTALLY on a growing log, not only at close:
  each poll re-reads the log, SIM-15 dedup consumes only new frames, and the
  recorder accumulates mid-stream (the poll line prints `obs=` /
  `obs_blocked=`). A non-growing log idles out with ZERO observations — a
  tail that sees no data fabricates nothing (R-1). Regression
  `rel_30_paper_tail_records_observations_incrementally` proves per-poll
  growth, early-fire BLOCKING before sufficient history (R-1), and that the
  tailed observation set + decision-log hash equal a one-shot replay of the
  completed log. Live proof on a growing 20→30 MB slice: obs 0→9→139→146
  mid-stream, session ends on idle, close attaches outcomes and refuses
  honestly (SAMPLE_TIER_PRELIMINARY + NET_NEG + DECAY).

- **2026-09-07, REL-31 relocation + conflict resolution:** the `footprint`
  study binary moves `features/src/bin` → `storage/src/bin` because
  `mp-storage` (which depends on `mp-features`, never the reverse) owns all
  Parquet writes — no dependency cycle. ENTRY-CONVENTION conflict: spec 017
  GRD-4 enters on the first trade STRICTLY AFTER the hit (execution-shaped:
  you cannot fill on your own trigger tick), while REL-14's outcome engine
  enters on the last mark at-or-before the fire time (measurement-shaped:
  the mark the signal could see). The observation flow's convention governs
  research artifacts (tested, used by sim/evaluation); GRD-4 remains binding
  for execution/fill studies. Direction: a screener hit carries no side, so
  the bridge defaults `Direction::Long` and the identity's params-hash must
  disclose it (documented, never hidden).
- **2026-09-07, REL-25 fire-context capture:** the sim `ObservationRecorder`
  captures the last-seen FINITE `regime.trend` value per symbol from the
  feature stream and enriches every fire snapshot (write-only side state;
  the triggering update wins when it IS the regime feature). The sim binary
  registers `TrendRegime` (lookback 20 / threshold 0.25 — the materialized
  swing defaults) on the engine's bar stream so research runs carry regime
  context; TEST engines do not register it, so the golden decision-log
  hashes are unaffected. Re-registered research runs produce NEW hashes by
  design — the feature stream grew; re-run ids carry an `-r2` suffix. A
  regime-void fire (feature never seen for that symbol) carries NO tag, and
  the R-6 gate stays silent rather than inventing regimes (R-1). First real
  result: no `ONLY_WORKS_IN_*` fired because NO regime is net-positive
  (carry negative in both regimes; orderflow's gross edge concentrates in
  TREND — win 0.534 vs 0.498 — but net-negative in both).
- **REL-32** — Venue-generic noise control: `coinflip-any` — a coinflip
  variant whose subscription matches ANY `cvd.{venue}` feature (the engine's
  per-venue CVD family exists for every venue with trades), fires on a
  seeded-rate SAMPLE of qualifying updates (fires/second is a parameter,
  default 1/100 updates so a 4-day tape yields thousands of fires without
  drowning the gate), with direction from the same seeded `ctx.next_u64()`
  (CONV-11: seeded, deterministic, edge-free). Purpose: the R-8 control —
  a strategy that MUST be rejected at every horizon; a pipeline that lets
  it pass is broken. Legacy `coinflip` (fixed `cvd.bybit` subscription,
  fire-on-every-update) is unchanged for golden-test stability.
- **REL-33** (2026-09-09, found by the PAP-12 smoke) W-6 idempotency must
  hold across the Parquet roundtrip AND across re-runs with divergent
  content. Two defects fixed:
  (a) the snapshot column is JSON TEXT; the f64→decimal→f64 cycle is not
  bit-exact for long-decimal values (1-ULP drift), so the read-back content
  hash never matched the in-memory hash and every identical re-run was
  refused. `observations_content_hash` now hashes each snapshot value in its
  storage-canonical form — the f64 that actually survives the column
  (`parse(ryu(value))`) — so in-memory and read-back hashes agree by
  construction.
  (b) `partitioned_write` keyed the guard on the FILENAME hash; divergent
  content landing on the same date silently created a duplicate
  `date=D-hash2.parquet` sibling instead of faulting. The guard now scans
  all `date=D-*.parquet` siblings and refuses ANY divergent content on the
  same identity+date (R-2 append-only). Identical re-writes remain no-ops.
  Observation-semantics goldens (REL-10/REL-28) are unaffected: they hash a
  different function (serde_json serialization), not this store hash.
- **REL-34** (2026-09-09) — the PYTHON grading arm must read the SAME
  identity-stamped observation Parquet the Rust store writes — one artifact
  store, two consumers, never a parallel export. `research/observation_store.py`
  owns the Python side of the contract:
  (a) the column schema (names + Arrow types) is declared as constants;
  loading refuses any file whose schema diverges;
  (b) the identity fingerprint is RE-VERIFIED cross-language: Python
  re-implements FNV-1a (offset `0xcbf29ce484222325`, prime `0x100000001b3`,
  wrapping) over signal_id bytes ‖ feature_version LE ‖ data_schema_version
  LE ‖ params_hash bytes ‖ cost_model_hash bytes and must reproduce the
  on-disk directory name for every loaded file — a mismatch is a hard error
  (a store the grader cannot cryptographically place must not be graded);
  (c) a load spanning files from >1 fingerprint raises — R-3 grades are
  per-identity or nothing;
  (d) the on-disk layout is ONE ROW PER (observation, outcome horizon):
  outcome-less observations store one all-NULL-`outcome_*` row; horizon
  selection therefore happens by `outcome_horizon_ns` with `observation_id`
  as the row-identity key (dedup on re-load is a loader invariant).
  Grading consumes the store's precomputed `outcome_net_return` (R-4: net is
  the primary metric) at the engine's horizons; rows with a NULL outcome at
  the requested horizon are skipped with an explicit skipped count (open
  windows — honest denominator, never imputed, R-1). `RuleGrade` gains
  `signal_id`/`identity_fingerprint`; the journal row carries the full
  identity so grades from different identities can never be compared
  implicitly. Quality-gated observations (code != 0) are EXCLUDED with a
  count, not folded into the sample.
- **REL-35** (2026-09-09, found by the REL-34 cross-check on the footprint
  study) `partitioned_write` took the identity fingerprint from
  `observations[0]` and filed EVERY observation under it — correct for the
  sim's single-identity batches, silently mis-filing multi-identity batches
  (the footprint study writes one identity per rule): all rows landed in
  observations[0]'s directory while carrying their true identity columns, a
  store the Python loader must refuse. Fix: group by identity fingerprint
  FIRST, then by UTC date — each identity's observations land in their own
  directory, and a multi-identity write yields per-identity date partitions
  that each load cleanly. Regression test writes a two-identity batch and
  asserts per-identity directories + clean single-identity loads.

## Out of scope (explicitly deferred)

- New database system (Parquet only).
- Bootstrap CIs / FDR correction.
- Complex multi-regime taxonomy.
- Trade-level footprint requiring full L2.
- Live trading (PD-1 — unchanged).

## Tests

Unit: identity invalidation (×4 dimensions), quality states, observation
creation/determinism, outcome math + no-lookahead, evaluation gates +
distribution, Parquet round-trip + W-6, POC/VAH/VAL tie determinism.
Integration (`sim/tests/observation_engine.rs`): raw → feature → signal →
observation → outcome; reproducibility; no-lookahead (open windows never
fabricated); recorder-write-only (decision-log hash unchanged); golden
observation hash fixture; golden DIRTY fixture (REL-28: insufficient
history + gap + invalid NaN + healthy segment, blocked fires counted,
frozen hash).

Phase 7 unit: tier classification + `SAMPLE_TIER_PRELIMINARY` refusal
(REL-24); regime buckets + `ONLY_WORKS_IN_*` / `REGIME_SAMPLE_TOO_SMALL_*`
/ `REGIME_COVERAGE_SINGLE_*` refusals (REL-25); decay-chunk detection +
`DECAY_SUSPECT` (REL-26); `Missing`/`Gap` tracker states incl. heal-after-
min-samples + Parquet code roundtrip (REL-27); machine reason-code prefixes
(REL-29).