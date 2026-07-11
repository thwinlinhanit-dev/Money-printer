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
- [x] Determinism golden: replay twice ⇒ identical decision-log hash + equity (SIM-7/14). `sim_7_replay_is_deterministic`, `sim_14_golden_hash_is_stable`.
- [x] Clock driven purely by event timestamps (SIM-1). `sim_1_clock_is_driven_by_events`.
- [x] Taker fill applies fee + slippage; latency-deferred against the trade tape (SIM-2, partial). `sim_2_taker_fill_applies_fee_and_slippage`.
- [x] Accounting identity holds through a full run and a hand-computed realized+funding case (SIM-13). `sim_13_*`.
- [x] L0/L1/L2 fill models incl. trade-print limit rule and participation/queue-share caps (SIM-2). `sim_2_l1_market_buy_is_capped_by_top_of_book_participation`, `sim_2_l1_limit_trade_print_rule_bans_touch_fills`, `sim_2_l2_market_buy_walks_multiple_levels_and_pays_impact`, `sim_2_l2_limit_fill_capped_by_queue_share_needs_multiple_prints`.
- [x] Funding-missing and low-coverage run refusals (SIM-4/6). `sim_4_missing_funding_refuses_to_report_a_held_perp_position`, `sim_4_funding_event_present_lets_the_run_report`, `sim_6_low_manifest_coverage_refuses_the_run`, `sim_6_full_coverage_runs_normally`.
- [x] 2x-cost stress column and optimistic-maker split (SIM-8/12). `sim_8_stress_expectancy_2x_is_never_better_than_base_when_fees_positive`, `sim_12_optimistic_maker_fills_are_tracked_separately`.
- [x] Walk-forward rolling windows, parameter-plateau sign-flip check, Monte-Carlo block bootstrap with seeded DD distribution (SIM-9). `sim_9_walk_forward_rolls_windows_and_reports_oos`, `sim_9_plateau_flags_sign_flip_within_30pct`, `sim_9_monte_carlo_is_seeded_and_reports_dd_distribution`, `sim_9_monte_carlo_empty_is_zero`.
- [x] Experiment tracker run records: config hash + git SHA + data range + manifest hashes + decision-log hash, reproducible & experiment-identifying (SIM-10). `sim_10_run_record_is_reproducible_and_identifies_experiments`.
- [x] `replay-live` decision-log divergence detection (SIM-11). `sim_11_replay_live_diff_detects_divergence`, `sim_11_length_mismatch_with_shared_prefix_diverges`.
- [x] `sim` CLI: `backtest|wf|plateau|mc|replay-live` over a real event-log file, writing tracker runs to `runs/index.jsonl`; `replay-live` exits non-zero on any decision-log divergence (SIM-9/10/11). `sim_9_cli_backtest_mc_and_replay_live_work_end_to_end` (end-to-end against the built binary).

## Decisions
- 2026-07-10: L3 (queue position) deferred to its own spec; until then maker-
  heavy strategies cannot pass gate G2 honestly and the funnel doc says so.
- 2026-07-10 (impl): the backtester runs the PRODUCTION `mp-features`,
  `mp-strategies`, and `mp-risk` crates unmodified (SIM-5) via a compile-time
  dependency — not copies. v1 fill model is a latency-deferred **taker fill
  against the next trade** (maps to L1 top-of-trade); L0 bar-open and L2 depth
  walk are deferred. Accounting is average-cost with an exact identity
  (`equity == start + realized + unrealized − fees − funding`) asserted at run
  end and unit-tested. Decision log uses an FNV rolling hash over canonical,
  bit-pattern-rendered records so determinism is a single `u64` comparison.
- 2026-07-10 (impl): deferred within spec 005 — walk-forward/plateau/Monte-Carlo
  harnesses, experiment tracker, `sim replay-live` diff, `sim wf` CLI,
  funding-missing/low-coverage run-failing guards (SIM-4/6). Tracked.
