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

## Edge results
### 2026-09-01 — hyperliquid BTC, 4-day merged log (Aug 22–25, 2.8M events)
**STATUS: HYPOTHESIS FALSIFIED. KILLED.**

Backtest (gauge sweep on 4-day merged corpus):
| Gauge | Trades | Exp | Stress 2× | Max DD |
|---|---|---|---|---|
| 0.2 | 27,067 | -3.67 | -6.66 | 252,467 |
| 0.3 (default) | 27,134 | -3.64 | -6.56 | 251,434 |
| 0.4 | 25,336 | -3.90 | -6.91 | 250,930 |
| 0.5 | 22,821 | -4.28 | -7.38 | 248,016 |

Every entry threshold produces negative expectancy. Stress 2× nearly
doubles losses. ~6,000 trades/day confirms over-trading on transient depth
fluctuations.

Walk-forward (12h train / 12h test, 27-combo grid, 5 windows):
| Window | OOS Exp | Deflated Sharpe | Best Params |
|---|---|---|---|
| 1 | -4.59 | -2,675 | gauge=0.4, tape=0.5 |
| 2 | -10.39 | -2,659 | gauge=0.4, tape=2.0 |
| 3 | -5.28 | -2,638 | gauge=0.4, tape=2.0 |
| 4 | -3.53 | -2,657 | gauge=0.3, tape=0.5 |
| 5 | -4.50 | -2,670 | gauge=0.4, tape=1.0 |

All 5 OOS windows negative. Deflated Sharpes ~-2,660 (catastrophic).
No parameter region produces a positive edge.

Monte Carlo (1,000 resamples, 1-day blocks): p50=p95=worst=98,771 max DD.

Falsification checklist:
- ✅ KILL: expectancy ≤ 0 in 2×-cost column at G1 (stress2x -6.56 to -9.36)
- Determinism: ✅ hash stable per-param (same inputs → same hash)

Diagnosis: Hyperliquid BTC book is extremely deep; gauge readings are
noise-driven even within the 0.5% band. 25k+ trades in4 days means the
strategy enters on every transient depth fluctuation aligned with a trade
print. Costs dominate at this trade frequency.

Full output: `runs/orderflow-v1-test/`.
