# 045 — Accumulation Detector Screener Rule

## Purpose

Detect tokens where "smart money" (cohort-graded wallets per spec 042) is
actively accumulating — the on-chain equivalent of iCrypto.ai's "Hot Gems of
Smart DEX Traders" feature. The detector combines three independent signals
that, when co-occurring, identify high-conviction accumulation:

1. **Rising OI with stable/rising price** — new positions opening, not
   shorting into weakness
2. **Smart money net inflow** — cohort-graded addresses buying more than
   selling
3. **Exchange outflow** — supply leaving exchanges (spec 043 velocity signal)

This maps directly to the Freebuff screener framework (spec 004 FEA-10):
a `RuleSet` that fires `ScreenerHit` events when all three conditions are
simultaneously true, graded through the RES-4 event study and signal
catalog (spec 025) before any strategy consumption.

## Scope

**In:** A new screener rule definition in `features.toml`, a
`ScreenerHit`-gradeable signal, event study (RES-4) for validation, signal
catalog registration (spec 025), analytics terminal display (spec 041).

**Out:** Strategy consumption before grading (PD-4), copy-trading signals,
real-time alerts on accumulation (v2), token-specific accumulation heuristics
beyond the three-condition model.

## Design

### Signal Model

The accumulation detector is a **compound condition** — all three sub-signals
must be true simultaneously:

```
accumulation_detected(asset, t) =
  oi_rising(asset, t)
  AND smart_money_buying(asset, t)
  AND exchange_outflow(asset, t)
```

Each sub-signal is independently graded before combination. The compound
signal is graded as a whole (the event study measures the joint prediction,
not each component separately).

### Sub-Signal Definitions

**1. `oi_rising` — Open Interest Rising with Price Support**

```
oi_rising(asset, t) =
  oi_delta.4h(asset, t) > oi_delta_threshold        (OI increasing)
  AND regime.trend(asset, t) ∈ {Trend}               (price trending up or flat)
  AND oi.quadrant.4h(asset, t) ∈ {1, 2}             (new longs or short covering)
```

- `oi_delta.4h` is the per-bar OI change (spec 004 catalog)
- `regime.trend` is the HTF trend regime (spec 035 SWG-2)
- `oi.quadrant` is the OI quadrant classification (spec 004):
  quadrant 1 = ↑price↑OI (new longs), quadrant 2 = ↑price↓OI (short cover)

Threshold: `oi_delta_threshold` is configurable per asset (default: top 20th
percentile of trailing 7d OI deltas — dynamic, not fixed). This ensures the
signal fires only when OI increase is unusual, not just positive.

**2. `smart_money_buying` — Smart Money Net Inflow**

```
smart_money_buying(asset, t) =
  cohort.smart_flow.{asset}.24h(t) > smart_flow_threshold
  AND cohort.net_delta.{asset}.smart_money(t) > 0
  AND whale_ratio.{asset}(t) > whale_ratio_floor
```

- `cohort.smart_flow` is the smart money net flow from spec 042
- `cohort.net_delta.{asset}.smart_money` is the net directional exposure
- `cohort.whale_ratio` is the fraction of OI held by whales

Thresholds: `smart_flow_threshold` (default: top 30th percentile of trailing
7d smart flow values), `whale_ratio_floor` (default 0.1 — at least 10% of
OI must be whale-held for the signal to be meaningful).

**3. `exchange_outflow` — Exchange Netflow Velocity Negative**

```
exchange_outflow(asset, t) =
  netflow.velocity.{asset}.24h(t) < outflow_threshold
  AND netflow.regime.{asset}(t) == Outflow  (encoding 2)
```

- `netflow.velocity` is the CEX flow velocity from spec 043
- `netflow.regime` is the flow regime classification

Threshold: `outflow_threshold` (default: −1σ of trailing 7d velocity
distribution — meaningfully negative, not just below mean). The regime
classification provides a second confirmation.

