# footprint-exhaustion-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
Directional moves end not with a decision but with an auction: the final
bar of a trend leg prints its largest volume on the SMALLEST range, because
the last holders who needed to transact have transacted and the crowd
chasing the move is fully positioned. This is the volume-climax exhaustion
signature — a century of tape-reading folklore (Wyckoff "effort vs result",
umpiring at climaxes) that is cheap to grade honestly now that the corpus
records trade tapes with real aggressor tags. The payer is the late chaser
who buys the climax bar at market; we take the other side of exactly that
fill, for one short horizon, with a hard invalidation.

Trade shape: directional fade of the climax, short horizon (1–4h), hard
stop. NOT a swing entry — this is a scalp-grade reversion harvesting the
chaser's immediacy premium, which is why the cost bar below is strict.

## Prior-family evidence (honest, written first)
- orderflow-v1 (the closest recorded-corpus relative, footprint/delta-based)
  was KILLED 2026-09-11 after an owner-approved retest: in-sample +12.04 →
  OOS −12.46, net-negative on every binance day. Its lesson is baked in
  twice below: (a) the pre-registered grid is small and fully reported —
  no post-hoc threshold selection; (b) WF OOS sign-flip is a kill, not a
  tune signal.
- The 08-13 funding-carry study showed the HL tape's tape-derived bars are
  dense enough (24h/24h hourly coverage on clean days) to compute honest
  per-bar volume percentiles — the machinery inputs exist.
- liq-fade-v1's exhaustion-conditioning design (fade only what stopped
  accelerating) is borrowed: the climax must show deceleration INSIDE the
  bar sequence, not just size.

## Signal decomposition (features, spec 049 catalog)
- `footprint.volume.bubble.{tf}` — per-bar volume percentile vs rolling
  window (climax > 80, dry-up < 20). The effort measure.
- `footprint.delta.{tf}.{bucket}` — net aggressor delta inside the bar.
  The conviction measure: a climax with POSITIVE delta into a down-close
  (absorption) or NEGATIVE delta into an up-close (exhaustion of buyers
  while price rises) is the conflicting-flow signature.
- bar range vs ATR (bar-based, from `mp-query bars`): the "result" measure.

Entry (down-fade, mirrored up):
- prior bar volume percentile ≥ 80 (climax), AND
- bar closes DOWN after ≥ 2 prior up-closes (the leg existed), AND
- bar range ≤ 40% of ATR(20 bars) — effort high, result tiny, AND
- `footprint.delta` of the climax bar ≥ 0 (buyers pressed the close and it
  did not go — absorption) — the deceleration condition.

Exit:
- price exceeds the climax bar's extreme by 0.1×ATR (invalidation — the
  auction continued), OR
- reversion to the climax bar's midpoint (target — the scalp is done), OR
- hard time stop 4 bars.

## Regime dependency: declared_regime + why
`regime.vol ∈ {Mid, High}`. Climaxes exist where volume exists; in dead tape
the percentile gate starves honestly. Scalp horizon ⇒ most exposed to the
cost leg; that is a falsification criterion, not a footnote.

## Data gates (written BEFORE any backtest; prefix FPX-n)
- **FPX-1** ≥ 20 qualifying climax events per symbol per venue across the
  recorded tapes (bybit BTC/ETH 08-25→09-12 continuous; HL same window),
  spanning ≥ 3 calendar weeks. Fewer ⇒ NOT GRADABLE, stays held.
- **FPX-2** Pre-registered parameter grid, fully reported, none selected
  post hoc: volume percentile ∈ {80, 90} × ATR-range ratio ∈ {0.4, 0.6}
  (delta sign condition fixed, mirrored for up-fades).
- **FPX-3** Tape-integrity precondition: only days passing the INT-4 audit
  (no stale-burst/known-dirty days) enter the study; a climax defined on a
  burst-duplicated tape is an artifact, not an event.
- Gate order: event study FIRST — CAR[+1h], CAR[+2h], CAR[+4h] of the raw
  mark return (and the BTC−ETH excess as the beta control), seeded bootstrap
  CI95, partial windows omitted (SIM-6). Excess-return CI is the verdict
  leg: a fade that only works as BTC beta is not this hypothesis.

## Falsification (written BEFORE any backtest)
Kill if, with full costs (taker entry+exit + perp spread — the scalp cost
bar makes this the strictest of the three; the registered cost model):
- expectancy ≤ 0 in the 2×-cost column in ≥ 3 of 4 pre-registered configs, OR
- gross-positive but net-negative at BASE costs in every config (the
  immediacy premium is smaller than the round trip — the hypothesis is dead
  at any realistic size), OR
- CAR CI95 excludes 0 opposite the hypothesis on the BTC−ETH excess leg at
  ≥ 2 horizons in ≥ 2 configs (the "reversion" is beta snap-back), OR
- profitable episodes concentrate in < 3 calendar weeks, OR
- WF OOS sign flips vs in-sample in ≥ 2 of 3 purged/embargoed windows.
Determinism per spec 018.

## Risks: what breaks it
Bar-based approximation (spec 054 REL-20: volume/delta per bar cannot see
intra-bar order — absorption read is approximate); tape-only volume on HL
(trades channel, no quote rule — percentile semantics differ per venue;
per-venue calibration reported, never pooled silently); climax-as-continuation
regimes (breakouts that accelerate THROUGH the climax — the invalidation
stop carries this risk, the falsifier above measures its cost); short
horizon × cost asymmetry (the most likely honest kill — declared up front).

## Edge results
(none yet — pre-registered 2026-09-13, awaiting FPX-1..3 grading)
