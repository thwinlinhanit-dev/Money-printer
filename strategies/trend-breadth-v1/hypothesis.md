# trend-breadth-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
Time-series momentum: assets that have moved tend to continue over
weekly horizons, across every asset class studied for decades. The
counterparties are (a) mean-reversion traders fading moves early, (b)
under-reactors anchored to old prices, (c) forced flows (liquidations,
hedging) that push with the trend late. They "accept" the loss via behavioral
bias and constraint, not choice. The solo edge is *breadth + discipline*:
watching 100+ perps continuously and taking every qualifying signal without
opinion — which is software, not talent.

## Regime dependency: declared_regime + why
`regime.trend = Trend` × `regime.vol ∈ {Mid, High}`. By construction bleeds
in chop (whipsaw cost is the strategy's insurance premium). Portfolio role:
the right-tail engine; pays in expansions when carry is squeezed.

## Falsification (written BEFORE any backtest)
Kill if, with full costs on the recorded universe:
- WF OOS expectancy ≤ 0 across ≥ 70% of windows (G2 shape), OR
- performance requires a knife-edge lookback (plateau check fails ±30%), OR
- all P&L comes from BTC/ETH beta (breadth adds nothing vs a 2-asset version
  — then it's not this hypothesis).

## Expected characteristics
Horizon: days–weeks. Trade rate: steady, universe-wide (dozens of
positions, small each). Hit-rate shape: LOW hit rate (~30–40%), avg win ≫
avg loss — positive skew; psychologically miserable for humans, trivial for
the machine. Deep, long drawdowns in chop are expected and budgeted, not
anomalous. Capacity: high relative to our size.

## Risks: what breaks it
Prolonged low-vol chop (known cost); regime flip whipsaws at position scale;
universe survivorship (delistings — SCD2 symbol metadata must be honored in
backtests, STO-9); crowded momentum unwinds; funding costs eating slow trends
(funding accrual in sim is mandatory, SIM-4).
