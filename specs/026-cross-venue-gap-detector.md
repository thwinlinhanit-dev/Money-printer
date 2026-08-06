# 026 — Cross-Venue Gap Detector

## Purpose

Turn the multi-venue recorder into its own quality oracle. For every per-venue
gap the manifest (spec 003/024) reports, classify whether the gap is
**venue-side** (the market kept trading on other venues) or **market-wide**
(a synchronized outage), using the *other* venues' recordings of the same
underlying. This diagnoses the Phase-0 gap blocker and raises market-wide
outage alerts — without ever recovering, imputing, or backfilling the missing
data (W-6) and without relaxing the promotion gate (PD-5).

## Scope

In: an offline batch job over already-recorded per-venue data that reads
manifests (STO-2/STO-5) and events (STO-4), forms a cross-venue cohort per
gap window, classifies each gap, and writes a separate, append-only findings
artifact. Out: live streaming (offline only), editing per-venue manifests
(W-6), backfilling missing events (W-6; `ops/runbooks/stream-gap.md`), relaxing
the INT-5 promotion gate (PD-5), and new collectors (spec 002).

## Design

```
per-venue manifests (STO-2) ──┐
                              ├─▶ CrossVenueDetector ──▶ cold/cross_venue/date={d}/findings.json
per-venue events (STO-4) ─────┘            (append-only, schema-versioned)
                                                │
                                                └─▶ (optional) INT-5 scorecard annotation
```

### Symbol cohort

Tickers differ per venue (Binance `BTCUSDT` perp, Bybit `BTCUSDT` perp, OKX
`BTC-USDT-SWAP`, Hyperliquid `BTC`, Coinbase `BTC-USD` spot). A `symbol_cohort`
config table maps one logical underlying to its per-venue symbols; only venues
present in the cohort AND with a recording for the date participate.

### Reconciliation axis

Venues report trades differently (`aggTrade` vs `publicTrade` vs fills), so
trades do NOT pair 1:1. Reconcile on a normalized per-window aggregate, not raw
trades. For each gap window `[from_ns, to_ns]` on (venue V, symbol S), for every
other cohort venue U: query the Dataset reader (STO-4) for S's trades in the
window and compute `trade_count`, `vwp` (Σ notional / Σ qty), and consult U's
manifest for any gap overlapping the window.

### Classification

- **corroborated_venue_side** — ≥ `min_corroborators` (default 1) cohort venues
  corroborate. U corroborates iff: (a) U's manifest has no gap overlapping more
  than `cohort_gap_overlap` (default 0.5) of the window, AND (b) `trade_count`
  ≥ `min_trades` (default 1), AND (c) U's `vwp` over the window is within
  `max_price_band_pct` (default 5%) of U's pre-gap `vwp`.
- **market_wide** — the cohort is non-empty AND every cohort venue has a gap
  overlapping the window (no corroborator is possible); a synchronized outage.
  Raises an alert (spec 009).
- **isolated_unknown** — fewer than `min_cohort` (default 2) cohort venues
  available, or corroboration is inconclusive (incl. non-finite `vwp`, CVG-10).
  No claim is made; the gap stays exactly as the manifest recorded it.

### Findings artifact

```json
{ "schema_ver": 1, "date": "2026-08-04", "detector_version": "<git-sha>",
  "config_hash": "<sha256>", "findings": [
    { "venue": "binance", "symbol": "BTCUSDT",
      "gap_id": "<refs manifest gap>", "from_ns": 0, "to_ns": 0,
      "classification": "corroborated_venue_side",
      "cohort": [{"venue":"bybit","corroborates":true,"trade_count":0,"vwp":0.0},
                 {"venue":"okx","corroborates":true,"trade_count":0,"vwap":0.0}],
      "evidence": "2/2 cohort venues continuous, price within 1.3%" } ] }
```

## Requirements

- **CVG-1** The detector MUST run offline, reading per-venue gap windows from
  manifests (STO-2/STO-5) and events from the Dataset reader (STO-4). It MUST
  NOT open network connections (CONV-9, PD-4) and MUST NOT depend on
  `collectors` or any networking crate (CONV-3).
