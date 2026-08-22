# Data eligibility — research-lab roadmap Phases 2.3 / 2.4

A research run may only consume days that pass the daily integrity gate, and the
run **record** must list every excluded day and *why*. A green collector status
is not sufficient if a study becomes biased by exclusions.

This gate reads the scorecard archive (`data/scorecards/{date}.json`) and
classifies every `(venue, symbol, date)` as eligible or excluded.

## Rule (defaults = the Phase-0 promotion bar, spec 024)

A day is **eligible** when it passes **all** of:

- `coverage >= 0.995`
- `clean == true` (no blocking findings)
- `stale_bursts == 0`

An optional `max_worst_gap_ns` budget can additionally reject long single gaps.
A date with **no scorecard** is treated as excluded with reason
`"no scorecard"` — an ungraded day is an untrusted day, never silently admitted.

## CLI

```sh
# Prose summary for the research window
py -3 research/run_eligibility.py --universe hyperliquid:BTC hyperliquid:ETH \
    --from 2026-08-13 --to 2026-08-17

# JSON summary (for a report / pipeline)
py -3 research/run_eligibility.py --universe hyperliquid:BTC \
    --min-coverage 0.98 --no-require-clean --json

# Journal an eligibility run record (append-only, duplicate-refusal)
py -3 research/run_eligibility.py --all --run-id elig-2026-08-17
```

Fail-closed by default: if the requested universe has **zero** eligible days the
CLI exits `2` (scheduled jobs never silently run on data that fails the gate).
Pass `--no-require-eligible` to inspect exclusions interactively.

## Embedding in a run record

`EligibilityReport.embed(run_id=..., git_sha=...)` returns a JSON-serializable
fragment with `eligible_days`, `excluded_days`, `excluded` (each day + reasons),
and `missing_scorecard_days`. Append it to the experiment's record in
`runs/index.jsonl` so a reviewer can reproduce the data gate from the same
scorecards (roadmap Phase 2.4 exit criterion).

## Library API (`research/mp_data/eligibility.py`, pure stdlib)

```python
from mp_data.eligibility import load_scorecards, select, EligibilityRule

grades = load_scorecards("data/scorecards")
rep = select(grades, [("hyperliquid", "BTC")], ["2026-08-13", "2026-08-14"])
print(rep.eligible_count(), rep.excluded_count())
for e in rep.excluded:
    print(e.venue, e.symbol, e.date, e.reasons)
```

Deterministic (no wall clock), pure stdlib, research-only (never on a live
decision path, CONV-2).
