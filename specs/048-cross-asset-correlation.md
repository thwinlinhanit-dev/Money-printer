# 048 — Cross-Asset Correlation Feature (from Own Recorded Prices)

## Purpose

Compute BTC-ETH-Gold-SPX cross-asset correlations **from our own recorded price
feed** to serve as a **regime signal** for the allocator/regime detector
(SYSTEM_BLUEPRINT §7). This is the *most on-brand* addition from
`research/external_data_sources_findings.md`: it compounds our retained-moat asset
(our own recorded prices, W-6) with **zero external dependency** — the only external
inputs are the free Gold/SPX/DXY price series already available via FRED (spec 030).

**Classification:** NEW regime signal (statistical, not per-trade alpha). When
BTC-SPX correlation spikes, it indicates a risk-on/off regime; BTC-Gold correlation
tells us whether crypto is acting as safe-haven or risk asset.

## Scope

**In**: a pure feature/derivation over existing collected price data + FRED macro
inputs: BTC & ETH from our recorded perps (Hyperliquid/Bybit), Gold/SPX/DXY from
FRED (spec 030, `correlation-grade not execution-grade`). **Out**: computing
correlations from an external paid service as primary (Sharpe et al. = fallback
validation only); using the correlation as a standalone entry signal (regime
*conditioner* only); any execution decision off TradFi data without a CME license
(spec 030 MAC-5).

## Design

```text
Recorded perps (Hyperliquid/Bybit BTC, ETH) ─┐
FRED Gold/SPX/DXY (spec 030 MacroPoint)      ─┼─▶ features (spec 004) — mp-materialize
Rolling-window Pearson r over bar returns     ─┘       corr.* FeatureUpdate family
```

- **Moat-first:** compute from OUR OWN prices; FRED supplies only Gold/SPX/DXY.
  Sharpe (`sharpe.ai`) is a **fallback/validation** cross-check only, NOT the source
  of truth.
- Runs through the standard feature engine + `mp-materialize` path (spec 004/016),
  feeding the allocator/regime detector as `corr.*` features.
- Deterministic (PD-3): same inputs ⇒ byte-identical correlation series; rolling
  window (e.g. 30d at daily) over injected clock.

## Requirements

- **COR-1** MUST compute rolling Pearson correlation of bar returns over our own
  recorded BTC/ETH prices + FRED Gold/SPX/DXY (spec 030). Zero new external data for
  the crypto leg.
- **COR-2** MUST be deterministic (PD-3, CONV-9..12): fixed window, injected clock,
  BTreeMap ordering, golden-hash tested.
- **COR-3** MUST be a **feature-engine product** (spec 004) emitting `FeatureUpdate`
  under a `corr.*` family (e.g. `corr.btc_eth`, `corr.btc_gold`, `corr.btc_spx`,
  `corr.eth_spx`) — NOT a new `MarketEvent`. It computes over existing recorded
  prices + FRED `MacroPoint`s; it is derived, so it consumes the event stores it
  needs and produces features. No spec-001 schema change required.
- **COR-4** MUST label fidelity `computed` over sources `own_record + fred`;
  TradFi legs remain "correlation-grade, not execution-grade" (MAC-5). MUST NOT be
  used as a standalone entry signal — regime conditioner only.
- **COR-5** MUST handle NaN/finite fraction fail-closed (CONV-8): if a window has too
  few shared observations, emit a `NaN r` labeled band rather than a fabricated value.
- **COR-6** Sharpe / Alternative are **fallback validation only**; MUST NOT be the
  primary input.
- **COR-7** Tests MUST be fixture-local, deterministic (CONV-23); requirement-ID test
  names (CONV-21); proptest on the correlation math (CONV-22); no `unwrap` (CONV-13).

## Acceptance criteria (initial)

- [ ] `cor_1_computes_from_own_prices_plus_fred`
- [ ] `cor_2_deterministic_golden`
- [ ] `cor_3_feature_family_corr_emitted` (spec 004 / materialize path)
- [ ] `cor_4_fidelity_computed_regime_only`
- [ ] `cor_5_nan_fail_closed_on_short_window`
- [ ] `cor_7_fixtures_deterministic`

## Decisions

- 2026-08-26 (owner sign-off): New spec (PD-6) from
  `research/external_data_sources_findings.md` revision — raised from Week-2 #7 to
  the top-tier because it compounds our own data (moat-first). Zero external
  dependency for the crypto leg.
- 2026-08-26 (owner sign-off): Classified **regime conditioner**, not standalone
  signal (PD-5); Sharpe demoted to validation-only (moat-first).
- **2026-08-26 (owner sign-off, decision resolving COR-3):** this is a
  **feature-engine derivation (spec 004)**, NOT a market-data event. It consumes the
  existing recorded-price event stores + FRED `MacroPoint`s and emits `corr.*`
  `FeatureUpdate`s. **No spec-001 amendment / no schema bump** — it adds no new
  `Venue`, no new `MarketEvent`. Window = **30d (daily)** by default; pairs =
  BTC-ETH, BTC-Gold, BTC-SPX, ETH-SPX (configurable).

## Open questions

- None (resolved 2026-08-26). Window/pair/cadence knobs are feature-engine config
  (`features.toml`), not decisions.

## Status

**Ready** — decisions and shape resolved + owner sign-off recorded (2026-08-26);
needs **no schema change** (feature-engine only). Implement after the Phase-0 gate.
Update `specs/README.md` status on implementation.