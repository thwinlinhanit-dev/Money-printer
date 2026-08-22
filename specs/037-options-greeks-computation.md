# 037 — Options Greeks Computation Engine

## Purpose

Compute aggregate portfolio-level Greeks — net delta, gamma, vega, theta exposure
across the entire options chain by strike and expiry — from the recorded Deribit
OptionTicker events (spec 031). These are the building blocks for GEX profiles,
max pain, implied probability, and vol-adjusted support/resistance levels.

This is the analytical layer that sits between raw option data (spec 031) and
strategy consumption (spec 006 feature engine). It does NOT execute trades; it
emits features consumed by strategies.

## Scope

**In:** Greeks aggregation engine, GEX profile computation, higher-order Greeks
(vanna, volga, charm), max pain calculation, implied probability at expiry,
cumulative GEX, per-expiry and per-strike net exposure. Both online (streaming)
and offline (materialized Parquet). Config-driven.

**Out:** IV surface construction (spec 038), options flow analytics (spec 039),
options trading/strategies (BACKLOG [v2]), real-time WebSocket serving (spec 041),
manual UI interaction.

## Design

### Data Flow

```
OptionTicker events (spec 031)
  │  { leg: OptionLeg, mark_iv, mark_price, underlying_price, open_interest, greeks: OptionGreeks }
  │  OptionLeg = { underlying, strike, expiry_ts_ns, kind: Call|Put }
  │  OptionGreeks = { delta, gamma, theta, vega }
  ▼
FeatureEngine (spec 004) — new catalog entries
  │
  ├──▶ gex.profile.{underlying}     — net gamma × OI × contract_multiplier per strike
  ├──▶ gex.cumulative.{underlying}  — cumulative net GEX across strikes
  ├──▶ gex.max_pain.{underlying}   — the strike where total OI-weighted payout is minimized
  ├──▶ gex.implied_prob.{underlying} — implied probability distribution from delta
  ├──▶ net.delta.{underlying}      — aggregate delta exposure (OI × delta × mult)
  ├──▶ net.vega.{underlying}       — aggregate vega exposure
  ├──▶ net.theta.{underlying}      — aggregate theta exposure (daily cost of carry)
  ├──▶ vanna.{underlying}          — d(delta)/d(IV) per strike (higher-order)
  ├──▶ volga.{underlying}          — d(vega)/d(IV) per strike (higher-order)
  └──▶ charm.{underlying}          — d(delta)/d(time) per strike (higher-order)
```

### GEX Profile (spec 037-GEX)

**Net Gamma Exposure** at strike `K`:

```
GEX(K) = Σ [ γ(K) × OI(K) × spot² × contract_multiplier ]
```

Where:
- `γ(K)` = gamma from OptionTicker (per-contract; Deribit reports per-BTC gamma)
- `OI(K)` = open_interest from OptionTicker (number of contracts)
- `spot` = underlying_price from OptionTicker
- `contract_multiplier` = from SymbolMeta (1.0 for Deribit per-contract quoting)

**Sign convention:**
- Call GEX is positive (long calls are long gamma)
- Put GEX is negative (short puts are short gamma)
- **Net GEX** = sum of call + put GEX at each strike

**Interpretation:**
- **Positive net GEX → market makers/dealers are LONG gamma → moves are
  dampened (volatility suppression; dealers buy dips / sell rips to hedge)**
- **Negative net GEX → market makers/dealers are SHORT gamma → moves are
  amplified (volatility amplification; dealer hedging chases price — squeeze
  risk)**
- Zero net GEX → the "gamma flip" level

### Max Pain (spec 037-MP)

The strike `K*` that minimizes total option payout at expiry:

```
K* = argmin_K Σ [ max(0, K - S) × OI_put(K) + max(0, S - K) × OI_call(K) ]
```

Computed per expiry, reported as the closest expiry's max pain. Spot-independent
(pure function of OI and strikes).

### Implied Probability Distribution (spec 037-IPD)

From each option's delta, derive the implied risk-neutral probability of the
underlying being above that strike at expiry:

```
P(S > K) ≈ |delta_call(K)|  (for European-style; Deribit uses European BTC options)
P(S < K) = 1 - P(S > K) = |delta_put(K)|
```

The probability density at each strike is the first difference of the CDF:

```
p(K) = P(S > K) - P(S > K+Δ)   (left-difference)
```

This gives the market-implied distribution. KS-test against lognormal as a
diagnostic (fat tails, skew).

### Higher-Order Greeks (spec 037-HIGHER)

Computed from the first-order Greeks already in OptionTicker, using finite
differences across the recorded IV surface:

- **Vanna** = `dΔ/dσ` ≈ `(Δ(K, σ+δ) - Δ(K, σ-δ)) / (2δ)` — sensitivity of
  delta to IV changes. Large vanna = delta will shift sharply if IV moves.
