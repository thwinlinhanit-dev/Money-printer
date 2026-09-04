# 054 — Research Lab Hardening

**Status:** Active (2026-09-04)
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

## Storage

- **REL-23** — Observations persist as Parquet (`mp-storage`), one row per
  (observation, outcome), zstd-6, footer KV = identity fingerprint + schema
  version, `{date}-{content_hash}.parquet` partitioning, W-6 no-overwrite
  guard, deterministic content hash. No new database.

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
observation hash fixture.