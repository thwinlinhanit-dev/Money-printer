# liq-fade-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
Liquidation cascades are *forced, price-insensitive selling/buying*: the
liquidation engine must trade NOW at any price. Forced flow overshoots fair
value; price mean-reverts once the forced flow exhausts. We provide liquidity
into the overshoot. The payer is the liquidated trader (via the engine) —
they don't accept the loss, they're compelled. Compensation is for catching
a falling knife with structure: entry only after cascade-exhaustion evidence,
tight invalidation, hard time stop. The solo edge is availability (3am) and
pre-committed structure (no fear at the lows, no greed either).

## Regime dependency: declared_regime + why
`regime.vol = High` (cascades are a high-vol phenomenon) × any trend, but
counter-trend fades in strong trends get half risk (params). Portfolio role:
panic-day payer, complements trend (which is often stopped/flat into
cascades) and carry (which is stressed then).

## Falsification (written BEFORE any backtest)
Kill if, with L1 fills (latency ≥ 150ms, participation cap — no fantasy
fills at the wick), full costs:
- expectancy ≤ 0 in the 2×-cost column, OR
- the edge exists only with fills better than top-of-book at event time
  (i.e. it's a fill-model artifact, SIM-2's trade-print rule decides), OR
- post-cascade drift is not distinguishable from zero in the event study
  (RES-4 harness) on ≥ 100 cascade events.

## Expected characteristics
Horizon: minutes–hours. Trade rate: episodic bursts (clusters on vol days,
nothing for weeks). Hit-rate shape: moderate hit rate, quick resolution
either way; time-stop losses common; occasional continuation loss = the cost
of the trade (must be capped by structure, never averaged into). Capacity:
small-mid; sensitive to size (we ARE the liquidity — impact modeling matters,
L2 fills before scaling).

## Risks: what breaks it
Cascade continuation (second wave — never add to losers, hard rule);
liquidation-stream sampling bias (Binance throttling ⇒ prefer venues with
complete feeds, COL-8 flag consulted); venue halts/auto-deleverage during
exactly our events; latency spikes at the worst moments (chaos drills cover);
detector overfitting to one historical crash (require event-count breadth).