- **Volga** (Vomma) = `d²V/dσ²` ≈ `(vega(σ+δ) - vega(σ-δ)) / (2δ)` — convexity
  of option value to IV. Positive volga = long vega gets more vega as IV rises.
- **Charm** = `dΔ/dt` ≈ `(Δ(t) - Δ(t+Δt)) / Δt` — rate of delta decay over time.
  Charm measures how delta changes as expiry approaches, independent of spot moves.

Higher-order Greeks require TWO snapshots (different IV or time) to compute via
finite differences. In offline mode, use the recorded history. In online mode,
use the previous bar's snapshot.

### Contract Multiplier Handling

Deribit BTC options: 1 BTC per contract. ETH: 1 ETH per contract.
The `contract_multiplier` from SymbolMeta converts contract-level Greeks
to unit Greeks. All aggregate computations MUST use unit Greeks (not
per-contract) so the GEX profile is denominated in base-asset units.

### Aggregation Windows

| Feature | Window | Emission |
|---|---|---|
| `gex.profile` | per-ticker update (no window — replaces previous) | on each OptionTicker event batch |
| `gex.cumulative` | per-ticker update | on each OptionTicker event batch |
| `gex.max_pain` | per-expiry | on each OptionTicker event batch |
| `gex.implied_prob` | per-expiry | on each OptionTicker event batch |
| `net.{delta,vega,theta}` | all active expiries | on each OptionTicker event batch |
| `vanna`, `volga`, `charm` | requires 2+ snapshots | on each OptionTicker event batch (after warmup) |

### Materialization

Offline materialization writes one Parquet file per underlying per date:

```
features/{feature}/ver=N/underlying=BTC/date=YYYY-MM-DD/part.parquet
```

Schema: `{ strike, expiry_ts_ns, kind, value, ts_ns }` (flat, matching
the "flat-with-metadata-columns" precedent from spec 031).

## Requirements

- **GRE-1** The Greeks computation engine MUST consume `OptionTicker` events
  from the feature engine (spec 004) and produce features registered in the
  004 catalog with ids matching the design above. Feature ids are normative
  (rename = schema change, CONV-20).

- **GRE-2** GEX profile MUST be computed as `γ × OI × spot² × multiplier`
  per strike, with calls positive and puts negative. The profile MUST be
  recomputed on every OptionTicker batch (replace semantics, not additive).

- **GRE-3** Max pain MUST be computed per expiry as the strike minimizing
  total OI-weighted payout, using the same strike set as the GEX profile.

- **GRE-4** Implied probability MUST use delta as a proxy for risk-neutral
  probability, with the CDF→PDF conversion via first-difference. The sum
  of all probability density buckets MUST equal 1.0 within tolerance
  (acceptance: ± 1e-6 on fixtures where the delta surface is complete;
  truncated chains MAY sum to less — the engine reports the covered mass).

- **GRE-5** Higher-order Greeks (vanna, volga, charm) MUST use finite
  differences across two recorded snapshots. The engine MUST suppress
  emission until ≥ 2 snapshots exist (FEA-3 warmup). The finite difference
  step δ MUST be configurable (`greeks_fd_step_iv` default 0.01, i.e. 1 vol
  point).

- **GRE-6** All computations MUST be deterministic (CONV-9..12): pure
  function of (events, config, seed); BTreeMap/sorted iteration over strikes
  and expiries (CONV-10); NaN/inf fail-closed (CONV-8).

- **GRE-7** Features MUST register in the 004 catalog with id, version,
  params, warmup; `TickFeature`/`BarFeature` locality per FEA-9; NaN
  suppression per FEA-5 (CONV-8 fail-closed).

- **GRE-8** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16),
  entries in `features.toml.example`; contract multiplier sources and FD
  step params explicit with documented defaults.

- **GRE-9** Tests MUST use recorded Deribit fixtures in `testdata/`, no
  network (CONV-23); requirement-ID test names (CONV-21); proptest for
  GEX math and max pain computation (CONV-22).

- **GRE-10** The underlying_price used in GEX MUST come from the
  OptionTicker event itself (not a separate spot feed) — the underlying
  price at the moment the Greeks were computed is the correct reference.

- **GRE-11** Per-expiry decomposition: every aggregate feature (net delta,
  net vega, etc.) MUST also be decomposable by expiry via an accessor
  method, even if the scalar emission is the total. The decomposition is
  available for materialization and the analytics terminal (spec 041).

- **GRE-12** Cross-underlying isolation: BTC and ETH option chains MUST
  NOT be mixed in the same GEX profile. The feature id embeds the
  underlying (`gex.profile.btc`), and the engine filters by
  `OptionLeg.underlying`.

## Acceptance criteria

- [ ] `gre_1_catalog_registration` — all feature ids from the design are
  registered in the 004 catalog with correct locality (FEA-9) and warmup.
