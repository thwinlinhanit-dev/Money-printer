# 038 — IV Surface Builder & Volatility Analytics

## Purpose

Construct the implied volatility surface from recorded Deribit OptionTicker
events — a 3D surface of IV across (strike, expiry, time) — and derive the
volatility analytics that strategies consume: IV term structure, IV skew,
IV vs realized vol (VRP), volatility regime classification, and vol index.

This is the vol analytics layer. It transforms the per-contract `mark_iv`
from OptionTicker into the structural vol features that carry regime
information for strategy filtering (spec 006) and risk sizing (spec 008).

## Scope

**In:** IV surface construction, term structure extraction, skew computation,
IV vs RV (volatility risk premium), volatility regime classifier, DVOL-style
vol index, IV percentile/rank, IV surface history for materialization.

**Out:** Greeks computation (spec 037), options flow analytics (spec 039),
options trading (BACKLOG [v2]), 3D visualization (spec 041 is the consumer,
not the producer).

## Design

### Data Flow

```
OptionTicker events (spec 031)
  │  { leg: { underlying, strike, expiry_ts_ns, kind }, mark_iv, mark_price,
  │    underlying_price, open_interest, greeks }
  ▼
IV Surface Builder
  │  Index: (underlying, strike, expiry) → mark_iv
  │  Interpolate missing strikes via SVI or linear
  ├──▶ iv.surface.{underlying}         — full surface snapshot (materialized)
  ├──▶ iv.term.{underlying}.{tenor}    — ATM IV for tenor ∈ {1w, 1m, 3m, 6m}
  ├──▶ iv.skew.{underlying}.{tenor}    — 25Δ risk-reversal per tenor
  ├──▶ iv.skew.{underlying}.{wing}     — wing richness per tenor (25Δ put / 25Δ call IV ratio)
  ├──▶ iv.atm.{underlying}            — ATM IV (nearest expiry, interpolated)
  ├──▶ iv.vrp.{underlying}            — IV − realized vol (volatility risk premium)
  ├──▶ iv.regime.{underlying}         — {Rich, Fair, Cheap} by IV percentile
  ├──▶ iv.index.{underlying}          — DVOL-style composite vol index
  ├──▶ iv.percentile.{underlying}     — rolling percentile rank of current IV vs history
  └──▶ iv.rv.{underlying}.{tf}.{w}   — realized vol (bridged from spec 004 vol.rv.*)
```

### IV Surface Construction (spec 038-SURFACE)

The surface is a matrix `IV(K, T)` where `K` is moneyness (K/spot) and `T` is
time-to-expiry in years.

**Step 1: Per-expiry IV slice**
From OptionTicker events for each expiry `T`:
- Collect `(strike, mark_iv)` pairs for all active strikes
- Sort by strike (BTreeMap, CONV-10)
- Interpolate between strikes using piecewise linear (v1) or SVI parameterization
  (v2, if surface smoothness is needed)
- Extrapolate wings using the last known slope (flat extrapolation beyond 2 standard
  deviations from ATM)

**Step 2: Interpolate across expiries**
For tenors between available expiries:
- Linear interpolation in log(time) space (standard for vol term structure)
- Expiry dates from OptionTicker `expiry_ts_ns` — no synthetic expiries beyond
  what Deribit lists

**Step 3: Moneyness normalization**
All strikes normalized to moneyness `m = K / spot` where `spot` = the
OptionTicker's `underlying_price` at the same timestamp. This makes the
surface comparable across spot levels.

### ATM IV Extraction (spec 038-ATM)

ATM IV = the mark_iv of the option with strike closest to spot, interpolated
if no exact ATM strike exists. For each available tenor:
- Find the two strikes bracketing spot
- Linear interpolate IV at moneyness = 1.0
- Fall back to the nearest strike if only one side exists

ATM IV is the primary input for term structure and VRP.

### IV Term Structure (spec 038-TERM)

The ATM IV curve across expiries:
```
term(tenors) = [ATM_IV(T₁), ATM_IV(T₂), ..., ATM_IV(Tₙ)]
```

Tenors reported: 1 week, 1 month, 3 months, 6 months (nearest available expiry).
Convention: contango = back months > front months (normal); backwardation = front
> back (fear/regime shift).

The feature engine emits `iv.term.{underlying}.{tenor}` as scalar ATM IV.

### IV Skew (spec 038-SKEW)

**Risk-reversal** per tenor:
```
RR(T) = IV_25Δ_call(T) - IV_25Δ_put(T)
```
Where 25Δ points are interpolated from the IV smile at the strike where
|delta| ≈ 0.25. Negative RR = put skew (fear); positive = call skew (greed).

**Wing richness** per tenor:
```
WR(T) = IV_25Δ_put(T) / IV_25Δ_call(T)
```
> 1.0 = put wing richer (downside protection expensive); < 1.0 = call wing richer.

Both are emitted per tenor, keyed by the nearest available expiry.

### IV vs Realized Vol — VRP (spec 038-VRP)

```
VRP(T) = IV_ATM(T) - RV(T)
```

