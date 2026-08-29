# 047 — Coinalyze Cross-Exchange Validation Collector

## Purpose

Provide real, cross-exchange derivatives data to **validate and contextualize**
existing models — NOT to chase "alpha." Specifically:
- Cross-exchange OI validates `oi_regime` (spec 045, currently Hyperliquid-only)
- `liquidation-history` provides ground truth for `liq_est_bands` (spec 029 / RES-4),
  which Hyperliquid cannot natively provide (no liq stream)
- `long-short-ratio-history` is a genuinely **new sentiment feature**

**Except the two honest no-go's (PD-5):** predicted-funding is public telegraphy
(priced in — context, NOT alpha) and intraday history is only ~1500–2000 points
(old data deleted daily) — so this is for **validation**, not for a permanent
multi-year intraday backtest (use CryptoDataDownload / our own recorder for that).

## Scope

**In**: a poller (`mp-coinalyze`) calling `api.coinalyze.net/v1` endpoints for OI,
funding, liquidation-history, long/short-ratio and — for research only — funding
and OHLCV. Written to `cold/macro/` (spec 030 MAC-6 pattern), stamped
`source: coinalyze`, `fidelity: external_api:coinalyze-v1` — never the live
recorder's `data/raw/` (W-6).
**Out**: relabeling as owned data, using any of it as a live execution signal
(before the funnel promotes something off it), long-history intraday backtests.

## Design

```
Coinalyze API (api.coinalyze.net/v1, COINALYZE_API_KEY from env)
   ──REST cadence──▶ mp-coinalyze
   /open-interest, /funding-rate, /liquidation-history, /long-short-ratio-history
        │  MacroPoint (Venue::Coinalyze) ──AGG_OI_*/AGG_FUNDING_*/AGG_LS_*/AGG_LIQ_*
        ▼
   cold/macro/ (source=coinalyze, fidelity=external_api:coinalyze-v1; manifest)
```

Key from env `COINALYZE_API_KEY` (CONV-17, PD-2 — never in repo/config/`.example`).
Coinalyze free tier: 40 requests/min — budget ~120 calls/day (hourly BTC+ETH) ≈ 5%
of limit.

## Requirements

- **COZ-1** MUST read the API key from env `COINALYZE_API_KEY` (CONV-17, PD-2 —
  never in repo/config/`.example`/fixtures). New external host ⇒ owner sign-off.
- **COZ-2** MUST write `MacroPoint` data append-only to `cold/macro/` (spec 030
  MAC-6 pattern) with a per-day manifest stamped `source: coinalyze`, `fidelity:
  external_api:coinalyze-v1` — the distinct `source` label keeps the external
  provenance auditable, never mixed into the live tick streams (W-6). MUST NOT write
  to `data/raw/` or `cold/trades/`.
- **COZ-3** MUST emit observations through the existing `MacroPoint` event variant
  (spec 001/030), enveloped with the appended `Venue::Coinalyze` (spec 001
  amendment). `series_id` follows the documented convention (`AGG_OI_{SYM}`,
  `AGG_FUNDING_{SYM}`, `AGG_LS_{SYM}`, `AGG_LIQ_{SYM}`, SYM ∈ {BTC, ETH}). No new
  `MarketEvent` variant — the only codec change is the appended venue (additive).
- **COZ-4** MUST be deterministic in normalization (CONV-9..12); BTreeMap for
  symbol order (CONV-10); `recv_ts_ns` at response read (COL-5). Cadence default
  1/hour for OI/funding/liq + 1/day for long-short.
- **COZ-5** MUST respect the API rate limit (default 1 req/s, configurable) with
  retry-backoff (mirrors HBS-8); a 404/throttle fails-closed, never fabricates.
- **COZ-6** MUST be idempotent/resumable via per-date/per-cursor completion marker
  (HBS-4/7).
- **COZ-7** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16), checked in as
  `coinalyze.toml.example` (symbols, endpoints, ERROR NO key); binary supports
  `--config`, `--check-config`, `--version` (CONV-18).
- **COZ-8** Tests MUST use recorded Coinalyze fixtures, no network, no real key
  (CONV-23, PD-2); requirement-ID names (CONV-21); proptest for serialization
  (CONV-22); NaN fail-closed (CONV-8); no panic (CONV-15); no `unwrap` (CONV-13).
- **COZ-9** The `predicted-funding-rate` endpoint, IF used, MUST be treated as
  *context/feature only* — MUST NOT be promoted as a standalone alpha signal
  (it is public telegraphy, priced in; PD-5 honesty).
- **COZ-10** MUST NOT be a source for multi-year intraday backtests (retention is
  intraday-limited); directive to use CryptoDataDownload / own recorder for that.

## Acceptance criteria (initial)

- [ ] `coz_1_key_from_env_not_repo` (PD-2: no key in config/example/fixtures)
- [ ] `coz_2_writes_to_cold_external_never_cold_trades` (W-6)
- [ ] `coz_3_event_variant_roundtrips` (CONV-22)
- [ ] `coz_4_normalization_deterministic` (golden)
- [ ] `coz_5_rate_limit_and_backoff_no_fabricate`
- [ ] `coz_6_idempotent_resumable`
- [ ] `coz_9_predicted_funding_context_only`
- [ ] `coz_10_labels_retention_limit`

## Decisions

- 2026-08-26 (owner sign-off): New spec (PD-6) from
  `research/external_data_sources_findings.md` revision. Classified
  **validation/context**, NOT alpha (PD-5). Explicitly corrects v1's claim that
  predicted-funding was a "novel alpha signal."
- 2026-08-26 (owner sign-off): Two concrete wins (COZ): validate `oi_regime` (045)
  cross-exchange + give ground truth to `liq_est_bands` (029) via
  `liquidation-history`; the long/short-ratio is the one NEW signal (sentiment) this
  source contributes.
- **2026-08-26 (owner sign-off, decision resolving COZ-3):** **reuse the existing
  `MarketEvent::MacroPoint` body** + add **one appended `Venue::Coinalyze`** variant
  (spec 001 amendment). No new `MarketEvent` variant — minimal additive change (W-5).
  New external host `api.coinalyze.net` has owner sign-off (CLAUDE.md safety table).
  `series_id` convention: `AGG_OI_{SYM}`, `AGG_FUNDING_{SYM}`, `AGG_LS_{SYM}`,
  `AGG_LIQ_{SYM}`.
- **2026-08-26 (owner sign-off):** symbol set = BTC + ETH (the two Phase-0 core
  perps). Cadence: hourly for OI/funding/liq, daily for long/short ratio.
- **2026-08-26 (owner sign-off):** validation consumption = feed the cross-exchange
  inputs into the existing `oi_regime` feature (spec 045) + a hand-built RES-4-style
  grading harness that scores `liq_est_bands` against `liquidation-history` (the
  `lig_est_bands` calibration the findings doc called for).

## Open questions

- None (resolved 2026-08-26). Implementation details (exact endpoint list, cursor
  keys) belong in `coinalyze.toml.example`, not here.

## Status

**Ready** — decisions and event shape resolved + owner sign-off recorded
(2026-08-26). Implement after the Phase-0 gate and the spec 001 amendment (schema-6
venue append) land. Update `specs/README.md` status on implementation.