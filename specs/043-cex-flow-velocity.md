# 043 — CEX Flow Velocity Features

## Purpose

Derive actionable flow signals from the spec 034 exchange-reserve balance
snapshots: velocity (rate of balance change), acceleration (change in
velocity), and flow regime classification. These transform raw balance
snapshots into the same "Hot Tokens from CEX Flows" signals that iCrypto.ai
provided — but computed from Freebuff's own recorded data with deterministic,
reproducible features (PD-3/CONV-9).

Exchange netflows are a documented 1–2h-ahead return signal (SSRN 4630115,
arXiv 2411.06327). The raw snapshots (spec 034 NFL-5) store honest
observations; this spec derives the trading signals from them.

## Scope

**In:** Feature engine additions (spec 004 catalog) computing velocity,
acceleration, flow regime, and cumulative flow from `NetflowSnapshot` events.
Online + offline (same code path, FEA-4).

**Out:** The collector itself (spec 034), real-time streaming of netflows
(current 300s cadence is sufficient for swing), strategy consumption before
RES-4 grading (PD-4), multi-chain netflows (Ethereum only in v1), exchange
attribution beyond the watchlist.

## Design

### Data Flow

```
NetflowSnapshot events (spec 034)
  │  { address, balance (raw USDT units), recv_ts_ns }
  ▼
Balance History Accumulator
  │  Per (address, asset): BTreeMap<ts_ns, balance> (rolling window)
  │  Window: max(velocity_window × 2, 24h) of trailing snapshots
  ▼
Derived Features
  ├─ netflow.velocity.{asset}.{w}     — Δbalance / Δtime over window w
  ├─ netflow.acceleration.{asset}.{w} — Δvelocity / Δtime (velocity slope)
  ├─ netflow.cumulative.{asset}.{d}   — Σ velocity over trailing d days
  ├─ netflow.regime.{asset}           — {Inflow, Neutral, Outflow} by percentile
  └─ netflow.zscore.{asset}.{w}      — z-score of current velocity vs trailing dist
```

### Feature Definitions

**`netflow.velocity.{asset}.{w}`** — Rate of balance change over window w.

```
velocity = (balance_now − balance_window_ago) / (ts_now − ts_window_ago) × 86400e9
```

Result is in **balance units per day** (e.g., USDT/day). Positive = net
inflow to exchanges (bearish signal — supply heading to market). Negative =
net outflow (bullish — supply leaving exchanges). Window w ∈ {1h, 4h, 24h}
in nanoseconds.

**`netflow.acceleration.{asset}.{w}`** — Change in velocity (second
derivative).

```
acceleration = (velocity_now − velocity_half_window) / (ts_now − ts_half_window) × 86400e9
```

Result is in **balance units per day²**. Positive acceleration = inflows
speeding up (bearish intensification). Negative acceleration = inflows
slowing or outflows accelerating (bullish intensification).

**`netflow.cumulative.{asset}.{d}`** — Cumulative signed flow over trailing d
days.

```
cumulative = Σ (velocity_i × Δt_i) for all snapshots in trailing d days
```

Result is in **balance units** (e.g., total USDT moved). Large negative
cumulative = significant exchange depletion over the period.

**`netflow.regime.{asset}`** — Classification of current velocity against its
trailing distribution.

```
percentile = rank(velocity_now) / n_samples  (IVS-6 midrank method)
regime = { Inflow (≥66%), Neutral (33-66%), Outflow (≤33%) }
```

Encoding: 0=Inflow, 1=Neutral, 2=Outflow (reverse-ordinal, same convention
as `iv.regime` spec 038).

**`netflow.zscore.{asset}.{w}`** — Standardized velocity.

```
zscore = (velocity_now − mean(velocity_window)) / stddev(velocity_window)
```

z > 2 = extreme inflow (bearish outlier). z < −2 = extreme outflow (bullish
outlier). Z-scores are the strongest screener input because they normalize
across assets with different absolute balance scales.

### Address Aggregation

Spec 034 records per-address snapshots. The velocity features aggregate
across the watchlist for each asset:

```
total_balance(asset, t) = Σ balance(address_i, t) for all watchlist addresses
```

Per-address snapshots may arrive at different times (different poll cadences).
The accumulator uses the LATEST snapshot per address at each query time —
last-wins upsert semantics (same as spec 028 whale.net stale eviction).

### Fail-Closed Semantics

- Fewer than 2 snapshots in the window → velocity emits None (CONV-8)
- Fewer than 3 snapshots → acceleration emits None (need velocity at two
  points)
- Non-finite balance values → skip that address's contribution (CONV-8)
- All addresses stale → all features emit None (no fabricated flow)
- Window跨越 a gap where no snapshots exist → velocity is computed from the
  two available endpoints (honest, not interpolated)

## Requirements

- **CFV-1** `netflow.velocity.{asset}.{w}` MUST compute (balance_delta /
  time_delta) × day_ns from the accumulated snapshot history per asset. MUST
  emit None when fewer than 2 snapshots exist in the window (CONV-8).

- **CFV-2** `netflow.acceleration.{asset}.{w}` MUST compute the rate of
  change of velocity using two velocity readings separated by half the window.
  MUST emit None when fewer than 3 snapshots exist (CONV-8).

- **CFV-3** `netflow.cumulative.{asset}.{d}` MUST sum (velocity × Δt) over
  all snapshots in the trailing d-day window. MUST use trapezoidal
  integration (average of consecutive velocity readings × time between them)
  for accuracy with irregular polling intervals.

