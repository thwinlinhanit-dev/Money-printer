# 039 — Cross-Exchange Options Flow Aggregator

## Purpose

Aggregate options trade flow across exchanges — detecting large/block trades,
net premium flow, directional positioning by moneyness, and whale activity —
from the recorded OptionTrade events (spec 031) and any future multi-venue
option feeds. Flow analytics reveal what sophisticated traders are doing
before it moves spot.

This is the "smart money" analytics layer. It does NOT execute trades; it
emits flow features consumed by strategies (spec 006) and the analytics
terminal (spec 041).

## Scope

**In:** Large trade detection (block threshold), net premium flow, flow by
moneyness (ITM/ATM/OTM), trade count and volume aggregation, per-expiry flow
decomposition, cross-exchange flow divergence (when multi-venue option data
is available), flow momentum (acceleration/deceleration of directional flow).

**Out:** Block/RFQ venue integration (Paradigm, Derive — deferred to a future
collector spec when those venues' option feeds are added), options strategies
(BACKLOG [v2]), execution decisions.

## Design

### Data Flow

```
OptionTrade events (spec 031)
  │  { leg: OptionLeg, price, qty, side, trade_id, ts_ns }
  │  OptionLeg = { underlying, strike, expiry_ts_ns, kind: Call|Put }
  ▼
Flow Aggregator
  │  Keyed by: (underlying, window)
  │
  ├──▶ flow.net_premium.{underlying}.{window}    — signed premium flow
  ├──▶ flow.net_delta.{underlying}.{window}      — delta-adjusted notional
  ├──▶ flow.block.{underlying}.{window}          — large trade detection
  ├──▶ flow.by_moneyness.{underlying}.{window}   — ITM/ATM/OTM breakdown
  ├──▶ flow.by_expiry.{underlying}.{window}      — per-expiry flow
  ├──▶ flow.call_put_ratio.{underlying}.{window} — call volume / put volume
  ├──▶ flow.accl.{underlying}.{window}           — flow acceleration (2nd deriv)
  └──▶ flow.whale.{underlying}.{window}          — whale-sized trade activity
```

### Block Trade Detection (spec 039-BLOCK)

A trade is classified as a **block** when:
```
|notional| = |price × qty × contract_multiplier| ≥ block_threshold_usd
```

Where `block_threshold_usd` is configurable (default: $100,000). The threshold
is in USD notional, not contract count, because BTC vs ETH options have
vastly different per-contract values.

Block trades are the most informative flow signal — they represent
institutional or whale-sized positioning. Each block is tagged with:
- `side`: Buy/Sell aggressor (from OptionTrade side)
- `kind`: Call/Put (from OptionLeg)
- `notional_usd`: absolute USD value
- `moneyness`: ITM/ATM/OTM classification
- `time_to_expiry`: days until expiry

The block feature emits the aggregate signed notional of block trades in the
window:
```
flow.block.{underlying}.{window} = Σ sign(side) × notional_usd  (blocks only)
```

### Net Premium Flow (spec 039-NPF)

The signed premium paid/received across all option trades in a window:
```
flow.net_premium.{underlying}.{window} = Σ (side_sign × price × qty × multiplier)
```

Where `side_sign` = +1 for buy aggressor (premium paid by buyer), −1 for
sell aggressor (premium received by seller).

- Positive net premium = net buying pressure (bullish if calls dominate,
  hedging if puts dominate)
- Negative net premium = net selling pressure

This is the most direct measure of directional options flow.

### Net Delta-Adjusted Flow (spec 039-DAF)

Delta-adjusted notional accounts for the directional exposure of each trade:
```
flow.net_delta.{underlying}.{window} = Σ (side_sign × delta(K) × notional_usd)
```

Where `delta(K)` = the option's delta at the time of trade. For calls, delta
∈ [0, 1]; for puts, delta ∈ [−1, 0]. The side_sign × delta gives the
effective directional exposure:
- Buy call (side=+1, delta=+0.7) → +0.7 × notional → bullish
- Sell put (side=−1, delta=−0.3) → +0.3 × notional → bullish (seller receives
  the delta exposure)
- Buy put (side=+1, delta=−0.5) → −0.5 × notional → bearish

This is the single most informative flow metric — it translates raw options
trades into equivalent spot delta exposure.

Delta source: prefer the `greeks.delta` from the OptionTicker (spec 031)
matched by (strike, expiry). If no ticker match, fall back to a Black-Scholes
delta computation from (mark_iv, spot, strike, T). Delta values MUST be
validated (CONV-8: |delta| ≤ 1.0 or suppress).

### Moneyness Classification (spec 039-MONEYNESS)

Each trade is classified by moneyness relative to spot at trade time. The
classification is **kind-aware**: ITM/OTM depends on whether the leg is a
call or a put — a 105k call with spot at 100k is OTM, but a 105k put is ITM.

```
rel      = |strike / spot - 1|
direction:
  call:  strike < spot → ITM side;   strike > spot → OTM side
  put:   strike > spot → ITM side;   strike < spot → OTM side

ATM:     rel < atm_threshold            (default: 0.02, i.e. ±2%)
ITM:     rel ≥ atm_threshold AND on the ITM side AND rel ≤ itm_cap
         (default: 0.10)
OTM:     rel ≥ atm_threshold AND on the OTM side AND rel ≤ otm_cap
         (default: 0.50)
Deep OTM: rel > otm_cap (excluded from flow to avoid noise)
```

The spot reference is the matched OptionTicker's `underlying_price`
(same contract as specs 037/038). Trades that arrive before any ticker for
the underlying (no spot reference yet) are suppressed from moneyness-bucketed
aggregation — fail-closed, never classified against a stale or invented spot.

The flow by moneyness reports separately for each bucket:
```
flow.by_moneyness.{underlying}.{window}.{bucket} = Σ signed_notional
```

Where `bucket` ∈ {itm, atm, otm}. OTM call buying is speculative bullish;
OTM put buying is hedging/bearish; ITM call buying is effectively spot
replacement. The moneyness decomposition reveals the CHARACTER of the flow,
not just the direction.

### Per-Expiry Flow (spec 039-EXPIRY)

Flow decomposed by time-to-expiry:
```
flow.by_expiry.{underlying}.{window}.{tenor} = Σ signed_notional
```

Where `tenor` ∈ {weekly, monthly, quarterly, leap} based on days-to-expiry:
- Weekly: ≤ 7 days
- Monthly: 8–45 days
- Quarterly: 46–180 days
- Leap: > 180 days

Short-dated flow = tactical/speculative; long-dated flow = structural positioning.

### Call-Put Ratio (spec 039-CPR)

```
flow.call_put_ratio.{underlying}.{window} = call_volume / put_volume
```

Where volume = Σ |notional_usd| for each side. CPR > 1.0 = more call activity
(bullish); CPR < 1.0 = more put activity (bearish/hedging). CPR is informational
— it does NOT distinguish between buying and selling (a CPR spike from put selling
is bullish; from put buying is bearish). Must be interpreted alongside net
premium direction.

### Flow Acceleration (spec 039-ACCL)

Second derivative of net premium flow:
```
flow.accl.{underlying}.{window} = NPF(t) - NPF(t-1)
```

Where NPF is the net premium flow over successive windows. Positive acceleration
= flow is increasing (momentum building); negative = flow is decelerating
(exhaustion candidate).

Acceleration is emitted only when ≥ 2 complete windows exist (warmup).

### Whale Flow (spec 039-WHALE)

Whale trades are block trades above a higher threshold:
```
whale_threshold_usd = max(whale_floor_usd, k_whale × p95_notional)
```

Defaults: `whale_floor_usd` = $500,000, `k_whale` = 3.0. The p95 is computed
over a rolling window of all trades (same P² quantile estimator as spec 004
`trade_size.p95.{w}`).

The whale feature emits:
```
flow.whale.{underlying}.{window} = {
  count: usize,
  net_notional: f64,    // signed aggregate
  call_notional: f64,   // absolute, calls only
  put_notional: f64,    // absolute, puts only
}
```

Whale flow divergence from spot price is screener fodder (spec 004/006).

### Windowing

All features support configurable windows (TOML config):
- `flow_window_short` (default 1h) — tactical view
- `flow_window_medium` (default 4h) — session view
- `flow_window_long` (default 24h) — structural view

Emission: each feature emits once per closed window (bar-close semantics,
spec 004 `BarFeature`). The window boundary is driven by the feature engine's
clock injection (CONV-5).

### Cross-Exchange Divergence (spec 039-XDIV)

When option data is available on multiple venues (today: Deribit only;
future: Derive, Bybit if they add options), the flow aggregator computes:

```
flow.xdiv.{underlying}.{window}.{venue_a}_{venue_b} = net_delta_a - net_delta_b
```

Large divergence = venues disagree on directional options flow (one venue
sees buying, the other sees selling). This is the options analog of
`liq.delta.{a}_{b}` (spec 029) and `px.divergence.{a}_{b}` (spec 004).

In v1 (single venue: Deribit), this feature is suppressed (None emission).
It lights up automatically when a second venue's option trades are recorded.

## Requirements

- **OFI-1** The flow aggregator MUST consume `OptionTrade` events from the
  feature engine (spec 004) and produce features registered in the 004
  catalog with ids matching the design above. Feature ids are normative
  (CONV-20).

- **OFI-2** Block trade detection MUST classify trades where
  `|price × qty × multiplier| ≥ block_threshold_usd` (configurable,
  default $100k). The notional MUST use the OptionTrade's price and qty,
  not the OptionTicker's mark_price.

- **OFI-3** Net premium flow MUST be signed: buy aggressor = +premium,
  sell aggressor = −premium. The sign MUST come from the OptionTrade's
  `side` field.

- **OFI-4** Net delta-adjusted flow MUST match OptionTrade events to
  OptionTicker delta values by (strike, expiry). When no match exists,
  a Black-Scholes delta is computed from configurable IV/spot/strike/T.
  |delta| > 1.0 MUST be suppressed (CONV-8).

- **OFI-5** Moneyness classification MUST use `strike / underlying_price`
  from the matched OptionTicker (spec 037/038 pattern — same reference).
  Deep OTM trades (beyond otm_cap) MUST be excluded from aggregation
  to avoid noise from near-worthless options.

- **OFI-6** Per-expiry tenor classification MUST use days-to-expiry
  from `expiry_ts_ns - trade_ts_ns`, with the window boundaries
  configurable (defaults: 7/45/180 days).

- **OFI-7** Flow acceleration MUST suppress emission until ≥ 2 complete
  windows exist (FEA-3 warmup). The acceleration is the difference of
  the two most recent window totals, not a smoothed derivative.

- **OFI-8** Whale threshold MUST be dynamically calibrated using the P²
  quantile estimator (spec 004 `trade_size.p95.{w}`) with a configurable
  floor. The p95 window MUST match the flow window.

- **OFI-9** All computations MUST be deterministic (CONV-9..12): pure
  function of (events, config, seed); BTreeMap iteration (CONV-10); NaN/inf
  fail-closed (CONV-8).

- **OFI-10** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16),
  entries in `features.toml.example`; block/whale thresholds, windows,
  moneyness bands, tenor boundaries explicit with documented defaults.

