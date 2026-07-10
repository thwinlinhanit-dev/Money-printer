# carry-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
Perp funding rates go to extremes when leveraged directional traders crowd
one side; they pay funding as the price of leveraged exposure they want NOW.
We take the other side hedged (short perp + long spot, or the reverse),
collecting funding while carrying basis risk instead of price risk. The payer
accepts the loss knowingly — funding is their cost of conviction/leverage;
we are compensated for balance-sheet use, unglamorous inventory risk, and
willingness to hold through squeeze discomfort. Capacity is small at the
extremes, which is why size skips it and a fund-of-one can eat.

## Regime dependency: declared_regime + why
`regime.vol ∈ {Mid, High}` × any trend state. Funding extremes cluster with
volatility and crowded positioning; in dead-calm regimes extremes are rare
and thin. Not directional — profits in chop are the point (portfolio role:
pays when trend-breadth bleeds).

## Falsification (written BEFORE any backtest)
Kill if, over the recorded history with full costs (entry+exit fees both
legs, funding actually accrued from Funding events, borrow/spot spread):
- expectancy ≤ 0 in the 2×-cost column at G1, OR
- p95 adverse excursion during holds implies a DD budget the sizing engine
  can't fund at alloc_floor, OR
- edge concentrates entirely in < 3 calendar events (not a harvest, a fluke).

## Expected characteristics
Horizon: hours–days (entry at |funding z-score| ≥ threshold, exit on
normalization or time stop). Trade rate: low, episodic (a few/week across
universe). Hit-rate shape: high hit rate, small wins, occasional larger loss
on squeezes — negative skew managed by sizing, hedge discipline, and hard
time stops. Capacity: limited; fine.

## Risks: what breaks it
Funding regime change by venue (formula/interval changes); squeeze while
hedge-legged (execution gap between legs — enter hedge-first); spot/perp
basis blowout; venue solvency during exactly the events that pay us;
crowding by other carry harvesters compressing extremes.
