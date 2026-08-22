# 017 — Screener Hit Journal & Grading Pipeline

## Purpose
Persist `ScreenerHit` to a durable journal, compute forward returns retroactively, and produce weekly grading reports so the funnel can promote/demote rules based on empirical performance.

## Scope
In: hit journal (JSONL), forward return computation (backfill), weekly grading job, promotion/demotion logic, human-readable + machine-readable reports. Out: screener rule logic (spec 004), feature materialization (spec 016), live trading decisions.

## Design

### Hit journal
Append-only JSONL per day:
```
journal/screener-hits/YYYY-MM-DD.jsonl
```

Each line:
```json
{
  "rule_id": "funding_extreme",
  "symbol": 0,
  "ts_ns": 1784456653319497000,
  "snapshot": {"funding_rate": 0.00015, "mark_price": 50000.0, "oi_notional": 1e9},
  "forward_return_1h": null,
  "forward_return_24h": null
}
```

`forward_return_*` fields are `null` at write time, backfilled later.

### Forward return computation
- Runs as a separate job (or lightweight query).
- For each hit with `null` forward returns, find the first price observation strictly after `ts_ns` (no look-ahead to the hit's own price), then compute `(price_at_T+N - price_at_entry) / price_at_entry` where `T` is the first post-hit tick.
- Backfill results are written to a separate backfill file, not modifying the original hit journal.

Write to a backfill journal:
```
journal/screener-hits/backfill/YYYY-MM-DD.jsonl
```
with same schema but `forward_return_*` populated. The grading job merges hit + backfill on `(rule_id, symbol, ts_ns)`, preferring backfill values when present.

### Grading job
Runs weekly (configurable). Produces per rule:
- Hit count
- Hit rate (hits / total evaluations)
- Average forward return per horizon
- Sharpe ratio of "buy the hit" strategy (long on hit, hold N hours)
- Recommendation:
  - **promote**: hit rate ≥ 60% AND Sharpe > 1.0
  - **demote**: hit rate < 40%
  - **kill**: hit rate < 20% OR (negative Sharpe AND hit rate < 50%)
  - **hold**: all other cases

### Report formats
- **Human**: Markdown summary with tables per rule.
- **Machine**: JSON for funnel API (`funnel promote/demote/kill`).

### No look-ahead guarantee
- Forward returns use the **first price observation strictly after** `ts_ns` as the entry price (never the price at `ts_ns` itself, which could be stale or coincide with the trigger tick).
- Price data is snapshotted at hit time — no future data leaks into the hit record.

## Requirements
- **GRD-1** `ScreenerHit` MUST be persisted to `journal/screener-hits/YYYY-MM-DD.jsonl` at hit time.
- **GRD-2** Forward returns (1h, 4h, 24h) MUST be computed retroactively and backfilled.
- **GRD-3** Grading job MUST run weekly (configurable), producing per-rule stats and recommendations.
- **GRD-4** Grading MUST use only data available at hit time (no look-ahead).
- **GRD-5** Results MUST be human-readable Markdown + machine-readable JSON.

## Acceptance criteria
- [x] Test: `test_res_2_forward_return_step_lookup_and_short_series_guard` — known price path, verify return (GRD-2 math)
- [ ] GRD-1: `ScreenerHit` journal writer (`journal/screener-hits/YYYY-MM-DD.jsonl`) — NOT implemented; the grading job consumes a materialized hit bundle (see Decisions 2026-08-17)
- [ ] GRD-2: backfill journal writer — NOT implemented (the forward-return math is; see Decisions 2026-08-17)
- [ ] GRD-4 letter check (`grd_3_no_look_ahead`): entry = first price strictly after `ts_ns` — NOT implemented; `grading.py` uses as-of-at-`ts_ns` (see Decisions 2026-08-17)
- [x] Test: `test_grd_4_grading_produces_report` — run on a week of hits, verify Markdown + JSON output (GRD-3/5)
- [x] Test: `test_grd_5_promotion_recommendation` — promote / demote / kill / hold thresholds (GRD-3)
- [ ] Integration: 30 days of hits, manual verification of 5 random samples

## Decisions
- 2026-07-19: Forward returns at 1h, 4h, 24h (configurable).
- 2026-07-19: Grading frequency: weekly (aligns with research ritual).
- 2026-07-19: Report format: Markdown for human, JSON for funnel API.
- 2026-07-19: Backfill uses a separate `backfill/` journal (immutable primary journal, append-only backfill).
- 2026-08-17: GRD-3/5 implemented for real (audit E4): `research/grading_job.py` now also emits a per-run Markdown report `{week}.md` with a "Next stage recommendations" section (GRD-3/5), alongside the machine `{week}.json`. Recommendation thresholds follow the spec's promote/demote/kill/hold rule with kill evaluated before demote (the stricter verdict wins); "hit rate" = wins / hits-with-computable-returns (total evaluations are not tracked — honest denominator, `grading.py`); Sharpe = per-period mean/std of per-hit excess returns, not annualized, scoring 0.0 on degenerate variance so a no-variance rule is never promoted (PD-5). Honest status of the rest: GRD-1 (hit-journal writer) and GRD-2 (backfill journal writer) are NOT implemented — the job consumes a materialized hit bundle and writes grades only; GRD-4's letter (entry = first price strictly after `ts_ns`) is approximated as as-of-at-`ts_ns` in `grading.py` (documented in its docstring). Those acceptance boxes stay unchecked until implemented; they are not silently claimed.

## Open questions
- None.