- **OFI-11** Tests MUST use recorded Deribit fixtures, no network
  (CONV-23); requirement-ID test names (CONV-21); proptest for signed
  premium and delta-adjusted math (CONV-22).

- **OFI-12** Cross-exchange divergence MUST be suppressed (None) when only
  one venue's option data is available. It MUST NOT emit zero divergence
  — it emits nothing, because "no divergence" and "only one venue" are
  semantically different.

- **OFI-13** The flow aggregator MUST NOT duplicate or replace the existing
  spot/perp flow features in spec 004 (`cvd.*`, `whale_print`,
  `whale_flow.*`). Options flow is additive — it represents a different
  asset class (options vs perps) and feeds different strategy logic.

## Acceptance criteria

- [ ] `ofi_1_catalog_registration` — all feature ids from the design
  registered in the 004 catalog with correct locality and warmup.
- [ ] `ofi_2_block_detection_correct` — fixture with $150k trade → block;
  $50k trade → not block.
- [ ] `ofi_3_net_premium_signs_correct` — buy call → +premium; sell put →
  −premium.
- [ ] `ofi_4_delta_adjusted_calls_bullish_puts_bearish` — buy call →
  positive delta flow; buy put → negative delta flow.
- [ ] `ofi_4_delta_suppressed_when_over_one` — delta=1.5 → suppress.
- [ ] `ofi_5_moneyness_classification` — strike=105k, spot=100k → OTM call;
  strike=102k, spot=100k → ITM put (review note: the original example said
  "strike=98k → ITM put", but a 98k put with spot at 100k is OTM —
  max(0, K − S) = 0; corrected here).