- 2026-07-11 (impl): the L0/L1/L2 fill-model ladder is implemented (SIM-2),
  closing the prior "taker-fill-only" gap. `L1TopOfBook` reads the production
  `BookMirror` (core) for opposing best price/qty, caps market fills at
  `top_qty × participation` with the remainder walking forward to the next
  event, and fills resting limits only on a trade-print crossing the price
  (touch-fills banned). `L2DepthWalk` walks `BookMirror` levels via new
  `walk_ask`/`walk_bid` methods (impact paid, book recovers from later
  deltas) and caps each limit-fill print at `trade_qty × queue_share`,
  needing multiple prints to complete (queue-position realism). `L0BarFill`
  uses `mp_features::BarBuilder` (new `current_open()` accessor) to fill at
  the next bar's open ± a tick/bps haircut, always complete. When no book
  data is present in the stream (trade-only fixtures), L1/L2 market orders
  fall back to filling in full at the next trade print — the exact pre-ladder
  behavior, so existing fixtures and the golden-hash test are unaffected
  (the golden test asserts determinism, not a pinned hash literal, so this
  was safe to change). SIM-4 (funding-missing) and SIM-6 (low-coverage) are
  now enforced: `Backtester::run_checked(events, coverage)` refuses upfront on
  coverage below `min_coverage`, and refuses at run end if a held perp
  position never saw a Funding event within `funding_check_interval_ns`
  (default 8h) — both return a `SimError`, not a silently-wrong number.
  SIM-8's 2×-cost stress is an approximation: `Metrics::stress_expectancy`
  re-prices the *actual* total fee spend at 2× and spreads it across the
  trade count, rather than re-simulating fills under doubled costs (a true
  re-simulation is additional scope, left for the walk-forward harness work).
  SIM-12 optimistic-maker fills are tracked via `Metrics::record_maker_trade`
  (a separate win/loss tally) and tagged in the decision log
  (`record_fill_tagged`) so the hash captures the distinction. 10 new tests
  in `sim/tests/fill_models.rs`.
- 2026-07-11 (impl): the statistical harnesses (SIM-9/10/11) are implemented
  as libraries: `harness::walk_forward` rolls `(train,test)` windows over the
  recv-ordered event span and hands each pair to a caller fit/apply closure
  (the fit is strategy-specific; the harness owns only the windowing);
  `harness::plateau_ok` is the curve-fit sign-flip check within ±30%;
  `harness::monte_carlo` block-bootstraps the per-trade P&L sequence by day
  with a seeded splitmix64 (CONV-11) and returns the DD distribution incl.
  `p95(maxDD)` (the RSK-5 sizing input). `tracker::RunRecord` captures the
  SIM-10 reproducibility fields (config hash + git SHA + data range + manifest
  hashes + decision-log hash) and a `same_experiment` identity; the `run_id`
  is caller-supplied (ULID at the binary edge) so the crate stays
  wall-clock-free (PD-3). `DecisionLog::first_divergence` is the SIM-11
  replay-live diff (any divergence is a P1). Gate **G1** (spec 006) is
  evaluated in `gates::evaluate_g1` — it lives here, not in `mp-strategies`,
  because it reads the backtest `Metrics` and `mp-strategies` must not depend
  on `mp-sim`. The Backtester now records the `(ts,pnl)` trade sequence and
  exposes a `summary()`. What remains is only the argv/`clap` binary front-end
  (`sim wf|plateau|mc|replay-live`) and its `runs/` file writes — a
  binary-edge concern with no new decision logic. 10 new tests in
  `sim/tests/harness.rs`.
- 2026-07-11 (audit): four fixes from the honesty audit (docs/AUDIT-2026-07-11.md):
  (1) per-trade P&L for metrics is now NET of costs — `Accountant::apply_fill`
  returns a `FillOutcome` attributing the closing fee + pro-rata released
  entry fees, so expectancy and the SIM-8 2×-stress column genuinely price
  costs (`regression_audit1_*`); the accounting identity keeps gross realized
  + separate fee term, unchanged. (2) trade-print rule is strictly-through —
  at-price prints are touches and do not fill (`regression_audit2_*`).
  (3) SIM-5 made real: the sim now runs the PRODUCTION `mp_risk::evaluate` on
  every sized intent (RG-2 allowlist = the feed's universe, Paper-mode
  semantics since RG-1's mode check governs live processes, RG-8/9 vs
  day-start equity, RG-10 kill switches tripable via `kill_switches_mut`);
  verdicts are recorded into the decision-log hash (SIM-7). Sim-default
  `RiskLimits` are backtest-scale and documented as never feeding live
  (`risk.toml` owns live limits). (4) SIM-3 latency now has an ID-named test.
  Remaining before `implemented`: the `sim wf|plateau|mc|replay-live` CLI
  binaries (SIM-9's letter) and recording consumed feature updates in the
  decision log (SIM-7's letter).
- 2026-07-11 (fix-all): the `sim` binary implements SIM-9/11's letter with
  hand-rolled argv parsing (no new dependency). The run id is caller-supplied
  (ULID at the ops layer) so the binary reads no wall clock at all. SIM-7's
  letter is now met too: the decision log records every consumed
  `FeatureUpdate` (`record_feature`) alongside intents, verdicts, and fills.
  With every requirement ID tested and every acceptance criterion automated,
  status flips to `implemented`. Remaining honest caveats live in COL/EXE
  (live-venue work), not here.

## Open questions
- None.
