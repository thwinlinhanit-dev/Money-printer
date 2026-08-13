# strategies

## Purpose

Trading strategy trait and concrete strategy implementations. Each strategy has a hypothesis document and a Rust implementation conforming to the `Strategy` trait.

## Ownership

- `src/strategy.rs` — `Strategy` trait definition
- `src/carry_v1.rs` — carry strategy implementation
- `src/orderflow_v1.rs` — order-flow imbalance strategy (book.depth gauge +
  tape.bps_delta alignment, spec 004/006)
- `src/liq_fade_v1.rs` — liquidation-fade strategy (liq.vol_*/liq.dist
  cascade exhaustion fade, spec 004/006; data gate: needs a bybit recording)
- `src/funnel.rs` — funnel aggregator
- `src/examples.rs` — example strategies
- `src/bin/funnel.rs` — funnel CLI binary
- `carry-v1/hypothesis.md` — carry strategy hypothesis
- `orderflow-v1/hypothesis.md` — order-flow imbalance hypothesis
- `liq-fade-v1/hypothesis.md` — liquidation fade hypothesis
- `trend-breadth-v1/hypothesis.md` — trend breadth hypothesis

## Local Contracts

- Each strategy must implement the `Strategy` trait from `strategy.rs`
- Strategy output must be consumable by `oms/` and `risk/`
- Hypothesis docs in strategy subdirectories are living documents updated with edge results

## Verification

- `cargo test -p mp-strategies`
- `cargo test -p mp-strategies --test strategy_funnel`

## Child DOX Index

None.
