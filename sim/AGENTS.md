# sim

## Purpose

Deterministic backtesting engine that replays event logs through strategy + risk + OMS pipelines. Produces fill reports, P&L metrics, and decision journals.

## Ownership

- `src/engine.rs` — main backtest harness loop (stream() drives paper sessions)
- `src/harness.rs` — sim harness configuration
- `src/fills.rs` — fill models (taker, maker, slice)
- `src/account.rs` — simulated account tracking
- `src/tracker.rs` — position tracker
- `src/metrics.rs` — backtest metrics computation
- `src/decision_log.rs` — strategy decision journaling
- `src/gates.rs` — risk gate simulation
- `src/paper.rs` — paper mode: batch-fed Backtester with merge-key dedup (SIM-15)
- `src/error.rs` — error types
- `src/bin/sim.rs` — CLI entry point (backtest|wf|plateau|mc|replay-live|paper|paper-tail)
- `src/bin/gen_fixture.rs` — test fixture generator
- `tests/backtest.rs`, `tests/fill_models.rs`, `tests/harness.rs`, `tests/regressions.rs`

## Local Contracts

- Must produce identical results on repeated runs with same inputs (determinism enforced)
- Fills must respect venue-specific latency models from config
- Decision log entries must match the format consumed by research/grading
- Multi-strategy: `Backtester::from_strategies` runs several strategies against one shared simulated account; event handlers dispatch to every strategy in registration order with per-strategy RNG/timers/subscriptions, and intent ids are engine-namespaced so fills stay attributed (audit 08-04). `Ctx` position/equity are the shared account — strategies in one run observe the same positions.
- Paper mode (SIM-15): `sim paper` re-plays a recorded log through the same fill machinery in batches; `sim paper-tail` re-reads a growing log each poll, skipping already-consumed frames by `(recv_ts_ns, stream_seq)` merge key. The tail loop is time-free — it sleeps a fixed poll duration and counts polls (PD-3). All arms write tracker records via `record_run`. `paper-tail` builds the strategy's `Universe` from the log's current contents (read once up front, seeded as the first batch) — never from an empty `&[]`, which would leave universe-gated strategies (`carry-v1`) emitting zero intents (audit 2026-08-08).

## Verification

- `cargo test -p mp-sim`

## Child DOX Index

None.
