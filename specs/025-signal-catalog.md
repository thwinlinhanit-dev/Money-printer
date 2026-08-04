# 025 — Signal Catalog

## Purpose

Apply the strategy-funnel asymmetry to *features* rather than strategies: every
signal (a rule over one or more features, or a single feature family such as
`footprint.imb.*`) gets a lifecycle — Hypothesis → Tested → Graded → Deployed —
with automatic decay re-testing and a terminal kill. **Promotion needs
evidence; demotion is automatic** (mirrors `strategies/funnel.rs` STR-3/5/6).
This is the bookkeeping substrate for the screener hit journal grading loop
(spec 017): the Rust arm and the Python research arm must agree on what a
"decayed" signal means.

## Scope

In: the pure Rust signal-record state machine, promotion/demotion policy,
decay math, and a serde persistence contract (`signals` CLI owns file I/O).

Out: orchestrating the grading run itself (that is `research/grading.py` +
`features` `footprint` bin feeding batches in), and any venue/network access.
This module is **pure**: no I/O, no wall clock (PD-3). `now_ns` is always a
parameter; persistence is serde at the binary edge.

## Design

Lifecycle stages, ordered by rank: `Hypothesis`(0) → `Tested`(1) → `Graded`(2)
→ `Deployed`(3). `Decayed` is a transient marker (rank 0, like Hypothesis) used
by the decay path before automatic demotion. `Killed` is terminal (rank −1).

- One grading batch (`GradeSnapshot`) records `run_id`, `created_ts_ns`,
  `horizon_ns`, `n`, `win_rate`, `avg_excess`.
- Promotion advances **exactly one** stage and only when the batch has ≥
  `min_n` samples **and** positive `avg_excess`, and the previous grade is not
  stale.
- `Deployed` additionally requires an explicit human flag — agents can never
  pass the final gate (mirrors G3/G4 in spec 005/006).
- Decay re-test uses the trailing weekly-mean series with the same math as
  `research/grading.py::decay_flag` (RES-3): needing ≥ 12 weeks, positive
  trailing 12-week mean, and a trailing 4-week mean below half of it.
- Persistence: one catalog, one JSON file, atomically replaced (never
  appended) by the `signals` binary (`features/src/bin/signals.rs`).

## Requirements

- **SIG-1** A signal MUST be registered with a non-empty id and a non-empty
  hypothesis; a duplicate id MUST be rejected; a new signal MUST start in
  `Hypothesis` with no grades.
- **SIG-2** Promotion MUST advance exactly one stage per grading batch and only
  when the batch has ≥ `min_n` samples and positive mean excess; promotion to
  `Deployed` MUST require an explicit human flag.
- **SIG-3** Decay detection (≥ 12 weekly points, positive trailing-12 mean,
  trailing-4 mean < half of trailing-12) MUST automatically demote the signal
  to `Hypothesis` (risk-off never needs a human).
- **SIG-4** Kill MUST require a non-empty justification (the autopsy artifact)
  and MUST be terminal.
- **SIG-5** A grade older than the re-test interval MUST refuse further
  promotion until a fresh re-test batch is applied.

## Acceptance criteria

- [x] `sig_1_register_requires_hypothesis` proves SIG-1 register rules and
  empty/duplicate id handling.
- [x] `sig_1_catalog_serde_roundtrip_and_duplicate_refusal` proves SIG-1 catalog
  serde persistence round-trip and duplicate refusal.
- [x] `sig_2_grade_promotes_only_with_evidence` proves SIG-2 min-samples,
  positive-edge, one-stage-at-a-time, and human-gated Deployed rules.
- [x] `sig_3_decay_detection_demotes_automatically` proves SIG-3 decay demotion
  (and that a young or never-positive series never flags).
- [x] `sig_4_kill_is_terminal_and_needs_justification` proves SIG-4.
- [x] `sig_5_stale_grade_refuses_promotion_until_retest` proves SIG-5.

## Decisions

- 2026-08-04: This spec is written retroactively for a WIP branch that already
  carried `features/src/signal_catalog.rs`, `features/src/bin/signals.rs`, and
  the `mp_features::signal_catalog` module — the code predates the spec (a
  PD-6 violation this spec repairs). Status is marked **implementing** until the
  decay scheduler and the Python research arm are wired to drive batches
  end-to-end; the pure state machine itself is implemented and unit-tested.
- 2026-08-04: `Decayed` is a transient marker, not a durable stage; callers
  journal it, then `demote(Hypothesis)` runs automatically.

## Open questions

- Who schedules the weekly decay re-test and the grading batches (research
  arm vs `opsd` timer)? Needs human/config decision before this can be fully
  "implemented".
