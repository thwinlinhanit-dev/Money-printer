# 046 — DeFiLlama Regime Collector (Stablecoin Supply + TVL + DEX Volume)

## Purpose

Feed the regime/risk-on-off detection layer (SYSTEM_BLUEPRINT §7 allocator + regime
detector; roadmap Phase 4 lead-lag) with free DeFi/L1 aggregate data: stablecoin
circulating supply, aggregate DeFi TVL, and top DEX volume. This is a genuine third
macro pillar beside FRED (spec 030) and exchange reserves (spec 034), and the
highest-RoI addition from `research/external_data_sources_findings.md` (top pick:
free, no auth, ~5 calls/day).

**Classification:** *regime/context signal*, NOT per-trade alpha — use it to
*condition* strategies and de-weight positions in risk-off regimes, never as a
standalone entry signal.

## Scope

**In**: a REST snapshotter (`mp-defillama`) that polls DeFiLlama's public API at
daily cadence and emits `MacroPoint` events (enveloped with the appended
`Venue::DeFiLlama`, spec 001 amendment) for: aggregate TVL, per-stablecoin
circulating supply (USDT/USDC), and top DEX volume.
**Out**: licensed/paid DeFiLlama data, live trading of a TVL signal (PD-1), using
this as execution-grade data.

## Design

```
DeFiLlama public API (api.llama.fi, keyless) ──REST daily──▶ mp-defillama
   /stablecoins, /v2/historicalChainTvl/{chain}, /dexs/{dex}
        │  MacroPoint { series_id, value, date, exch_ts_ns, recv_ts_ns }
        ▼
   data/raw/ + cold/macro/  (manifest source=defillama, fidelity=daily)
```

No API key (DeFiLlama free tier, keyless — PD-2 friendly). Fidelity labels and
`source` in the manifest per MAC-5/6 pattern (spec 030): `source: defillama`,
`fidelity: daily`. **MUST NOT** be relabeled as our own recorded corpus; it is
external data (`external_api:defillama-v1`, see `research/external_data_sources_findings.md`).

## Requirements

- **DEF-1** MUST poll the DeFiLlama public API (`api.llama.fi`) with NO API key
  (keyless, free tier). New external host — owner sign-off before implementation
  (CLAUDE.md safety table).
- **DEF-2** MUST emit events through the existing `MacroPoint` event variant (spec
  001/030), enveloped with the appended `Venue::DeFiLlama` (spec 001 amendment). The
  `series_id` is a stable category key from the documented set (e.g.
  `DEFI_TVL_AGG`, `USDT_SUPPLY`, `USDC_SUPPLY`, `STABLECOIN_MCAP`, `DEX_VOL_1D`) —
  see the Decisions block. No new `MarketEvent` variant; the only codec change is the
  appended venue (additive, CONV-20).
- **DEF-3** MUST be deterministic in normalization (CONV-9..12); BTreeMap for
  series_id order (CONV-10); `recv_ts_ns` at socket/response read (COL-5). Daily
  cadence (configurable, default 1/day + backfill).
- **DEF-4** MUST label fidelity `daily` and `source: defillama`; MUST be labeled
  "regime-grade, not execution-grade" and MUST NOT be used as a standalone trade
  signal (honest classification per `external_data_sources_findings.md`).
- **DEF-5** MUST write to `data/raw/` + `cold/macro/` Parquet, append-only (W-6).
  Manifest per series with `source`, `fidelity`, `sampled: false`.
- **DEF-6** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16), checked in as
  `defi_llama.toml.example` (categories/series list, keyless); binary supports
  `--config`, `--check-config`, `--version` (CONV-18).
- **DEF-7** Tests MUST use recorded DeFiLlama fixtures in `testdata/`, no network,
  no real key (CONV-23, PD-2); requirement-ID test names (CONV-21); proptest for
  the event serialization (CONV-22); NaN fail-closed (CONV-8); no panic on
  malformed input (CONV-15); no `unwrap` (CONV-13).
- **DEF-8** MUST be idempotent and resumable: re-run skips or byte-identical rewrite
  via per-date completion marker (mirrors HBS-4/7).

## Acceptance criteria (initial)

- [ ] `def_1_keyless_defillama_endpoint` (no key in config/fixtures; PD)
- [ ] `def_2_macropoint_or_snapshot_variant` (round-trips; CONV-22)
- [ ] `def_3_normalization_deterministic` (golden)
- [ ] `def_4_fidelity_regime_not_execution`
- [ ] `def_5_writes_raw_and_cold_macro_append_only` (W-6)
- [ ] `def_7_fixtures_no_network_no_real_key`
- [ ] `def_8_idempotent_resumable_via_completion_marker`

## Decisions

- 2026-08-26 (owner sign-off): New spec (PD-6) from
  `research/external_data_sources_findings.md` revision — DeFiLlama elevated to the
  top pick for its stablecoin-supply-delta + TVL + DEX-volume regime signal.
- 2026-08-26 (owner sign-off): Classified **regime/context**, not per-trade alpha —
  consistent with PD-5. De-weight strategies in worse-off regimes, never a
  standalone entry.
- **2026-08-26 (owner sign-off, decision resolving DEF-2):** **reuse the existing
  `MarketEvent::MacroPoint` body** (it is already `{series_id, value, date}`) and add
  **one appended `Venue::DeFiLlama`** variant (spec 001 amendment). No new
  `MarketEvent` variant, so the codec surface only grows by one venue integer —
  the minimal additive change (W-5). New external host `api.llama.fi` has owner
  sign-off (CLAUDE.md safety table). `series_id` keys (stable, documented below)
  namespace this under the `MacroPoint` scheme.
- **2026-08-26 (owner sign-off):** Default `series_id` set = `DEFI_TVL_AGG`,
  `USDT_SUPPLY`, `USDC_SUPPLY`, `STABLECOIN_MCAP`, `DEX_VOL_1D`. Aggregate-fund
  both USDT+USDC supply and the top-10 DEX volume sum as separate macro series;
  daily cadence.

## Open questions

- None (resolved 2026-08-26). Remaining config knobs (exact endpoint calls / series
  map) are implementation details captured in `defi_llama.toml.example`, not
  decisions.

## Status

**Ready** — decisions and event shape resolved + owner sign-off recorded
(2026-08-26). Implement after the Phase-0 gate and the spec 001 amendment (schema-6
venue append) land. Update `specs/README.md` status on implementation.