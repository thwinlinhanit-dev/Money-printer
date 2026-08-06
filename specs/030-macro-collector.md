# 030 — Macro Data Collector (Hyperliquid HIP-3 + FRED)

## Purpose

Feed the market-correlation view (BRAINSTORM §6; roadmap Phase 4 lead-lag)
with free macro data: (a) Hyperliquid HIP-3 TradFi pairs (Crude Oil, SP500,
XYZ100) — on-chain, real-time, via the existing Hyperliquid collector (zero
new infra); (b) FRED (St. Louis Fed) daily series (rates, DXY) — free,
authoritative, via a new REST poller. Both labeled "correlation-grade, not
execution-grade" — CME licensed data is cost-prohibitive for a free tool
(BRAINSTORM).

## Scope

In: (a) recording HIP-3 TradFi symbols through the existing
`collectors::hyperliquid` normalizer (config + `asset_class:
tradfi_synthetic` metadata); (b) a FRED REST collector emitting a NEW
`MacroPoint` event variant (spec 001 amendment, owner sign-off) for daily
economic series. Out: licensed CME/TradFi tick data (cost-prohibitive), live
trading of macro (PD-1), options on macro (spec 031), FRED series beyond a
configured set.

## Design

```
Hyperliquid HIP-3 pairs (Crude Oil, SP500, XYZ100, …) ──existing WS──▶ collectors::hyperliquid
   (just more symbols; asset_class=tradfi_synthetic metadata)            │
                                                                           ▼
                                                       data/raw/ + cold/trades/ ( HIP-3 )

FRED public API (api.stlouisfed.org, FRED_API_KEY from env) ──REST daily──▶ collectors::fred
   │  MacroPoint { series_id, value, date, exch_ts_ns, recv_ts_ns }       │
   ▼                                                                       ▼
                                            data/raw/ + cold/macro/  (manifest source=fred, fidelity=daily)
```

FRED API key from env `FRED_API_KEY` (CONV-17, PD-2 — never in repo/config/
`.example`). HIP-3 needs no key (on-chain, public).

## Requirements

- **MAC-1** HIP-3 TradFi symbols MUST be recorded via the existing
  `collectors::hyperliquid` normalizer (no new code path) with `asset_class:
  tradfi_synthetic` metadata. Zero new network dependency for this part.
- **MAC-2** FRED collector MUST poll the public FRED API with `FRED_API_KEY`
  from env (CONV-17, PD-2 — key never in repo/config/`.example`). New external
  host ⇒ owner sign-off before implementation (CLAUDE.md).
- **MAC-3** FRED MUST emit a NEW `MacroPoint` event variant (spec 001
  amendment, `schema_ver` bump, CONV-20). ⚠️ schema amendment ⇒ owner sign-off
  (CLAUDE.md; 004 Decisions pattern).
- **MAC-4** MUST be deterministic in normalization (CONV-9..12); BTreeMap for
  `series_id` order (CONV-10); `recv_ts_ns` at socket read (COL-5). FRED daily
  cadence (configurable, default 1/day + backfill).
- **MAC-5** MUST label fidelity: FRED `daily` (authoritative, gov source,
  lagged), HIP-3 `realtime_synthetic` (on-chain exposure, not CME). Both
  labeled "correlation-grade, not execution-grade" — MUST NOT be used as
  execution data for TradFi futures (no CME license).
- **MAC-6** MUST write to `data/raw/` + `cold/macro/` Parquet, append-only
  (W-6). Manifest per series with `source`, `fidelity`, `sampled: false`.
- **MAC-7** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16), checked
  in as `macro.toml.example` (FRED `series_id` list, NO key); binary in
  collectors supports `--series`, `--hip3-symbols`, `--config`, `--check-config`,
  `--version` (CONV-18).
- **MAC-8** Tests MUST use recorded FRED/HIP-3 fixtures in `testdata/`, no
  network, no real API key (CONV-23, PD-2); requirement-ID test names
  (CONV-21); proptest for `MacroPoint` serialization (CONV-22); NaN fail-closed
  (CONV-8); no panic on malformed input (CONV-15); no `unwrap` (CONV-13).

## Acceptance criteria

- [x] `mac_1_hip3_via_existing_hyperliquid_normalizer` (`asset_class` metadata)
- [x] `mac_2_fred_key_from_env_not_repo` (PD-2: assert no key in config/example/fixtures)
- [x] `mac_3_macropoint_event_variant_roundtrips` (CONV-22 proptest)
- [x] `mac_4_normalization_deterministic` (golden)
- [x] `mac_5_fidelity_labeled_correlation_not_execution`
- [x] `mac_6_writes_raw_and_cold_macro_append_only` (W-6)
- [x] `mac_7_check_config_rejects_unknown_fields`
- [x] `mac_8_fixtures_no_network_no_real_key`

## Decisions

- 2026-08-04: New spec (brainstorm B6; BRAINSTORM §6 correlation; roadmap
  Phase 4). Two sources: HIP-3 (zero new infra) + FRED (new REST poller).
- 2026-08-04: HIP-3 is the lowest-effort macro path — already a venue we
  collect (Hyperliquid), flows through the existing normalizer (MAC-1).
  BRAINSTORM already notes this.
- 2026-08-04: FRED is authoritative + free (rates, DXY — the macro driver
  behind crypto risk-on/off). API key free, from env (MAC-2, PD-2/CONV-17).
  Stooq/Yahoo deferred (FRED is cleaner).
- 2026-08-04: Correlation-grade only (MAC-5) — CME licensed data is
  cost-prohibitive for a free tool (BRAINSTORM). Do NOT use as execution data
  for TradFi futures. Adds `MacroPoint` event variant ⇒ spec 001 amendment
  (CONV-20) + new FRED egress host ⇒ owner sign-off before implementation
  (CLAUDE.md). Spec written now (PD-6).
- 2026-08-05 (owner sign-off): IMPLEMENTED. Default series set
  `{DTWEXBGS (DXY), DGS10, DGS2, SOFR, DFF}`; FRED poller reads
  `FRED_API_KEY` from env only (MAC-2) and wraps responses with the series_id
  so the normalizer stays frame-based + deterministic (MAC-4). Per-series
  observation watermark drives daily cadence + backfill (mp-macro). HIP-3 via
  `mp-collector --hip3-symbols` with `InstrumentKind::TradFiSynthetic`
  metadata (MAC-1). Cold path: `cold/macro/` via a dedicated Parquet writer,
  manifest `sampled: false` (MAC-6).

## Open questions

- `MacroPoint` event variant schema + spec-001 amendment — owner sign-off.
- Which FRED `series_id`s are in scope (DXY, 10y, 2y, SOFR, …) — owner pick;
  default a small set.
- HIP-3 symbol list + asset IDs — verify at implementation (Hyperliquid asset
  IDs drift, pitfall #2).
