# 005 — Backtester & Simulation

## Purpose
The honest evaluation machine: event-replay simulation with explicit fill
models, pessimistic costs, and statistical harnesses (walk-forward, Monte
Carlo). Its job is to kill bad ideas cheaply and to make live/backtest
divergence measurable.

## Scope
In: replay engine, fill models L0–L2, cost model, portfolio accounting,
metrics, walk-forward, Monte Carlo, experiment tracker, determinism check.
Out: L3 queue-position model (future spec), strategy API details (006).

## Design

```
Dataset reader (003) ─▶ SimClock event loop ─▶ FeatureEngine (004)
                                            ─▶ Strategy (006) ─▶ OrderIntent
                                            ─▶ RiskGate (007, same code)
                                            ─▶ FillSimulator ─▶ Fill events
                                            ─▶ Accountant ─▶ equity, positions
all of it ─▶ DecisionLog (append-only, hashable)
```

The sim uses the REAL risk gate and the REAL feature engine — only the fill
simulator and clock differ from live. Paper mode = live feeds + this same
FillSimulator (spec 007 wires it).

### Fill models (ladder)
- **L0 `BarFill`** — intents evaluated at next bar open ± `slip_ticks`
  haircut (default 1 tick + `slip_bps` 1bp); fills always complete. For
  daily/hourly strategies only.
- **L1 `TopOfBookFill`** — market intents fill at opposing best price at the
  first event with `recv_ts_ns ≥ intent_ts + latency_ns` (config, default
  150ms round-trip); size capped at displayed top-level qty × `participation`
  (default 0.5); remainder walks to next event (partial fills are real).
  Limit intents fill only when the opposing side TRADES through the price
  (trade-print rule — touching is not filling), always as taker-priced maker
  fee but flagged `optimistic_maker: true`.
- **L2 `DepthWalkFill`** — market intents walk BookMirror levels; impact is
  paid; book recovers naturally from subsequent deltas. Limit fills same
  trade-print rule but capped by traded volume at level × `queue_share`
  (default 0.25).
- All models emit `Fill {intent_id, price, qty, fee, liquidity: Maker|Taker,
  ts_ns}` and MUST respect symbol `step_size`/`min_notional`.

### Cost model (applies in every mode, sim and paper)
`fee = notional × fee_rate(venue, maker|taker)` from a checked-in fee table;
`slippage` per fill model; **funding**: perp positions accrue funding every
interval from Funding events (missing funding data ⇒ ERROR, not silent zero).
Reported metrics ALWAYS include a `2x-cost` stress column (SIM-8).

### Accounting
Positions per (strategy, venue, symbol); equity = cash + Σ pos × mark
(MarkPrice events; last trade if absent). Realized/unrealized split; Kahan
summation (CONV-7). Equity sampled to a curve at bar close.

### Metrics (per run, per strategy, per regime cell)
trades, hit rate, avg win/loss, **expectancy**, profit factor, max DD
(magnitude + duration), Sharpe & Sortino (bar returns, annualized), turnover,
fees paid, funding paid/earned, exposure %, and regime slicing by
`regime.vol × regime.trend` (FEA catalog).

### Harnesses
- **Walk-forward:** rolling (train `T`, test `t`) windows (default 90d/30d,
  step 30d); params fit in-window (grid from strategy's param space), applied
  out-of-window; report per-window OOS metrics + param stability table.
- **Parameter plateau:** for each param, re-run at ±10%, ±30%; flag if OOS
  expectancy sign flips within ±30% (curve-fit detector).
- **Monte Carlo:** resample trade sequence (block bootstrap, block = 1 day)
  1000×; report DD distribution; the sizing input is `p95(maxDD)` (RSK-5).

### Experiment tracker
Every run writes `runs/{run_id}/`: config.toml (full, hashed), git SHA, data
range + manifest coverage, metrics.json, equity.parquet, decision_log hash.
`runs/index.sqlite` for queries ("have we tried this?"). Run IDs are ULIDs.

## Requirements
- **SIM-1** Replay MUST consume the Dataset reader in global order and drive
  SimClock from event timestamps exclusively (no wall time anywhere).
- **SIM-2** Fill models L0/L1/L2 MUST implement the exact semantics above;
  the trade-print rule for limit fills is normative (touch-fills banned).
- **SIM-3** Latency injection (intent→venue, venue→fill-visibility) MUST be
  configurable per venue; default 150ms; 0 allowed only in unit tests.
- **SIM-4** Funding accrual MUST come from recorded Funding events; absence of
  funding data for a held perp position is a run-failing error (SIM refuses to
  produce numbers it knows are wrong).
- **SIM-5** The sim MUST run the production RiskGate and FeatureEngine crates
  unmodified (one code path; compile-time enforced by dependency, not copies).
- **SIM-6** A run MUST fail loudly (default) if any consumed stream's manifest
  coverage < `min_coverage` (default 0.995) — gaps poison silently otherwise.
  Override flag exists but is recorded in run metadata (honesty trail).
- **SIM-7** DecisionLog MUST record every (event_seq, feature updates consumed,
  intent, verdict, fill) with a stable hash; two runs with identical inputs
  MUST produce identical hashes (CONV-12).
- **SIM-8** Metrics output MUST always include the 2×-costs stress column;
  gate G1 (spec 006) reads it.
- **SIM-9** Walk-forward, plateau, and Monte Carlo harnesses MUST be CLI
  subcommands (`sim wf`, `sim plateau`, `sim mc`) writing tracker runs.
- **SIM-10** Experiment tracker MUST capture config hash + git SHA + data
  manifest hashes so any run is reproducible from the index alone.
- **SIM-11** A `sim replay-live` command MUST replay a live session's event
  log against the same config and diff DecisionLogs — the daily determinism
  check; any diff is a P1 bug.
- **SIM-12** Maker fills in L1/L2 MUST be tagged `optimistic_maker`; the
  metrics report shows maker-dependent P&L separately (upper-bound honesty).
- **SIM-13** Accounting MUST reconcile: cash + Σrealized + unrealized −
  fees − funding == equity, asserted continuously in debug builds and at
  run end always.
- **SIM-14** A golden fixture (checked-in small event log + config +
  expected DecisionLog hash) MUST run in CI (CONV-12 enforcement).

## Acceptance criteria
- [ ] Golden determinism fixture in CI (SIM-14); replay-live diff command works on a fixture (SIM-11).
- [ ] Fill model unit suites: L0 haircut math; L1 trade-print rule (touch ≠ fill), latency, partials; L2 depth walk with impact — each against hand-computed fixtures (SIM-2).
- [ ] Funding-missing run fails (SIM-4); low-coverage run fails without override (SIM-6).
- [ ] Accounting identity holds through a randomized property test of fills (SIM-13, CONV-22).
- [ ] `sim wf` on a toy strategy over fixture data produces per-window OOS table + plateau report (SIM-9).

## Decisions
- 2026-07-10: L3 (queue position) deferred to its own spec; until then maker-
  heavy strategies cannot pass gate G2 honestly and the funnel doc says so.

## Open questions
- None.
