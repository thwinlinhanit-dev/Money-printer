# sim

## Purpose

Deterministic backtesting engine that replays event logs through strategy + risk + OMS pipelines. Produces fill reports, P&L metrics, and decision journals.

## Ownership

- `src/engine.rs` — main backtest harness loop
- `src/harness.rs` — sim harness configuration
- `src/fills.rs` — fill models (taker, maker, slice)
- `src/account.rs` — simulated account tracking
- `src/tracker.rs` — position tracker
- `src/metrics.rs` — backtest metrics computation
- `src/decision_log.rs` — strategy decision journaling
- `src/gates.rs` — risk gate simulation
- `src/error.rs` — error types
- `src/bin/sim.rs` — CLI entry point
- `src/bin/gen_fixture.rs` — test fixture generator
- `tests/backtest.rs`, `tests/fill_models.rs`, `tests/harness.rs`, `tests/regressions.rs`

## Local Contracts

- Must produce identical results on repeated runs with same inputs (determinism enforced)
- Fills must respect venue-specific latency models from config
- Decision log entries must match the format consumed by research/grading

## Verification

- `cargo test -p mp-sim`

## Child DOX Index

None.