Where `RV(T)` = realized vol over the same period `T` (from spec 004
`vol.rv.{tf}.{w}` — the feature engine already computes this).

- Positive VRP → options are expensive relative to realized moves
  (sell vol / short straddle is the carry trade)
- Negative VRP → options are cheap relative to realized moves
  (buy vol / long straddle is the insurance trade)
- The VRP is the single most important vol signal for strategy filtering

Emission: `iv.vrp.{underlying}` = IV_ATM_nearest − RV_nearest_window.

### Volatility Regime Classifier (spec 038-REGIME)

```
iv.regime.{underlying} = {
  Rich   if percentile(IV_ATM) ≥ 66,
  Fair   if 33 < percentile < 66,
  Cheap  if percentile(IV_ATM) ≤ 33,
}
```

Where `percentile` = rolling percentile rank of current ATM IV over the
trailing `iv_regime_lookback_days` window (default: 90 days). The classifier
is categorical (encoded as small int per CONV-6 conventions: 0=Rich, 1=Fair,
2=Cheap).

> ⚠️ **Encoding is REVERSE-ordinal**: a HIGHER encoded value means CHEAPER
> vol (lower IV percentile). Any consumer doing threshold logic (`regime > t`)
> must know that the scale runs Rich(0) → Cheap(2). This is documented here,
> in the feature metadata, and in the config example — do not "fix" it by
> flipping values after data has materialized (that is a schema change, CONV-20).

### DVOL-Style Vol Index (spec 038-INDEX)

Composite implied vol index (analogous to Deribit DVOL):
```
DVOL = sqrt(Σ w_i × IV²(K_i, T_nearest)) / sqrt(Σ w_i)
```

Where weights `w_i` are proportional to OI at each strike (more liquid strikes
contribute more). Computed per update cycle, emitted as `iv.index.{underlying}`.

This is the headline "what is implied vol right now" number — used for
cross-asset comparison and historical percentile ranking.

### IV Percentile (spec 038-PERC)

```
percentile(IV_current) = count(IV_t ≤ IV_current for t in window) / window_size
```

Rolling window: `iv_percentile_window_days` (default 365 days). Emitted as
`iv.percentile.{underlying}` ∈ [0.0, 1.0]. Used by the regime classifier
and by strategies as a vol filter.

### IV Surface History

For materialization, the full surface is written as a Parquet snapshot:

```
features/iv.surface/ver=N/underlying=BTC/date=YYYY-MM-DD/part.parquet
```

Schema: `{ moneyness, time_to_expiry_years, iv, ts_ns }`. One row per
(moneyness, tenor) grid point. Materialized at bar-close cadence (daily)
per the feature engine's offline path.

## Requirements

- **IVS-1** The IV surface builder MUST consume `OptionTicker` events and
  produce features registered in the 004 catalog with ids matching the
  design above. Feature ids are normative (rename = schema change, CONV-20).

- **IVS-2** ATM IV extraction MUST interpolate between strikes at
  moneyness = 1.0 using linear interpolation on (strike, IV). The spot
  reference MUST be the OptionTicker's `underlying_price` (same contract
  as spec 037 GRE-10).

- **IVS-3** IV term structure MUST report ATM IV for tenors {1w, 1m, 3m, 6m},
  falling back to the nearest available expiry when an exact tenor is not
  listed. The feature MUST NOT synthesize expiries beyond what Deribit offers.

- **IVS-4** IV skew MUST compute risk-reversal (25Δ call − 25Δ put IV) and
  wing richness (put/call IV ratio at 25Δ). The 25Δ strikes MUST be
  interpolated from the IV smile using the delta-strike mapping from the
  OptionTicker's greeks.

- **IVS-5** VRP MUST be `IV_ATM - RV` using the same underlying and
  comparable time windows. RV comes from the existing spec 004
  `vol.rv.{tf}.{w}` feature. The VRP feature MUST fail-closed (suppress)
  when RV is unavailable (FEA-8 stale-book pattern).

- **IVS-6** Vol regime MUST be a categorical output {Rich, Fair, Cheap} based
  on the rolling percentile of ATM IV over a configurable lookback window.
  The percentile computation MUST use a sort-based exact method (not
  approximate), consistent with BTreeMap ordering (CONV-10).

- **IVS-7** The DVOL-style vol index MUST weight by OI at each strike.
  Strikes with zero OI MUST NOT contribute. The index MUST be positive
  and finite (NaN fail-closed, CONV-8).

- **IVS-8** All computations MUST be deterministic (CONV-9..12): pure
  function of (events, config, seed); sorted iteration (CONV-10); NaN/inf
  fail-closed (CONV-8).

- **IVS-9** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16),
  entries in `features.toml.example`; regime thresholds, lookback windows,
  tenor list explicit with documented defaults.

- **IVS-10** Tests MUST use recorded Deribit fixtures, no network
  (CONV-23); requirement-ID test names (CONV-21); proptest for surface
  monotonicity in the wing regions and VRP math (CONV-22).

