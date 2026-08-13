# orderflow-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
When resting depth on one side of the book dominates (a one-sided liquidity
imbalance) AND the tape is simultaneously printing aggressive trades in the
same direction, the market is often in the middle of a genuine push rather
than a liquidity-wall fake-out. The Cryexc/OpenMarket "fake vs real move"
heuristic: a move with flow behind it AND no wall in front of it is more
likely to continue in the short horizon. We join the push early and exit as
the imbalance normalizes. The counterparty accepts the loss because they are
chasing momentum (aggressive) or defending a stale quote (passive) — we are
compensated for speed of reaction to a transient, verifiable state of the
book+tape. This is a short-horizon, high-frequency-ish continuation trade,
NOT a long-horizon position.

## Signal decomposition (features, spec 004)
- `book.depth.0.5` — depth gauge `(Σbids − Σasks)/(Σbids + Σasks)` within
  0.5% of mid. Positive = resting bid dominance (demand waiting).
- `book.depth_total.0.5` — total resting notional in the band. Filter: trade
  only when it is thick enough that the gauge is not dust-driven.
- `tape.bps_delta` — per-trade price change in bps (emitted ≥ 0.5 bps).
  Positive = aggressive buying is moving the price.

Entry: gauge sign = tape sign, |gauge| ≥ entry_gauge, |tape| ≥ min_tape_bps,
both observed within a short confirm window, depth_total ≥ min_depth.
Direction = gauge sign (bids dominate + aggressive buys → long).
Exit: gauge neutralizes (|gauge| < exit_gauge), gauge flips against the
position, or a hard time stop (default 4h).

## Regime dependency: declared_regime + why
Choppy/range-bound regimes produce constant small imbalances that fade; the
trade needs the imbalance to persist a little, which skews toward
high-volume, trending-lite sessions. Declared regime: any (the gauge+tape
alignment is itself the regime filter) — but we expect the edge, if real, to
be strongest in mid/high vol sessions and to die in dead-calm tape.

## Falsification (written BEFORE any backtest)
Kill if, over the recorded history with full costs (entry+exit taker fees,
funding accrual from Funding events):
- expectancy ≤ 0 in the 2×-cost column at G1, OR
- the edge concentrates in < 3 distinct calendar windows (not a harvest, a
  fluke), OR
- walk-forward OOS flips sign vs in-sample in ≥ 2 of 3 windows (curve fit,
  not an edge).
The strategy must also prove determinism: two identical-seed runs over the
same log must produce byte-identical decision logs (spec 018 discipline).

## Expected characteristics
Horizon: minutes–hours (entry on a confirmed push, exit on normalization).
Trade rate: episodic (a handful per day per symbol in active sessions, near
zero in dead tape). Hit-rate shape: moderate hit rate, small wins, occasional
larger loss when a push reverses hard — the gauge-flip stop is the
risk-keeper. Costs dominate at this horizon, so the 2×-cost column at G1 is
the first gate, not the last.

## Honest scope for v1
Single venue (hyperliquid), single band (0.5%), market intents only (IOC),
no position scaling, no venue arbitrage. The book/tape features are new
(spec 004, 2026-08-13); this hypothesis is the first consumer of
`book.depth.*` / `tape.*` in the funnel.
