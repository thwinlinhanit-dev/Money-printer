# liq-fade-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
Forced liquidations print at whatever price the exchange can fill — often far
from fair value. A sell cascade (longs being dumped) pushes the book down and
marks liquidation prints well below mid; a buy cascade (shorts squeezed) does
the mirror above it. The forced seller/buyer pays ANY price to exit, so they
accept the loss. Once the cascade is done, the mechanical pressure that moved
price is gone, and the market tends to revert toward the level the book says
is fair — the stretch created by forced flow, not information, mean-reverts.
We fade the cascade AFTER exhaustion: we do NOT catch the knife; we wait until
the flow has demonstrably stopped accelerating and then take the side of
reversion. Counterparty: late momentum chasers who extrapolate the cascade's
last prints. Short-horizon mean-reversion trade, NOT a continuation trade and
NOT a long-horizon position.

## Signal decomposition (features, spec 004 §Liquidation flow)
- `liq.vol_sell` — rolling Σ sell-side liquidation notional (5-min window).
  The cascade measure: big value = longs being dumped.
- `liq.vol_buy` — mirror: shorts being squeezed.
- `liq.dist` — liquidation price-distance from mid, bps, at each liq print.
  The stretch measure: how far from fair value the cascade is hitting.
- `liq.rate` — rolling liquidation event rate (events/sec). Context: the
  cascade is many events, not one print.

Entry (sell-cascade fade → BUY, mirrored for the buy cascade):
- `liq.vol_sell` ≥ `entry_vol` (a real dump, not noise), AND
- `liq.dist` ≥ `entry_dist_bps` (the prints are STRETCHED from fair — the
  knife has already fallen far), AND
- **exhaustion**: current `liq.vol_sell` ≤ `exhaust_frac` × the recent peak
  of `liq.vol_sell` (the rolling sum is DRAINING because the cascade stopped
  printing — flow is over, not paused mid-swing). This is the whole edge: we
  fade only what has already stopped moving price.

Exit:
- `liq.dist` < `exit_dist_bps` — the stretch is gone (reversion complete), OR
- `liq.vol_sell` makes a NEW peak above the entry peak — the cascade
  re-accelerated; the fade was wrong, cut it, OR
- hard time stop (default 4h).

## Regime dependency: declared_regime + why
The edge needs a liquidation cascade to exist at all, so it is inherently
episodic — it lives in volatile, cascade-prone sessions and is dead in calm
tape (and dead whenever the venue has no liquidation stream: hyperliquid
today). Declared regime: any, with the expectation that the edge concentrates
in high-vol windows (that IS the signal — cascades happen there). The
falsification window-count rule below is the honest check that we are fading
real cascades, not one lucky week.

## Falsification (written BEFORE any backtest)
Kill if, over the recorded history with full costs (entry+exit taker fees):
- expectancy ≤ 0 in the 2×-cost column at G1, OR
- the edge concentrates in < 3 distinct calendar windows (not a harvest, a
  fluke), OR
- walk-forward OOS flips sign vs in-sample in ≥ 2 of 3 windows (curve fit,
  not an edge).
The strategy must also prove determinism: two identical-seed runs over the
same log must produce byte-identical decision logs (spec 018 discipline).

## Honest data gate (written BEFORE implementation)
The `liq.*` features require a recording with a liquidation stream — Bybit's
public `allLiquidation.` topic (COL-29, spec 024). The current corpus is
hyperliquid-only, which has NO native liq stream, so `liq.*` never emits on
it. Therefore: this strategy CANNOT be backtested on the corpus as it stands.
The v1 deliverable is the hypothesis + implementation + unit tests (state
machine proven on synthetic feature updates) + sim registration; the honest
first verdict is "no data — not falsifiable yet," recorded as such, and the
backtest gate opens when a bybit recording day exists. No synthetic
liquidation data will be invented to fake a verdict (PD-6/INT-1: the audit
judges provenance, and so does the funnel).

## Expected characteristics
Horizon: minutes–hours (entry after exhaustion, exit on reversion or
re-acceleration). Trade rate: episodic — a handful per day in cascade-heavy
sessions, near zero otherwise. Hit-rate shape: moderate; the winner is the
stretch collapse, the loser is the re-accelerating cascade (which the new-peak
cut keeps bounded). Costs dominate at this horizon — the 2×-cost column at G1
is the first gate.

## Honest scope for v1
Single venue (bybit, once recorded), market intents only (IOC), no position
scaling, no venue arbitrage, no cross-cascade (buy cascade + sell cascade on
the same window do not compound). The `liq.*` features are new (spec 004,
2026-08-13); this hypothesis is their first consumer in the funnel.
