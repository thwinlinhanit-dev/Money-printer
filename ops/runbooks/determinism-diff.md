# determinism-diff (P2)

The daily determinism check (spec 018 MOD-9..11, `mp-determinism`) replayed a
recorded day through the PRODUCTION runtime (features -> strategy -> risk) and
the two fresh runs produced **different** decision logs. Online == offline is
the system's core trust assumption (SIM-11): if replay is not byte-identical,
the backtest is not measuring the live edge.

## What triggers it

- `mp-determinism --date <day> --write` exits non-zero — run every morning by
  `daily_maintenance.sh` (VPS cron) and `daily_pipeline.ps1` (Windows task),
  step 1.5, fail-closed: no cold writes, promotion blocked for that day.
- A promotion window whose `<date>.determinism.json` artifact is missing,
  corrupt, or FAILED is held back — `mp-ops promote`/`status` name the days via
  `determinism_failures` and `determinism_ok: false`.

## Artifacts

- `<data>/scorecards/<date>.determinism.json` — verdict artifact (written by
  `--write`; two runs must both PASS and match).
- Replay uses the SAME log loader and input set as the materializer
  (`load_logs_merged`, spec 018 MOD-10), so replay inputs == feature inputs.

## Diagnosis

This is a correctness bug, not a market event. From the binary's stderr /
`pipeline.log`:

1. Identify the first diverging event and decision (the check prints the first
   divergence line).
2. Look for wall-clock reads, unseeded RNG, or hashmap-order iteration on a
   decision path (PD-3) — the usual suspects in the strategy/risk/features
   crates.

## Remediation

- Do NOT loosen the check to silence it (PD-5). Freeze promotions, capture the
  minimal replay, and fix the non-determinism at its source. Add a
  `regression_<issue>` test.
- After the fix: rebuild, re-run the check for the affected day(s)
  (`mp-determinism --date <day> --write --force`), and confirm the window
  promotes again.

## Escalation

Any determinism diff on a live strategy ⇒ `/kill <strategy>` until root-caused;
the edge is unmeasurable while online≠offline. A missing binary on the gate
host is NOT a pass — the gate host must have `mp-determinism` installed (the
scripts fail-closed when it is absent).