### Screener Rule Configuration

```toml
# features.toml [screener.rules.accumulation]
[accumulation]
name = "accumulation_detector"
enabled = true
horizon_ns = 86400000000000    # 24h forward return for grading
min_n = 30                     # minimum hits before signal catalog promotion
cooldown_ns = 14400000000000   # 4h cooldown between hits per asset

[accumulation.thresholds]
oi_delta_percentile = 0.80     # top 20th percentile of OI delta
smart_flow_percentile = 0.70   # top 30th percentile of smart flow
whale_ratio_floor = 0.10       # minimum whale OI share
outflow_sigma = 1.0            # negative velocity must exceed 1σ
min_trend_bars = 3             # trend must hold for ≥3 bars
```

### Event Study Design

The RES-4 event study measures:

```
Event:    accumulation_detected(asset, t) == true
Window:   pre = 0, post = +24h
Metric:   forward return at +4h, +12h, +24h vs asset baseline
Hypothesis: tokens flagged by accumulation detector show positive
            forward returns with win_rate > 0.55 and avg_excess > 0
Slicing:  by regime (trend/chop), by asset (BTC vs alts)
Minimum:  n ≥ 30 hits across the corpus
```

### Signal Catalog Entry

```json
{
  "id": "SIG-ACC-1",
  "hypothesis": "co-occurring rising OI + smart money buying + exchange outflow predicts positive 24h returns",
  "params_hash": "<hash of [accumulation] config section>",
  "stage": "Hypothesis",
  "grades": [],
  "weekly_avg_excess": [],
  "last_grade_ts_ns": 0,
  "kill_justification": null
}
```

### Analytics Terminal Integration

The accumulation detector feeds the spec 041 terminal:

- **Dashboard panel:** "Accumulation Alerts" — list of assets where
  the detector fired in the last 7 days, sorted by recency
- **Per-asset view:** accumulation score (0–3, count of sub-signals
  active) as a metric card
- **Flow panel:** overlay of smart money flow + exchange velocity on the
  same chart

## Requirements

- **ACC-1** The accumulation detector MUST be defined as a screener rule
  in `features.toml` with the three sub-signals and their thresholds.
  The rule MUST produce `ScreenerHit` events per spec 004 FEA-10.

- **ACC-2** Each sub-signal MUST be independently testable: the screener
  rule evaluation MUST log which sub-signals fired (for debugging and
  event study slicing). A hit where only 2/3 sub-signals fire MUST NOT
  generate a `ScreenerHit`.

- **ACC-3** Thresholds MUST be dynamic (percentile-based) not fixed
  values, so the detector adapts to each asset's normal OI/flow regime.
  Percentile windows are configurable (default 7d trailing).

- **ACC-4** The cooldown period MUST prevent duplicate hits within
  `cooldown_ns` (default 4h) for the same asset. A re-fire after cooldown
  is a new independent signal.

- **ACC-5** The event study (RES-4) MUST be run on the accumulated
  `ScreenerHit` records, computing forward returns at +4h, +12h, +24h.
  MUST NOT promote to `Tested` in the signal catalog (spec 025) until
  n ≥ 30 hits exist in the corpus.

- **ACC-6** The compound signal MUST require ALL three sub-signals
  simultaneously (AND logic). No sub-signal alone is sufficient. This is
  the core hypothesis: the co-occurrence is the edge, not any component.

- **ACC-7** The detector MUST work offline (replay over recorded data)
  and online (live feature engine) with identical results (FEA-4 one-code-
  path). Same ScreenerHit sequence from the same event log.

- **ACC-8** Configuration MUST be TOML `serde(deny_unknown_fields)`
  (CONV-16). All thresholds, windows, and the cooldown are explicit with
  documented defaults.