- [ ] `ofi_5_moneyness_kind_aware_regression` — strike=104k, spot=100k →
  OTM **call** (the naive `|strike/spot − 1| < band` formula misclassifies
  this as ITM; guards the kind-aware fix).
- [ ] `ofi_5_deep_otm_excluded` — strike=150k, spot=100k (50% OTM) → excluded
  from aggregation.
- [ ] `ofi_6_tenor_classification` — 3d → weekly; 20d → monthly; 90d →
  quarterly; 270d → leap.
- [ ] `ofi_7_acceleration_suppressed_until_warmup` — first window → None.
- [ ] `ofi_8_whale_threshold_dynamic` — with p95=$200k and k=3, threshold=$600k
  (or floor, whichever is larger).
- [ ] `ofi_9_deterministic_golden` — same fixture → byte-identical.
- [ ] `ofi_10_check_config_rejects_unknown_fields`.
- [ ] `ofi_12_xdiv_suppressed_single_venue` — Deribit-only → flow.xdiv=None.
- [ ] `ofi_13_spot_flow_not_duplicated` — assert no conflict with cvd.*
  feature ids.

## Decisions

- 2026-08-22: New spec. Options flow is the second major analytics layer
  on top of spec 031 recording. While spec 037/038 compute structural
  Greeks and vol, this spec captures the DYNAMIC flow — what traders are
  actually doing. Derivativesmonkey.com's `/tape`, `/order-flow`, and
  `/volume-flow` pages compute exactly these metrics.

