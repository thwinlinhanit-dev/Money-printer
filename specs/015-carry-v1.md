# 015 — Real Strategy Implementation (carry-v1)

## Purpose
Implement the first real `Strategy` trait implementor — a funding-rate carry strategy (`carry-v1`) — so the funnel has real logic to promote from hypothesis through backtest to paper.

## Scope
In: `CarryV1` struct, `Strategy` trait impl, funding rate tracking state machine, entry/exit thresholds, position sizing, hedge logic, hypothesis.md. Out: other strategy types (basis, momentum), cross-venue execution (Phase 1+), live order placement.

## Design

### Strategy state machine

```
IDLE → ENTRY_SIGNALED → ENTERED → EXIT_SIGNALED → IDLE
```

### Configuration
```rust
pub struct CarryConfig {
    pub entry_threshold: f64,         // default: 0.0001 (0.01% per 8h)
    pub exit_threshold: f64,          // default: 0.00002 (0.002% per 8h)
    pub vol_target: f64,              // annualized vol target for sizing
    pub max_gross_exposure: f64,      // fraction of portfolio
    pub max_hold_ns: i64,             // max hold time (14 days default)
    pub max_adverse_funding: f64,     // stop loss on accumulated funding (2% default)
    pub signal_timeout_ns: i64,       // cancel entry if not filled within N ns
}
```

### Entry logic
- Track funding rate per `(venue, symbol)`. If no funding event received within `signal_timeout_ns`, cancel the entry signal.
- When `|funding.rate| >= entry_threshold` (default: 0.01% per 8h ≈ 11% annualized):
  - **Short** if funding rate positive (positive funding means longs pay shorts → go short to receive funding).
  - **Long** if funding rate negative (negative funding means shorts pay longs → go long to receive funding).
- Emit `OrderIntent` with direction, size, and `reason: "carry-v1"`.

### Exit logic
- When `|funding.rate| < exit_threshold` (default: 0.002% per 8h ≈ 2.5% annualized):
  - Emit `OrderIntent` to close the position.
- Also exit on: `max_hold_ns` elapsed since entry, or `max_adverse_funding` accumulated.

### Sizing
- Vol-targeted: position size = `vol_target * portfolio_value / (asset_vol * sqrt(hold_time))`.
- No fixed contracts — uses `vol_target` from config.
- Respects `max_gross_exposure` (fraction of portfolio, STR-13): the strategy
  Ctx exposes no mark/price, so the notional cap is applied at the risk-unit
  layer — `risk_units()` clamps to `max_gross_exposure / per-unit-risk`
  (default 0.1 / 0.005 = 20 units). The engine risk gate remains the
  notional-level authority (RiskLimits). No new entry intent is emitted while
  a position is already held (single-position state machine).

### Hedge (Phase 1)
- If perp-perp opportunity exists on two venues with opposite funding directions:
  - Take long on negative-funding venue, short on positive-funding venue.
  - Net flat, collecting spread.
  - Not implemented in v1 (single-venue only).

### Hypothesis
`strategies/carry-v1/hypothesis.md` must contain:
- **Edge**: Perpetual swap funding rates mean-revert; market makers overpay for leverage during volatile periods. Retail longs consistently pay funding on altcoins during uptrends.
- **Expected regime**: Chop / range-bound. Carry thrives when price is flat but funding oscillates.
- **What kills it**: Funding flip (sudden reversal), sustained squeeze (one-sided funding for weeks), exchange policy change (funding cap, interval change).
- **Risk parameters**: max hold time 14 days, max adverse funding accumulation 2% of notional, stop on funding flip beyond 2 standard deviations.

## Requirements
- **STR-9** `strategies/src/carry_v1.rs` MUST define `CarryV1` implementing `Strategy`.
- **STR-10** carry-v1 logic MUST: track funding rate per venue/symbol; emit `OrderIntent` when funding extreme detected; exit when funding normalizes below `exit_threshold`; position opposite to funding sign.
- **STR-11** carry-v1 MUST have a complete `hypothesis.md` with edge, regime, kill conditions, and risk parameters.
- **STR-12** The strategy MUST pass the funnel: hypothesis → backtest → walk-forward → paper → live-small.
- **STR-13** carry-v1 sizing MUST respect `max_gross_exposure`: the risk-unit request clamps to `max_gross_exposure / per-unit-risk`, and no entry intent is emitted while a position is already held.

## Acceptance criteria
- [x] carry-v1 compiles and passes unit tests
- [x] Test: `str_9_carry_emits_intent_on_funding_extreme` — feed funding event, verify intent
- [x] Test: `str_10_carry_exits_on_normalization` — verify exit intent
- [x] Test: `str_10_carry_enters_on_negative_funding` — negative funding → long (opposite sign)
- [x] Test: `str_11_carry_hypothesis_is_complete` — parse hypothesis.md, verify edge/regime/kill/risk sections
- [x] Test: `str_12_carry_passes_funnel` — carry-v1 through hypothesis→backtest→walk-forward→paper→live-small (live-small human-gated)
- [x] Test: `str_13_carry_respects_max_exposure` — risk units clamp to the exposure cap; no intent while holding
- [ ] Backtest: run over 30 days of recorded data (or synthetic), verify expectancy > 0 before costs
- [ ] Walk-forward: 3 windows, verify OOS performance ≥ 50% of IS

## Decisions
- 2026-07-19: Start with single-venue (venue with working funding data, e.g. Hyperliquid) to avoid cross-venue execution complexity.
- 2026-07-19: Use L0 fill model (bar close) — carry is slow-moving.
- 2026-07-19: Size is vol-targeted, not fixed contracts.
- 2026-08-17 (fix-all): realigned the str_11/str_12 test IDs with their spec
  requirement letters (the old tests — negative-funding entry and
  feature-ignore — were named after requirements they did not test). The
  negative-funding test is now `str_10_*` (STR-10: opposite to funding sign);
  the feature-ignore test is an audit-C1 defense test without a requirement
  ID; `str_11_carry_hypothesis_is_complete` and `str_12_carry_passes_funnel`
  now implement their letters. Added STR-13 making the previously-dead
  `max_gross_exposure` config normative: `risk_units()` clamps to the
  exposure cap (Ctx exposes no mark, so the cap binds at the risk-unit layer;
  the engine risk gate stays the notional authority). Non-requirement test
  IDs str_13..15 (z-score entry, adverse-funding stop, vol-target sizing)
  were renamed to descriptive non-ID names.
- 2026-08-17 (fix-all): `check_entry` — the `is_none_or` rewrite of the
  entry-z gate must preserve the original `map_or(true, …)` None semantics:
  an undefined z (degenerate window) is NOT an extreme and must never enter
  (a constant rate must not become an entry signal).

## Open questions
- None.