- [ ] `gre_2_gex_profile_signs_and_values` — hand-computed GEX for a
  3-strike fixture (1 call, 1 put, 1 ATM) matches the engine output
  to within 1e-10.
- [ ] `gre_2_gex_replaces_on_update` — a second OptionTicker batch replaces
  the profile (not appends).
- [ ] `gre_3_max_pain_correct_on_fixture` — max pain for a known OI
  distribution matches hand-computed value.
- [ ] `gre_4_implied_prob_sums_to_one` — Σ density buckets = 1.0 ± 1e-6.
- [ ] `gre_4_implied_prob_symmetric_on_symmetric_chain` — symmetric call/put
  OI at symmetric strikes → symmetric probability distribution.
- [ ] `gre_5_higher_greeks_suppressed_until_warmup` — vanna/volga/charm
  emit None before 2nd snapshot.
- [ ] `gre_5_higher_greeks_finite_difference_correct` — known vanna input
  → hand-computed output.
- [ ] `gre_6_deterministic_golden` — same fixture replayed twice →
  byte-identical feature outputs.
- [ ] `gre_8_check_config_rejects_unknown_fields`.
- [ ] `gre_9_proptest_gex_bounded_by_oi_and_spot` — |GEX(K)| ≤ OI(K) × γ(K) × spot² × mult × 1.01
  (tolerance for float rounding).
- [ ] `gre_10_underlying_price_from_ticker` — GEX uses the OptionTicker's
  underlying_price, not an external spot source.
- [ ] `gre_11_per_expiry_decomposition` — net delta decomposed by expiry
  sums to the aggregate net delta.
- [ ] `gre_12_cross_underlying_isolation` — BTC GEX profile does not include
  ETH options.

## Decisions

- 2026-08-22: New spec (extends spec 031 — recording only → analytics layer).
  The OptionTicker events already carry mark_iv, greeks, and OI; this spec
  computes the aggregate portfolio-level Greeks that the raw per-contract
  data does not directly provide. "Record now, analyze later" thesis from
  spec 031 is paying off.

- 2026-08-22: GEX formula uses `spot²` (not `K²`) following the standard
  institutional GEX definition (SpotGamma, Squeezemetrics). The difference
  is small for near-ATM options but matters for deep OTM — spot² is the
  consensus convention.

- 2026-08-22: Higher-order Greeks use finite differences (not closed-form
  Black-Scholes second derivatives) because: (a) the Deribit ticker already
  provides first-order Greeks, so we avoid recomputing them from raw IV;
  (b) finite differences generalize to any pricing model, not just BS;
  (c) the FD step is configurable, so accuracy can be tuned.

- 2026-08-22: Max pain uses the raw OI distribution, not a modeled
  distribution. This is the standard industry calculation. The feature is
  informational — strategies may use it as a magnet level, but it is NOT a
  trading signal by itself (no edge claim, no hypothesis).

- 2026-08-22: Implied probability from delta is a first-order approximation.
  For European options (Deribit BTC/ETH), delta ≈ N(d1) and the exercise
  probability is N(d2); using |delta| as P(S>K) overestimates for ITM
  options. This is acceptable for v1 and noted in the feature metadata.

- 2026-08-22 (review): Finite-difference higher-order Greeks across time
  snapshots are CONTAMINATED approximations, not clean partial derivatives:
  charm = dΔ/dt holds spot constant, but consecutive snapshots differ in BOTH
  spot and time; vanna = dΔ/dσ likewise conflates IV and spot moves. The FD
  result is a regime indicator (direction/magnitude of second-order exposure),
  not a pricing-grade Greek. Documented here and in feature metadata; a
  closed-form BS cross-check is a v2 refinement.

- 2026-08-22 (review): The streaming engine emits SCALAR features (one f64 per
  id). Per-strike profiles (`gex.profile.*`, `gex.implied_prob.*`) are exposed
  via deterministic accessor methods on the aggregator for offline
  materialization and tests (gre_2, gre_4, gre_11 exercise them), while the
  registered catalog features are the chain-level scalars: `gex.net.{u}`,
  `gex.max_pain.{u}`, `net.{delta,vega,theta}.{u}`, `vanna.{u}`, `volga.{u}`,
  `charm.{u}`. This mirrors the GRE-11 decomposition pattern and avoids
  inventing a non-scalar emission path ahead of spec 041's needs.

## Open questions

- Should the GEX profile account for pin risk differently for weekly vs
  monthly expiries? Deribit weeklies have different liquidity; weighting
  by OI alone may overweight illiquid weeklies. Defer to implementation.
- Charm computation needs a consistent time-to-expiry reference. Should it
  use the actual wall-clock time or a model time (e.g. trading days only)?
  The clock injection (CONV-5) suggests model time, but expiry is
  calendar-based. Needs owner input.
