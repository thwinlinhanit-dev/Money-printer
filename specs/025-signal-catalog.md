# 025 — Signal Catalog

## Purpose

Track every feature-level trading signal through a strict promotion ladder,
with automatic decay re-testing, so that only evidence-backed signals reach
the strategy funnel.  The catalog is the single source of truth for what a
feature is allowed to do and why.

## Scope

In: signal lifecycle (register → grade → retest → kill), the promotion
ladder, decay detection, the `signals` CLI, and the catalog file format.
Out: feature computation itself (spec 004), strategy-level funnel promotion
(spec 006), and live deployment decisions (the human plane, spec 018).

## Design

Catalog file: one JSON document, atomically replaced on every mutation
(never appended).  Each record:

```json
{
  "id": "SIG-A",
  "hypothesis": "whale taker imbalance predicts 1h direction",
  "params_hash": "abc123",
  "stage": "Tested",
  "grades": [{"run_id": "R1", "created_ts_ns": 1785e15, "horizon_ns": 3.6e12, "n": 40, "win_rate": 0.60, "avg_excess": 0.002}],
  "weekly_avg_excess": [0.002],
  "last_grade_ts_ns": 1785e15,
  "kill_justification": null
}
```

Stage ladder (forward only until terminal):

```
Hypothesis → Tested → Graded → Deployed
```

- `register` enters at `Hypothesis`; requires an id, a hypothesis statement,
  and the params hash of the feature configuration it refers to.
- `grade` pushes one `GradeSnapshot` and moves one rung up.  A grade is
  refused (a valid result, not a crash — PD-5) when: the signal is killed
  (terminal), `n < min-n` (default 30), `avg_excess <= 0` (no positive edge),
  the grade is stale (older than the re-test interval), or the promotion is
  to `Deployed` without `--human`.
- `retest` runs decay detection (RES-3 semantics): with ≥ 12 weekly means,
  trailing 4-week mean below half the trailing 12-week mean ⇒ decayed.
  Decay auto-demotes to `Hypothesis` — risk-off never needs a human.
- `kill` is terminal; requires a non-empty justification (the autopsy
  artifact, mirroring `strategies::funnel::Autopsy`).

Time discipline (PD-3/CONV-5): the CLI takes `--now-ns` for replay paths;
without it, the live edge uses the sanctioned `WallClock`.

## Requirements

- **SIG-1** Every signal MUST have an id, a hypothesis, and a params hash
  before it can be graded.
- **SIG-2** Promotion MUST be strictly forward through the ladder; a grade
  without positive edge or with too few samples MUST be refused, not
  recorded.
- **SIG-3** Decay detection MUST demote automatically (no human required)
  when the trailing 4-week mean falls below half the trailing 12-week mean,
  and MUST require at least 12 weekly grades before it can fire.
- **SIG-4** A killed signal MUST be terminal: no further grades or retests,
  and the kill MUST carry a non-empty justification.
- **SIG-5** The catalog MUST be a single file, atomically replaced on each
  mutation, and MUST round-trip through JSON losslessly.

## Acceptance criteria

- [x] `sig_1_register_requires_params_hash_and_hypothesis` proves register
  validates its inputs.
- [x] `sig_2_grade_ladder_requires_positive_edge_and_min_samples` proves
  refused promotions are hard errors.
- [x] `sig_3_decay_detection_demotes_automatically` proves the ≥12-week
  RES-3 rule and auto-demotion.
- [x] `sig_4_kill_is_terminal_and_needs_justification` proves the terminal
  state.
- [x] `sig_5_catalog_roundtrips_json_losslessly` proves the file contract.

## Decisions

- 2026-08-04: Weekly means are averaged per grade with `weekly_avg_excess`
  storing per-grade values; the decay window is grades, not wall-clock
  weeks, because the grading cadence is the evidence cadence.  No wall-clock
  reads on decision paths (PD-3).
- 2026-08-04: Promotion to `Deployed` requires `--human`; demotion and kill
  never do.  Mirrors the strategy funnel's asymmetry (spec 006).
- 2026-08-04: The funnel runs at the feature level (footprint/CVD rules,
  spec 004 catalog) and at the strategy level (spec 006); the catalog
  bridges them by recording which params hash each signal was graded under.

## Open questions

- Should grade `n` count trades or independent windows?  Currently it is the
  number of screener hits graded — revisit when hit volume grows.