- 2026-08-22: USD notional for block/whale thresholds (not contract count)
  because 1 BTC option ≈ $100k+ while 1 ETH option ≈ $3k+. A 10-contract
  BTC block is very different from 10-contract ETH. USD normalizes.

- 2026-039-08-22 → 2026-08-22 (typo fix): Delta-adjusted flow is the most valuable flow metric
  because it directly quantifies the spot-equivalent directional exposure.
  Raw volume or premium can be misleading (e.g., high put buying volume
  could be hedging, not bearish positioning — delta-adjustment clarifies).

- 2026-08-22: Deep OTM exclusion (OFI-5) prevents a single $50k trade in
  a 200% OTM call from dominating the flow aggregation. The otm_cap
  (default 50%) is configurable per asset — altcoin options may have
  wider ranges.

- 2026-08-22: Cross-exchange divergence is designed NOW but suppressed in v1.
  The feature id exists in the catalog, the logic is implemented, but it
  emits None until a second venue's option trades are recorded. This is the
  "record now, analyze later" pattern — when the second venue lights up,
  the feature is already wired.

- 2026-08-22: Flow acceleration is a simple first-difference, not an
  exponential moving average. EMA would be smoother but introduces a
  lookback parameter that complicates determinism. The screener (spec 004
  FEA-10) can smooth the acceleration signal on the consumer side.

- 2026-08-22 (review): The whale threshold's p95 is computed over the same
  rolling window that CONTAINS the whales — large prints inflate their own
  threshold (self-referential bias). Accepted for v1 because the floor
  ($500k) dominates in practice for Deribit BTC options and a whale-free
  window must not produce a degenerate threshold; the bias is conservative
  (raises the bar). Revisit only if whale counts look suppressed on real
  data.

- 2026-08-22 (review): v1 computes p95 by EXACT sort of the current window's
  trade notionals (deterministic, CONV-9/CONV-10 friendly), not the P²
  streaming estimator referenced from spec 004 `trade_size.p95.{w}`. Option
  trade counts per window are small (hundreds, not millions), so exact sort
  is affordable and removes estimator drift between replay and live. OFI-8's
  "MUST match the flow window" is unchanged.

- 2026-08-22 (review): The Black-Scholes delta fallback (OFI-4) uses a
  rational-approximation normal CDF (Abramowitz–Stegun 7.1.26), pure and
  deterministic — no external math crate. T = (expiry_ts_ns − trade_ts_ns) /
  (365.25 × 86400 × 1e9); T ≤ 0 or σ ≤ 0 → suppress (fail-closed).

## Open questions

- Paradigm/RFQ block flow: Derivativesmonkey.com integrates Paradigm's
  anonymous block venue and Derive's RFQ flow. Should we add a dedicated
  collector for these venues when they become available, or derive block
  signals purely from the lit exchange tape? The answer affects whether
  OFI-2's block detection is the primary source or a fallback.
- Minimum quote size for options: Deribit's minimum order size for options
  varies by strike/expiry. Should the block threshold account for the
  venue's minimum size (i.e., a "block" is ≥ N × minimum)? Defer.