- **IVS-11** IV percentile MUST handle gaps in the history gracefully:
  missing days (collector downtime) MUST NOT count as "below current IV"
  in the percentile denominator. Only days with valid IV observations
  contribute to the window.

- **IVS-12** Cross-underlying isolation: BTC and ETH surfaces MUST NOT be
  mixed. The feature id embeds the underlying (`iv.term.btc.1m`).

## Acceptance criteria

- [ ] `ivs_1_catalog_registration` — all feature ids registered in the 004
  catalog with correct locality (FEA-9) and warmup.
- [ ] `ivs_2_atm_iv_interpolation_correct` — fixture with strikes [90k, 100k,
  110k] and spot=100k → ATM IV interpolated at 100k to within 1e-10.
- [ ] `ivs_2_atm_uses_ticker_underlying_price` — same as GRE-10 pattern.
- [ ] `ivs_3_term_structure_falls_back_to_nearest` — if 30d expiry does not
  exist, use 28d or 32d (whichever is closer).
- [ ] `ivs_4_risk_reversal_sign_and_value` — symmetric call/put IV → RR=0;
  put-rich IV → negative RR.
- [ ] `ivs_5_vrp_positive_when_iv_above_rv` — IV=80%, RV=60% → VRP=20%.
- [ ] `ivs_5_vrp_suppressed_when_rv_unavailable` — no RV feature → VRP=None.
- [ ] `ivs_6_regime_thresholds` — IV at 70th percentile → Rich; 40th → Fair;
  20th → Cheap.
- [ ] `ivs_7_dvol_positive_and_finite` — fixture with non-zero OI → DVOL > 0.
- [ ] `ivs_7_dvol_zero_oi_strikes_excluded` — strikes with OI=0 do not affect
  DVOL.
- [ ] `ivs_8_deterministic_golden` — same fixture replayed twice →
  byte-identical feature outputs.
- [ ] `ivs_9_check_config_rejects_unknown_fields`.
- [ ] `ivs_10_proptest_vrp_bounded_by_iv_and_rv`.
- [ ] `ivs_11_percentile_skips_missing_days` — 90-day window with 10 missing
  days uses n=80, not n=90.
- [ ] `ivs_12_cross_underlying_isolation` — BTC surface excludes ETH options.

## Decisions

- 2026-08-22: New spec (extends spec 031 recording → analytics). The IV
  surface is the single most valuable derivative of the recorded option
  data. It turns per-contract IVs into a structural view of market
  expectations. This is what derivativesmonkey.com's `/surfaces` and `/term`
  pages compute.

- 2026-08-22: Linear interpolation for v1 (simple, deterministic, testable).
  SVI (Gatheral) parameterization deferred to v2 if smoothness or arbitrage
  free constraints are needed. The surface is used for analytics, not for
  pricing — interpolation artifacts are acceptable if they don't reverse the
  sign of skew or term structure.

- 2026-08-22: Moneyness normalization (K/spot) rather than strike-absolute
  makes the surface comparable across spot levels and is the standard
  convention for vol surface visualization. The surface is recomputed
  on each update (replace semantics), so the normalization adapts to
  changing spot.

- 2026-08-22: VRP uses the same time window for IV and RV. In practice,
  ATM IV for the nearest expiry may correspond to a 7-day horizon while
  RV may be computed over 30 days. The mismatch is documented; v2 can
  align windows via IV interpolation to a target tenor.

- 2026-08-22: DVOL weights by OI (not equal-weight) because illiquid strikes
  with near-zero OI should not distort the composite. This differs from
  Deribit's exact DVOL methodology (which uses a proprietary formula) but
  captures the same economic intent. The composite is the OI-weighted mean of
  VARIANCE (sqrt(Σ wᵢ·IVᵢ² / Σ wᵢ)) — variances, not vols, are what average
  meaningfully across strikes.

- 2026-08-22 (review): VRP is implemented as a PURE FUNCTION
  `vrp(iv_atm, rv)` in v1, not a live-wired feature: the RV input comes from
  a different feature family (`vol.rv.*`, bar-close locality) and bridging
  bar features into tick-feature state would require a cross-family coupling
  the engine does not yet express. Strategies/research call `vrp` with the
  two materialized series until spec 016's offline path wires it; IVS-5's
  fail-closed semantics apply at the call site (suppress when either side is
  missing).

## Open questions

- SVI vs linear interpolation — does the surface need to be arbitrage-free
  (calendar spread, butterfly) for the vol analytics to be meaningful?
  For v1 the answer is no (the analytics are regime signals, not pricing
  inputs), but this may matter for the options strategy builder (spec 039).
- Should the VRP use ATM IV for a fixed tenor (e.g. 30d) rather than the
  nearest expiry? Fixed-tenor VRP is more comparable over time but requires
  IV surface interpolation to the target tenor. Defer to implementation.
- The existing `vol.rv.{tf}.{w}` feature (spec 004) uses bar-level returns.
  Does this RV match what a vol trader would expect for the VRP? The
  annualization convention matters. Verify against Deribit's own RV
  reporting during implementation.
