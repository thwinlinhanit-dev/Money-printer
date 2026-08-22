# Hypothesis — swing-range-reclaim-v1

**Spec:** 036 (`specs/036-volume-profile-liquidity.md`, SLQ-S)
**Stage at writing:** Idea (registered into the funnel with a complete
hypothesis; promotion is gated by the standard pipeline, not by this doc).

## Edge claim (falsifiable)

In crypto perp markets, a compressed daily range acts as a liquidity shelf:
resting stops and breakout orders pile just outside both boundaries. A wick
that pierces a boundary on elevated volume (a *sweep*) and then CLOSES back
inside the range within 2 daily bars signals that the breakout failed and the
swept side's liquidity was consumed. Price then tends to revert across the
range toward the opposite boundary, more so when the opposite side of the
profile is thin (LVN — low-volume nodes offer little friction).

Testable claim: entering AFTER confirmation of a sweep-reclaim event
(`swing.sweep.{low|high}.stop.*` emission) and exiting per the spec-036 rule
set produces positive expectancy net of fees/slippage on out-of-sample daily
data, with drawdown within the risk budget.

## Entry / exit summary (spec 036 §3 is authoritative)

- Long: confirmed LOW sweep-reclaim. Short: mirror.
- Hard invalidation: daily CLOSE through the event's stop price
  (extreme ∓ 0.5·ATR). Close-evaluated only; no intrabar stops. Every entry
  therefore carries a bounded, defined max loss at entry time — the EDGE hard
  gate that permanently disqualifies ladder/martingale sizing variants.
- T1 = closer of {opposite range boundary, nearest LVN beyond it}; exit half;
  stop to breakeven; remainder trails at `trail_atr`·ATR toward the next
  HVN/LVN; time stop after 60 held bars.
- Sizing: standard risk-units convention (risk gate owns contracts); one
  entry, one defined risk.

## Falsification / grading procedure

Standard funnel gates apply (G1 cost sanity, walk-forward ≥2-of-3-window OOS
consistency via `ops/scripts/grade_wf.py` semantics, signal-catalog decay
re-tests). Additionally:

- KILL if the strategy's expectancy is non-positive on the pooled OOS windows
  once the corpus supports grading.
- NOT-YET-FALSIFIED (and explicitly **no data**) until ≥30 OOS trades exist.

## Honest data gate

Daily bars are materialized from recordings that started 2026-08-12
(hyperliquid BTC/ETH canonical; bybit legs joined later). The current corpus
cannot produce 30 OOS trades for a daily-cadence strategy whose detector
needs ~40 bars of warmup before its first possible event. Until then every
backtest verdict is recorded as **"no data"** — zero trades by construction,
never faked with synthetic fills or widened windows.

## Risks / known limitations

- OHLCV-approximate volume profile (`approx: true` family, close-bucketed):
  POC/LVN levels are approximations, never blended with tick-derived volume.
- Range compression definition (mean TR vs pre-window baseline ATR) is one
  deterministic choice among several reasonable ones; grid does not cover it
  in v1 — revisit only with data.
- Single-position state machine: concurrent symbols each get their own
  context but the strategy holds at most one position (same as liq-fade-v1).
- Regime conditioning deliberately absent in v1 (spec 036 §7 defers the
  manual regime tag); the strategy must survive without hand-maintained
  inputs.

## Results journal

| Date | Corpus | Verdict | Notes |
|---|---|---|---|
| 2026-08-21 | none | no data | Implementation complete; awaiting daily-bar corpus growth |
