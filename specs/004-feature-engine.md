# 004 — Streaming Feature Engine

## Purpose
Turn raw events into named, versioned, timestamped features consumed by
strategies (live) and research (offline) — from the SAME code. This is the
"one code path" pillar: features computed in a notebook must equal features
computed live, to the bit.

## Scope
In: feature framework, the v1 feature catalog with formulas, bar building,
materialization. Out: signals/strategies (006), ML models (future spec).

## Design

```
EventEnvelope stream ──▶ FeatureEngine ──▶ FeatureUpdate { feature_id, symbol,
                                            ts_ns, value: f64, ver: u16 }
```

- Features are `struct X: Feature { fn on_event(&mut self, ev, ctx) ->
  Option<f64> }` — pure, deterministic (CONV-9), registered in a catalog with
  id, name, version, params, warmup.
- **As-of discipline:** a `FeatureUpdate` at `ts_ns` uses only events with
  `recv_ts_ns <= ts_ns`. The engine enforces this structurally: features see
  events in stream order, period.
- Offline materialization: the engine run over the Dataset reader (003) writes
  `features/{feature}/venue=…/symbol=…/date=…` Parquet — the feature store.
  Research reads Parquet; live reads the in-memory stream; same numbers.

### Bars
`BarBuilder` produces time bars (1s/1m/5m/1h) and volume bars (config
threshold) from trades: `{o,h,l,c, vol, buy_vol, sell_vol, vwap, n_trades,
first_ts_ns, last_ts_ns}`. Bar close is the ONLY place bar-derived features
update (no intra-bar repaint — repainting features are banned).

### v1 Feature catalog (id → definition)

**Order flow**
- `cvd.{venue}` — cumulative Σ(signed trade qty): +qty Buy aggressor, −qty Sell.
  Reset: never (session-relative views are consumer-side).
- `cvd.agg` — Σ over configured venues of notional-signed flow
  (Σ signed qty·price), venues weighted 1.0.
- `delta.bar.{tf}` — per-bar buy_vol − sell_vol.
- `trade_size.p95.{w}` — rolling p95 of trade notional over window `w`
  (P² quantile estimator; document estimator choice — determinism).
- `sweep` — event feature: single-side aggressor volume ≥ `k_sweep` × rolling
  median bar volume within ≤ `sweep_window_ms` (defaults k=3, 500ms) AND price
  displacement ≥ `n_ticks_min`. Emits 1.0 with side sign (+buy/−sell).
- `absorption.{tf}` — |delta.bar| / (|close−open| in ticks + 1); high value =
  aggression without displacement. Emitted at bar close.

**Liquidity / book** (require BookMirror, EVT-9)
- `depth.{bps}.{side}` — resting qty within ±bps of mid (bps ∈ {10,25,50}),
  sampled on a 1s timer aligned to event stream (SimClock-driven).
- `imbalance.{bps}` — (bid_depth − ask_depth)/(bid_depth + ask_depth).
- `spread.ticks` — (best_ask − best_bid)/tick_size, 1s samples.
- `wall_persistence.{bps}` — for levels ≥ `wall_min_notional`: seconds
  survived since appearance; emitted on removal (survival time) — feeds
  spoof-vs-real research.

**Derivatives**
- `funding.{venue}` — passthrough of Funding.rate; `funding.zscore.{w}` —
  z-score vs rolling `w` (default 30d of intervals).
- `basis.{a}_{b}` — (mark_a − index_b)/index_b between venue pairs.
- `oi.delta.{tf}` — ΔOI per bar; `oi.quadrant.{tf}` — categorical {1..4} from
  sign(Δprice)×sign(ΔOI) (1: ↑p↑oi new longs, 2: ↑p↓oi short cover,
  3: ↓p↑oi new shorts, 4: ↓p↓oi long liquidation).
- `liq.cluster` — event feature: Σ liquidation notional within
  `cluster_window_s` (default 10s) ≥ `cluster_min_notional`; value = notional,
  sign = side being liquidated (− for longs liquidated).

**Cross-venue**
- `px.divergence.{a}_{b}` — (mid_a − mid_b)/mid_b, 1s samples.
- `leadlag.{a}_{b}.{w}` — argmax cross-correlation lag of 1s returns over
  window `w` (research-grade; offline materialization only in v1 — FEA-9).

**Regime**
- `vol.rv.{tf}.{w}` — realized vol: √(Σ r² over w bars), annualized.
- `regime.vol` — {Low, Mid, High} by rolling percentile (33/66) of rv over 90d.
- `regime.trend` — {Trend, Chop} via Efficiency Ratio = |Σr| / Σ|r| over `w`
  bars ≥/< threshold (default 0.3).

## Requirements
- **FEA-1** Feature trait + catalog + engine MUST exist as designed; feature
  ids and formulas above are normative (rename = schema change, CONV-20).
- **FEA-2** Engine MUST enforce as-of ordering structurally; there MUST be no
  API to query "current" external state from inside a feature (PD-3).
- **FEA-3** Every feature declares `warmup()`; the engine suppresses outputs
  until warmup is satisfied — emitting pre-warmup values is a fault.
- **FEA-4** Offline (Dataset) and online (Ring) runs over identical event
  sequences MUST produce identical FeatureUpdate sequences (golden test —
  this IS the one-code-path guarantee).
- **FEA-5** NaN handling per CONV-8: validate, suppress, count, WARN.
- **FEA-6** Materialization MUST write the feature store layout with footer
  metadata {feature ver, engine git sha, params hash}; changed params/version
  ⇒ new directory (`ver=N`), never overwrite (W-6).
- **FEA-7** All catalog params (windows, thresholds) MUST live in one
  `features.toml` (deny_unknown_fields), hashed into materialization metadata.
- **FEA-8** Book-derived features MUST refuse to emit while BookMirror is
  stale (gap ⇒ silence, not stale numbers).
- **FEA-9** `leadlag.*` is offline-only in v1 (cost); catalog marks features
  `online | offline | both`; the engine rejects running offline features live.
- **FEA-10** Screener support: a `RuleSet` evaluator over FeatureUpdates
  (boolean expressions with comparison + AND/OR + time-windowed persistence,
  parsed from TOML) emitting `ScreenerHit {rule_id, symbol, ts_ns, snapshot:
  Map<feature_id, f64>}` — hits are journaled (research grades them weekly).

## Acceptance criteria
- [ ] Golden online/offline identity test on a 1-day fixture (FEA-4).
- [ ] Unit tests per catalog feature against hand-computed fixtures (each formula above gets at least one numeric example test).
- [ ] Warmup suppression test (FEA-3); stale-book silence test (FEA-8).
- [ ] Screener: rule TOML → hits on fixture stream with exact expected snapshots (FEA-10).
- [ ] Materialization reruns with changed params land in `ver=2` without touching `ver=1` (FEA-6).

## Decisions
- 2026-07-10: features output scalar f64 only in v1 (categoricals encoded as
  small ints); vector-valued features deferred.

## Open questions
- None blocking; `wall_min_notional` defaults need per-symbol calibration
  during implementation (record chosen values in features.toml comments).