- **CVG-2** The detector MUST be a pure function of (recorded events,
  manifests, config, seed). Venue and symbol iteration MUST use `BTreeMap` /
  sorted order (CONV-10). The same inputs MUST produce byte-identical findings
  (golden hash test, mirroring MAT-5 / CONV-12).
- **CVG-3** For each per-venue gap window on symbol S at venue V, the detector
  MUST form a cohort from the other cohort venues that have a recording for the
  date. If the cohort has fewer than `min_cohort` (default 2) venues, the
  finding MUST be `isolated_unknown` — no corroboration claim is made.
- **CVG-4** A cohort venue U corroborates iff (a) U's manifest has no gap
  overlapping more than `cohort_gap_overlap` (default 0.5) of the window, AND
  (b) U's `trade_count` over the window is ≥ `min_trades` (default 1), AND
  (c) U's `vwp` over the window is within `max_price_band_pct` (default 5%)
  of U's pre-gap `vwp`. The gap is `corroborated_venue_side` iff ≥
  `min_corroborators` (default 1) cohort venues corroborate.
- **CVG-5** If the cohort is non-empty and every cohort venue has a gap
  overlapping the window, the finding MUST be `market_wide` and an alert MUST
  be raised (spec 009). A synchronized outage is operationally significant,
  never silent.
- **CVG-6** Corroboration MUST NOT recover, impute, or backfill the gapped
  venue's missing events (W-6; `stream-gap.md`: "the gap is a fact"). The
  per-venue manifest's gap entry MUST remain unchanged by this detector.
  Findings are classification evidence, not data.
- **CVG-7** Cross-venue findings MUST NOT relax the INT-5 promotion gate
  (PD-5, "Honesty over green"). A recording containing a gap is NOT promotable
  regardless of corroboration. Findings annotate; they do not excuse.
- **CVG-8** Findings MUST be written to a separate, append-only artifact
  `cold/cross_venue/date={date}/findings.json`, schema-versioned (CONV-20),
  with metadata `schema_ver`, `detector_version` (git sha), `config_hash`. The
  detector MUST NOT modify per-venue manifests (STO-2).
- **CVG-9** Config MUST be TOML with `serde(deny_unknown_fields)` (CONV-16),
  checked in as `cross_venue.toml.example` with safe defaults; the binary MUST
  support `--check-config` and `--version` (CONV-18).
- **CVG-10** Any non-finite computed value (`vwp`, price band) MUST fail-closed
  (CONV-8): the finding is `isolated_unknown` with a WARN, never a silent
  default. NaN/inf MUST NOT propagate into findings.
- **CVG-11** Tests MUST use recorded fixtures under `testdata/` and MUST NOT
  hit the network (CONV-23). Every acceptance criterion maps to ≥1 test whose
  name embeds the requirement ID (CONV-21).
- **CVG-12** A binary `mp-cross-venue` in `storage/src/bin/` MUST provide the
  detector, reusing `mp_storage::Dataset`; it MUST accept `--date`, `--symbol`,
  `--venue`, `--config`, `--check-config`, `--version`. It is a sibling of
  `mp-audit` (INT-3) and `mp-migrate`.

## Acceptance criteria

- [ ] `cvg_1_reads_manifests_and_dataset_offline` — runs with no network; reads
  gaps from manifests and trades from the Dataset reader.
- [ ] `cvg_2_output_is_deterministic` — two runs over the same fixtures yield
  byte-identical `findings.json` (golden hash).
- [ ] `cvg_3_cohort_requires_min_venues` — a gap with fewer than `min_cohort`
  cohort venues yields `isolated_unknown`.
- [ ] `cvg_4_corroborated_when_cohort_continuous` — a Binance gap with Bybit +
  OKX trading continuously across the window yields `corroborated_venue_side`.
- [ ] `cvg_5_market_wide_when_all_cohort_gapped` — all cohort venues gapped over
  the window yields `market_wide` and raises the spec-009 alert.
- [ ] `cvg_6_no_backfill_of_missing_events` — after running, the gapped venue's
  event log/manifest gap is unchanged (W-6); only the separate findings artifact
  is written.
- [ ] `cvg_7_promotion_gate_unchanged` — a gapped recording is still reported
  non-promotable by the INT-5 scorecard after corroboration (gate not relaxed).