- **ACC-9** Tests MUST verify: (a) each sub-signal fires independently
  on fixture data, (b) the compound AND logic requires all three,
  (c) cooldown suppresses duplicates, (d) dynamic thresholds adapt to
  the data distribution, (e) ScreenerHit output matches hand-computed
  results on fixture data. No network (CONV-23); requirement-ID test
  names (CONV-21).

- **ACC-10** The signal MUST be registered in the signal catalog (spec 025)
  at `Hypothesis` stage at implementation time. The hypothesis statement
  and falsification criteria are documented in the catalog entry.

## Acceptance criteria

- [ ] `acc_1_screener_rule_produces_hits_on_fixture_data` — three-condition
  match on recorded events produces a ScreenerHit with correct snapshot.
- [ ] `acc_2_partial_match_does_not_fire` — 2/3 sub-signals true → no hit.
- [ ] `acc_3_dynamic_thresholds_adapt_to_distribution` — threshold at
  80th percentile changes when the data distribution shifts.
- [ ] `acc_4_cooldown_suppresses_duplicate_hits` — second hit within 4h
  of the same asset → suppressed.
- [ ] `acc_5_forward_return_computation` — hand-computed forward returns
  at +4h, +12h, +24h match the event study output.
- [ ] `acc_6_and_logic_requires_all_three` — each individual sub-signal
  alone produces zero hits.
- [ ] `acc_7_offline_online_identity` — same event log replayed offline
  and online → identical ScreenerHit sequence (golden).
- [ ] `acc_8_check_config_rejects_unknown_fields` — TOML parse rejects
  typos in the accumulation config section.
- [ ] `acc_9_signal_catalog_entry_valid` — SIG-ACC-1 has hypothesis,
  params_hash, and starts at Hypothesis stage.
- [ ] `acc_10_cooldown_crosses_bar_boundary` — hit at bar close, next
  eligible hit after cooldown expires at a different bar boundary.

## Decisions

- 2026-08-23: New spec (iCrypto.ai "Hot Gems of Smart DEX Traders" mapped
  to Freebuff's screener framework + specs 042/043 feature data). The core
  insight: accumulation is detectable from the co-occurrence of rising OI
  + smart money buying + exchange outflow. No single signal is sufficient.

- 2026-08-23: AND logic (not OR) for the compound signal — the hypothesis
  is specifically about co-occurrence. An event study on each individual
  sub-signal is a separate research question (and likely weaker, since each
  component alone has lower information density).

- 2026-08-23: Dynamic (percentile) thresholds, not fixed — a $10M OI
  increase is normal for BTC but extraordinary for a small-cap. Percentile
  thresholds automatically adapt to each asset's scale. The 7d trailing
  window is a compromise between responsiveness and statistical stability.

- 2026-08-23: 4h cooldown prevents the detector from firing repeatedly on
  the same accumulation episode. Each hit after cooldown is treated as an
  independent signal — either the accumulation renewed or a new episode
  started.

- 2026-08-23: Forward return horizons (+4h, +12h, +24h) span the range
  from scalping to swing. The signal catalog grades against all three;
  strategies select the horizon matching their holding period (spec 035
  SWG-3).

- 2026-08-23: The accumulation detector depends on specs 042 (cohort
  grading) and 043 (CEX flow velocity). If either is unavailable (cohort
  snapshot stale, netflow data missing), the compound signal fails closed
  — no partial matches, no fallback heuristics.

## Open questions

- Should there be a "divergence" variant that fires when smart money is
  buying but exchange flow is neutral (not outflow)? This would capture
  accumulation before exchange flows confirm. Deferred — needs separate
  event study to validate.

- Minimum asset coverage: the signal is only meaningful for assets with
  both options data (specs 037-040) AND netflow data (spec 034). Currently
  this means Ethereum USDT netflows + any options-underlying. BTC netflows
  would require a separate collector. Should the detector require both
  data sources, or degrade to 2/3 signals when netflow is unavailable?
  Recommendation: degrade with a lower confidence flag, but still fire.
