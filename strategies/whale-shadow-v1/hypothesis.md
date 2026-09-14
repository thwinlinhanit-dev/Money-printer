# whale-shadow-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
Hyperliquid's public leaderboard census (spec 028) exposes the signed net
positioning of the venue's most successful accounts, polled continuously.
When this cohort builds position in one direction *faster than price moves*,
the flow is informed or at least persistent — whales accumulate on limit
orders inside the book, deliberately, over hours. The payer is the reactive
trader who chases *after* price has already moved, paying spread and funding
late; we position with the census flow at its granularity and hand the
position to that chaser on exit. We collect the basis drift the flow itself
creates: whale buying on HL pushes HL's mark rich relative to the same
underlying on bybit (whose local crowd is not the same crowd — the funding-arb
corpus proved the venues price independently).

Trade shape: cross-venue basis trade, NOT directional. Long the venue the
whale cohort is accumulating, short the same underlying on bybit, net-flat.
Carries venue-relative basis risk, not price risk — same portfolio role as
funding-arb-v1 (which is killed; see Prior-family evidence).

## Prior-family evidence (honest, written first)
funding-arb-v1 was KILLED 2026-09-13: the cross-venue spread normalized
within hours and collectible carry (0.1–4.3 bps) never cleared the two-leg
RT cost (29 bps). This hypothesis differs on the mechanism: it does not
require a funding extreme, only *directional venue-local flow* (whale
delta), and its holding window is flow-driven (exit on flow normalization),
not threshold-driven. The shared, already-proven fact it rests on: HL and
bybit marks/funding move independently on the same underlying (19 overlap
days recorded, spreads ±1000 bps/yr). If whales are NOT informed — if their
delta is noise or lagged chasing — the event study below kills this cheaply.

## Signal decomposition (features, all recorded today)
- `whale.delta.{venue}` — per-symbol Σ signed position-notional change across
  census addresses (spec 028 impl, live since 2026-09-01, fresh through
  09-10; 10-min staleness eviction). Aggregate to 1h bars.
- `whale.net.{venue}` — census level (diagnostic, not the signal).
- Entry z-score: `z = whale.delta_1h / rolling_std(whale.delta_1h, 14d)`,
  computed on the PRIOR closed bar (no lookahead, FARB-3 convention).

Entry:
- `|z| ≥ 2.0` at bar close. Direction: sign of delta. Long HL + short bybit
  same notional (mirror for negative). Both legs entered as one intent pair
  (hedge-first, funding-arb discipline).

Exit:
- `|z| < 0.5` (flow normalized — the accumulation window closed), OR
- hard time stop 48h, OR
- delta makes a NEW extreme opposite the entry (the cohort reversed — cut).

## Regime dependency: declared_regime + why
`regime.vol ∈ {Mid, High}` × any trend. Positioning changes carry
information when the crowd is engaged; in dead-calm regimes whale deltas are
small and the z-gate starves (empirically honest — recorded deltas cluster
with vol).

## Data gates (written BEFORE any backtest; prefix WSH-n)
- **WSH-1** ≥ 10 distinct calendar days with ≥ 1 qualifying `|z| ≥ 2.0`
  event, across the recorded whale-census window (census live 2026-09-01).
- **WSH-2** Both legs recorded same-day: HL positions log + bybit mark
  (continuous since 08-25). Overlap currently ≥ 10 days — verify per event.
- **WSH-3** Census-composition control: the leaderboard refresh (hourly)
  changes cohort membership; qualifying events must span ≥ 2 distinct
  leaderboard epochs — if all events come from one cohort regime, the
  "edge" may be one wallet's trade, not a cohort property.
- Gate order: event study FIRST (CAR[+4h] and CAR[+12h] of the HL−bybit
  mark-relative excess at qualifying events, seeded bootstrap CI95, partial
  windows omitted per SIM-6). Only a CI that does not exclude 0 in the
  WRONG direction proceeds to backtest.

## Falsification (written BEFORE any backtest)
Kill if, with full costs (two-leg RT: taker entry+exit both venues — the
registered funding-arb bar 29.0 bps base / 42.5 stressed; funding accrued
from both venues' Funding events while held):
- event-study CAR CI95 excludes 0 OPPOSITE the hypothesis at either horizon
  (the flow is contrarian or noise — the mechanism is dead, not mispriced), OR
- expectancy ≤ 0 in the 2×-cost column, OR
- profitable episodes concentrate in < 3 calendar windows (not a harvest), OR
- WF OOS sign flips vs in-sample in ≥ 2 of 3 purged/embargoed windows
  (SWG-5 convention).
Determinism per spec 018 (byte-identical decision logs, pinned seed).

## Risks: what breaks it
Leaderboard rotation (the census is not a fixed cohort; composition drift
bakes a moving average into the signal — WSH-3 controls, not cures);
notional distortion (census notionals use ENTRY price, not mark — documented
spec 028); 60s poll granularity misses fast flow; whales hedged on other
venues (HL notional ≠ net exposure — fatal to the mechanism, partially
controlled by the bybit-relative grading); API shape changes (the 08-05
leaderboard migration precedent — spec 028 Decisions).

## Edge results
(none yet — pre-registered 2026-09-13, awaiting WSH-1..3 grading)
