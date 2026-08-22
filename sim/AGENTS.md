# sim

## Purpose

Deterministic backtesting engine that replays event logs through strategy + risk + OMS pipelines. Produces fill reports, P&L metrics, and decision journals.

## Ownership

- `src/engine.rs` — main backtest harness loop (stream() drives paper sessions); SWG-5 bar-return equity sampling at each `bar_tf_ns` boundary; SWG-7 bar-close dispatch gating — `Daily`/`FourHour`-cadence strategies get feature updates and timers ONLY at bar boundaries, mid-bar timers defer to the next boundary (timer→strategy attribution in `pending_timers`/`queued_timers`), legacy `Event` cadence is never gated; SWG-6 wires `max_concurrent_positions` (distinct held symbols) and `corr_adjusted_exposure_notional` (resulting gross under perfect-correlation worst case — the sim has no cross-asset correlation model) into the gate's RG-12/RG-13 inputs
- `src/harness.rs` — sim harness configuration + walk-forward grid selection (`pick_best_eligible`: min-trades eligibility filter, SIM-9 integrity — a 0-trade combo scoring 0.0 must never beat a trading combo, nor count as a selection); SWG-5 purged splits (`WalkForwardParams.embargo_ns` — the gap between train and test is in NEITHER slice) + `MetricsSummary.sharpe`/`deflated_sharpe` (Bailey & López de Prado, probit = Acklam approximation)
- `src/fills.rs` — fill models (taker, maker, slice)
- `src/account.rs` — simulated account tracking
- `src/tracker.rs` — position tracker
- `src/metrics.rs` — backtest metrics computation; SWG-5 bar-return Sharpe (sampled once per bar boundary at `bar_tf_ns`, warmup-gated — None never NaN) + Deflated Sharpe
- `src/decision_log.rs` — strategy decision journaling
- `src/determinism.rs` — daily decision-determinism check (spec 018 MOD-9..11): `check_day` over a materialized slice; `check_day_streamed`/`replay_stream` over a per-run event-source factory (the RAM guard — the eager slice path OOM-killed the 952 MB VPS box; both paths must produce byte-identical verdicts)
- `src/bin/mp-determinism.rs` — spec 018 gate binary; streams the day via `storage::stream_logs_merged` (same canonical order as the materializer), writes the `{date}.determinism.json` artifact
- `src/gates.rs` — risk gate simulation
- `src/paper.rs` — paper mode: batch-fed Backtester with merge-key dedup (SIM-15)
- `src/error.rs` — error types
- `src/bin/sim.rs` — CLI entry point (backtest|wf|plateau|mc|replay-live|paper|paper-tail); `wf` takes `--min-trades N` (default 10), `--embargo-ns` (SWG-5 purged split), `--bar-tf-ns` and `--bars-per-year` (SWG-5 bar replay / Deflated Sharpe), and emits an explicit `VACUOUS` verdict per window when no grid combo trades enough to judge
- `src/bin/gen_fixture.rs` — test fixture generator
- `tests/backtest.rs`, `tests/fill_models.rs`, `tests/harness.rs`, `tests/regressions.rs`, `tests/swing_execution.rs` (SWG-7 bar-close dispatch: `swg_7_*`)

## Local Contracts

- Must produce identical results on repeated runs with same inputs (determinism enforced)
- Fills must respect venue-specific latency models from config
- Decision log entries must match the format consumed by research/grading
- Costs are real: maker-priced fills pay `SimConfig.maker_fee` (default 0.0002), taker fills pay `taker_fee`; funding accrues to held positions and is released pro-rata on reduce/close/flip, so per-trade NET P&L = gross − fees − funding (audit fix-all 2026-08-17). The SIM-13 identity (`equity == start + realized + unrealized − fees − funding`) is unchanged.
- The trade-level optimism tag (`Metrics` SIM-12 buckets) is the worst-case across the position's legs — `FillOptimism::merge` + `Position.optimism` — so a maker-optimistic entry closed by a taker fill still lands in the G1 maker bucket (B4). Per-fill tags in the decision log stay per-fill.
- Timers fire at-or-after their scheduled time (`t <= now`).
- Tracker run records (`record_run`): the backtest arm's `config_text` includes strategy, seed, coverage, and the carry overrides — runs differing only in params must not collide in the experiment identity (M6).
- Multi-strategy: `Backtester::from_strategies` runs several strategies against one shared simulated account; event handlers dispatch to every strategy in registration order with per-strategy RNG/timers/subscriptions, and intent ids are engine-namespaced so fills stay attributed (audit 08-04). `Ctx` position/equity are the shared account — strategies in one run observe the same positions.
- Paper mode (SIM-15): `sim paper` re-plays a recorded log through the same fill machinery in batches; `sim paper-tail` re-reads a growing log each poll, skipping already-consumed frames by `(recv_ts_ns, stream_seq)` merge key. The tail loop is time-free — it sleeps a fixed poll duration and counts polls (PD-3). All arms write tracker records via `record_run`. `paper-tail` builds the strategy's `Universe` from the log's current contents (read once up front, seeded as the first batch) — never from an empty `&[]`, which would leave universe-gated strategies (`carry-v1`) emitting zero intents (audit 2026-08-08).

## Verification

- `cargo test -p mp-sim`
- SWG-5 acceptance: `swg_5_sharpe_and_deflated_sharpe`, `swg_5_probit_is_monotone_and_symmetric` (metrics.rs), `swg_5_summary_reports_bar_return_sharpe` (backtest.rs), `swg_5_walk_forward_purged_splits_embargo_gap` (harness.rs)
- SWG-7 acceptance: `swg_7_swing_strategy_dispatched_only_on_bar_close`, `swg_7_event_cadence_strategy_keeps_per_event_dispatch`, `swg_7_swing_timer_deferred_to_bar_close_not_dropped` (swing_execution.rs)

## Child DOX Index

None.
