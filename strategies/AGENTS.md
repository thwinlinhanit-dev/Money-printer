# strategies

## Purpose

Trading strategy trait and concrete strategy implementations. Each strategy has a hypothesis document and a Rust implementation conforming to the `Strategy` trait.

## Ownership

- `src/strategy.rs` — `Strategy` trait definition (swing metadata per spec 035 SWG-3: `holding_period_bars()` default `[1,MAX]`, `rebalance_cadence()` default `Event` — legacy per-tick; swing strategies override with `Daily`/`FourHour` and the sim dispatches them on bar close only, SWG-7)
- `src/carry_v1.rs` — carry strategy implementation
- `src/orderflow_v1.rs` — order-flow imbalance strategy (book.depth gauge +
  tape.bps_delta alignment, spec 004/006)
- `src/liq_fade_v1.rs` — liquidation-fade strategy (liq.vol_*/liq.dist
  cascade exhaustion fade, spec 004/006; data gate: needs a bybit recording)
- `src/swing_range_reclaim_v1.rs` — swing range-reclaim strategy (spec 036
  SLQ-S): enters on confirmed `swing.sweep.*.stop.*` events, close-evaluated
  hard invalidation (bounded max loss per trade), T1 half-exit + breakeven +
  ATR trail; Daily cadence; single-position state machine like liq-fade-v1
- `src/funnel.rs` — funnel aggregator
- `src/examples.rs` — example strategies
- `src/bin/funnel.rs` — funnel CLI binary
- `carry-v1/hypothesis.md` — carry strategy hypothesis
- `orderflow-v1/hypothesis.md` — order-flow imbalance hypothesis
- `liq-fade-v1/hypothesis.md` — liquidation fade hypothesis
- `swing-range-reclaim-v1/hypothesis.md` — sweep-reclaim swing hypothesis
  (spec 036; honest data gate: daily-bar corpus too young for 30 OOS trades)
- `trend-breadth-v1/hypothesis.md` — trend breadth hypothesis
- `funding-arb-v1/hypothesis.md` — cross-venue funding spread hypothesis
  (data gate: two-venue same-underlying overlap ≥ 3 days, FARB-1..6)

## Local Contracts

- Each strategy must implement the `Strategy` trait from `strategy.rs`
- Strategy output must be consumable by `oms/` and `risk/`
- Hypothesis docs in strategy subdirectories are living documents updated with edge results
- `Transition::to_jsonl` never panics: a serialize failure is journaled as an explicit `{"error": …}` line (audit fix-all 2026-08-17)
- `entry_ts_ns` in strategy state is the SIGNAL timestamp, not the fill timestamp — hold/exit checks count from signal time (latency_ns shifts the actual fill)
- `subscriptions()` must be PREFIX forms, never glob (`"funding."`, not `"funding.*"`): the sim engine matches with `name.starts_with(sub)`, so `"funding.rate".starts_with("funding.*")` is false and the strategy is silently starved (fixed 2026-08-21 — carry-v1 ran every sim VACUOUS for this reason)
- carry-v1 thresholds are calibrated to hourly hyperliquid funding (observed span ±1.5e-5, median ≈ 1e-5): default entry 3e-6 / exit 1e-6, grid entry 2e-6..4e-6 (empirical boundary — entry 5e-6 starves the z-gate). The prior 5e-5..2e-4 calibration sat above the whole observed range and could never enter.

## Verification

- `cargo test -p mp-strategies`
- `cargo test -p mp-strategies --test strategy_funnel` (incl. `str_8_launch_strategies_have_written_hypotheses`, which now also enforces `swing-range-reclaim-v1`)

## Child DOX Index

None.