- **CFV-4** `netflow.regime.{asset}` MUST classify current velocity using the
  midrank percentile method from spec 038 (IVS-6): `(below + 0.5 × equal) /
  n`. Thresholds: ≥66% = Inflow, ≤33% = Outflow, else Neutral. Encoding:
  0=Inflow, 1=Neutral, 2=Outflow. MUST require ≥20 historical velocity
  observations before emitting a regime classification.

- **CFV-5** `netflow.zscore.{asset}.{w}` MUST compute (value − mean) /
  stddev over the trailing window. MUST emit None when stddev < ε (constant
  velocity → z-score undefined, CONV-8).

- **CFV-6** All features MUST be deterministic (CONV-9..12): pure function of
  (NetflowSnapshot events, config, seed). Address iteration via BTreeMap
  (CONV-10). Same inputs ⇒ byte-identical output (golden test).

- **CFV-7** Features MUST register in the 004 catalog with prefix
  `netflow.` (FEA-7/CONV-20). Per-asset features are per-symbol tick
  features (each asset is a distinct SymbolId). Regime and z-score are
  tick features; velocity and acceleration are tick features; cumulative is
  a tick feature.

- **CFV-8** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16).
  Parameters: velocity windows (default 1h, 4h, 24h), cumulative window
  (default 7d), regime lookback (default 90d), z-score window (default 24h),
  min observations for regime (default 20). Entry in `features.toml.example`.

- **CFV-9** Features MUST enter the signal catalog (spec 025) at
  `Hypothesis` stage. The hypothesis: "exchange netflow velocity z-score > 2
  predicts negative forward returns over 4–24h; z-score < −2 predicts
  positive returns." Promotion requires RES-4 event study with n ≥ 30.

- **CFV-10** Tests MUST use recorded fixture data (spec 034 NetflowSnapshot
  events), no network (CONV-23); requirement-ID test names (CONV-21);
  proptest for the velocity/acceleration math (CONV-22).

## Acceptance criteria

- [ ] `cfv_1_velocity_computed_from_snapshot_pairs` — hand-computed:
  balance went from 1000 to 1100 over 3600s → velocity = 2400/day.
- [ ] `cfv_2_acceleration_computed_from_velocity_pairs` — velocity went
  from 100 to 200/day over 12h → acceleration = 200/day².
- [ ] `cfv_3_cumulative_trapezoidal_integration` — irregular polling
  intervals correctly integrated.
- [ ] `cfv_4_regime_classification_with_midrank` — velocity at 80th
  percentile → Inflow; at 20th → Outflow; at 50th → Neutral.
- [ ] `cfv_5_zscore_normalization` — z-score of 2.0 for extreme value,
  0.0 for mean value.
- [ ] `cfv_6_deterministic_golden` — same snapshots + config → byte-
  identical feature values.
- [ ] `cfv_7_catalog_registration` — all five feature families registered
  with correct IDs.
- [ ] `cfv_8_stale_addresses_suppressed` — all addresses stale > 2× poll
  cadence → features emit None.
- [ ] `cfv_9_degenerate_inputs_fail_closed` — non-finite balance, empty
  watchlist, single snapshot → None (no panic, CONV-15).
- [ ] `cfv_10_proptest_velocity_is_balance_over_time` — proptest: velocity
  always equals (delta_balance / delta_time) × day_ns for valid inputs.

## Decisions

- 2026-08-23: New spec (iCrypto.ai "Hot Tokens from CEX Flows" mapped to
  Freebuff's spec 034 data layer). The raw snapshots exist; velocity
  features are the natural derivation (NFL-5: "derived netflows computed in
  research").

- 2026-08-23: Balance units per day (not per second) for velocity — the
  natural human-readable unit for exchange flows. A 1-hour window producing
  "2400 USDT/day" is immediately interpretable; "0.0278 USDT/s" is not.

- 2026-08-23: Trapezoidal integration for cumulative flow (CFV-3) —
  irregular polling intervals (spec 034 default 300s, but addresses may
  miss polls) make simple summation inaccurate. Trapezoidal is the standard
  numerical method and costs one extra multiply per segment.

- 2026-08-23: Regime encoding 0=Inflow/1=Neutral/2=Outflow (reverse-ordinal)
  matches spec 038's `iv.regime` convention (0=Risk/1=Neutral/2=Cheap).
  The direction is: 0 = bearish extreme, 2 = bullish extreme. Consistent
  encoding across feature families avoids classification bugs.

- 2026-08-23: Min 20 observations for regime (CFV-4) — fewer samples make
  the percentile unreliable. This means the regime feature takes ~6 hours
  to activate (20 samples × 300s cadence = 100 min minimum, but with
  address stagger it's ~6h). The velocity features activate immediately
  (2 snapshots needed).

- 2026-08-23: v1 is Ethereum-only (USDT primary, spec 034). Multi-chain
  netflows require new collectors and are a separate spec. The feature
  architecture is chain-agnostic — the `asset` parameter carries the token
  name, and the collector determines the chain.

## Open questions

- Should velocity features track per-address or only aggregate? Per-address
  velocity would enable "which exchange is seeing outflows" — valuable for
  venue-specific signals. Deferred to v2 when address count grows.

- Cross-asset correlation: does BTC USDT outflow predict ETH inflow? This
  would be a GLOBAL feature (FEA-20) like `liq.delta`. Deferred — needs
  multi-asset netflow data first.