- [ ] `cvg_8_artifact_is_separate_and_append_only` — findings written to
  `cold/cross_venue/date={d}/`; per-venue manifests byte-identical before/after.
- [ ] `cvg_9_check_config_rejects_unknown_fields` — an unknown TOML key fails
  `--check-config`; `--version` prints the git SHA.
- [ ] `cvg_10_nan_fails_closed` — a fixture producing non-finite `vwp` yields
  `isolated_unknown` + WARN, with no NaN in findings.

## Decisions

- 2026-08-04: New spec, motivated by the Phase-0 gap blocker (ROADMAP current
  status: 0 clean days vs required 7) and the spec 024 2026-08-04 incident —
  from this network Binance futures drops `aggTrade`/`markPrice`/`forceOrder`
  while Bybit/OKX/Hyperliquid WS connect normally. Cross-venue corroboration
  classifies those Binance gaps as `corroborated_venue_side` immediately,
  separating venue-side drops (the common case) from recorder bugs.
- 2026-08-04: Corroboration is deliberately NOT a recovery path (CVG-6) and NOT
  a gate relaxation (CVG-7). Recorded data is append-only (W-6) and a gap is a
  fact (`stream-gap.md`); the detector only *classifies* for diagnosis, honest
  annotation, and market-wide alerting. Relaxing INT-5 would violate PD-5
  ("Honesty over green").
- 2026-08-04: The detector lives in `storage` (sibling of `mp-audit`/
  `mp-migrate`) and reuses `mp_storage::Dataset` — no new crate, no new network
  dependency (CVG-1). This stays inside the "you may freely implement specs"
  boundary (CLAUDE.md safety table); no external network dependency is added.
- 2026-08-04: Reconcile on per-window `trade_count` + `vwp` + manifest overlap,
  NOT raw trade matching — venues' trade reporting differs (`aggTrade` vs
  `publicTrade` vs fills), so 1:1 pairing is meaningless. Continuity + price
  band is the honest common axis.
- 2026-08-04: A `symbol_cohort` config table maps one underlying to per-venue
  symbols (tickers differ: `BTCUSDT` vs `BTC-USDT-SWAP` vs `BTC` vs `BTC-USD`).
  Open: whether spot venues (Coinbase `BTC-USD`) may corroborate perp gaps —
  the price band absorbs basis, but spacing/liquidity differs; default allow,
  grade via event study (RES-4) before trusting in any strategy.
- 2026-08-04 (impl): CVG-1..12 implemented — `storage/src/cross_venue.rs`
  (detector + config + findings), `storage/src/bin/mp-cross-venue.rs` (CVG-12
  flags: `--data-dir`, `--date`, `--venue`, `--symbol`, `--config`,
  `--check-config`, `--version`), `storage/tests/cross_venue.rs` (cvg_1..cvg_10
  all pass). Judgment calls recorded (W-5): `Finding.venue` /
  `CohortMember.venue` store `layout::venue_slug` (e.g. "binance_futures") so
  they round-trip with cohort-config keys (`Venue::from_slug` accepts both the
  short and partition slug forms). The CVG-5 market-wide alert is a
  `tracing::warn!` + the `MarketWide` classification; wiring to Telegram is
  spec 009, out of the storage crate's scope. `detect()` takes
  `detector_version` + `config_hash` as parameters so the golden test is
  build-env independent. `vwp` is `Option<f64>` (`None` when no trades) so
  NaN is never serialized — serde_json rejects non-finite f64, which is the
  CVG-10 fail-closed proof plus a `vwp.is_none()` assertion. Bin `--version`
  uses the `MP_GIT_SHA` build env or `"dev"` (CONV-18; honest, never
  fabricated). `toml.workspace = true` added to `storage/Cargo.toml` for
  config parsing (pure parser, not a network dependency — not gated).

## Open questions

- Should `corroborated_venue_side` gaps feed a *research-mode* Dataset read
  path that skips gapped windows honestly (vs SIM-6's hard abort)? Useful for
  backtests that tolerate known venue-side gaps; needs a SIM-6 amendment and
  owner sign-off (it touches the "silence a gap" boundary). Do NOT implement
  without a spec change.
- Spot-as-corroborator validity (see Decisions) — resolve with an event study.

