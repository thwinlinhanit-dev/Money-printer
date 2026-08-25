# 034 — Exchange Netflow Indexer (Ethereum Reserve Balances)

## Purpose

Record Ethereum exchange-reserve wallet balances (USDT primary; ETH/USDC
extensible) as a private dataset for the on-chain flow edge: exchange
netflows are a documented 1–2h-ahead return signal (SSRN 4630115,
arXiv 2411.06327) and the only major venue-agnostic flow source this system
does not yet record. The collector stores raw balance SNAPSHOTS — flows are
derived research-side from deltas (NFL-5), keeping the recorded artifact
honest and replay-safe.

## Scope

In: `Venue::Ethereum` + new `NetflowSnapshot` event variant (spec 001
amendment, schema 3→4, owner sign-off), a REST poller (`mp-netflow`) for
Etherscan's free public API (rate-limit-compliant), daily raw logs +
cold Parquet. Out: derived netflow/velocity features (after event-study
grading, RES-4), exchange attribution beyond a watchlist (free tier limit),
stablecoin-supply tracking (not a wallet balance), any strategy input before
grading (PD-4).

## Design

```
Etherscan free REST (no key in config; MP_ETHERSCAN_KEY env only, PD-2)
   │  poll watchlist addresses: action=balance (ETH) or tokenbalance (ERC-20)
   ▼
EtherscanNormalizer (wrapped payload {address, asset, result})
   ──▶ NetflowSnapshot { address (opaque 0x), balance (RAW base units) }
   ├──▶ data/raw/{date}_netflow_ethereum.log   (append-only, W-6)
   └──▶ cold/netflow/… (census manifest)
```

- Symbol = asset (`"USDT"`); envelope venue = `Ethereum`; `exch_ts_ns = 0`
  (CONV-4: Etherscan reports no per-observation timestamp; the envelope's
  `recv_ts_ns` is the observation time).
- Balances stored in RAW base units (USDT: 6 decimals) — no decimal scaling
  in the collector; research scales. USDT ERC-20 contract
  `0xdAC17F958D2ee523a2206206994597C13D831ec7` documented in the example
  config.
- A failed poll emits `Status::GapDetected` (COL-6/INT) so the watchdog's
  data-flow check can never be fooled by a silent collector.

## Requirements

- **NFL-1** MUST add `Venue::Ethereum` (appended) and `NetflowSnapshot {
  address: String, balance: f64 }` as an APPENDED `MarketEvent` variant
  (spec 001 amendment, `schema_ver` 3→4, CONV-20; owner sign-off).
- **NFL-2** MUST poll Etherscan's free public REST API for each watchlist
  address/asset pair; `action=balance` for native ETH, `action=tokenbalance`
  (with contract) for ERC-20 tokens.
- **NFL-3** MUST take the API key from the `MP_ETHERSCAN_KEY` environment
  variable ONLY (PD-2 — never config); a missing key MUST fail at startup.
  Watchlist entries are `{label, address, asset, contract?}` in TOML
  `serde(deny_unknown_fields)` (CONV-16), checked in as
  `netflow.toml.example`; labels are for logs/ops only, NEVER recorded in
  events (addresses stay opaque, WHL-3 stance).
- **NFL-4** MUST record the response balance as-is; empty / non-numeric /
  absent `result` MUST be skipped honestly (no invented values, Major #7),
  and a failed poll MUST emit `Status::GapDetected`.
- **NFL-5** MUST record snapshots only — derived netflows (deltas over time)
  are computed in research, never synthesized in the collector.
- **NFL-6** MUST treat recorded balances as DATA ONLY; they become
  strategy-consumable features only after event-study grading (RES-4, PD-4).
- **NFL-7** MUST respect free-tier rate limits (poll cadence config,
  `min_poll_gap_ms` floor, default 300s cadence); a missed poll window emits
  `Status::GapDetected` (COL-6/INT).
- **NFL-8** MUST be covered by: fixture tests with recorded Etherscan
  payloads (incl. empty/garbage `result`), no network (CONV-23); proptest
  round-trip (CONV-22); golden-bytes pin of the new variant (schema-4 table);
  no `unwrap` (CONV-13), NaN fail-closed (CONV-8), deterministic
  normalization (CONV-9..12).

## Acceptance criteria

- [x] `nfl_1_wrapped_balance_emits_snapshot` (etherscan normalizer parses wrapped `{address, asset, result}` frames — inline tests in `collectors/src/etherscan.rs`)
- [x] `nfl_2_balance_vs_tokenbalance_route` (mp-netflow fetch branch by contract presence — `collectors/src/netflow.rs`)
- [x] `nfl_3_empty_or_garbage_result_is_skipped` + `nfl_4_missing_wrap_fields_is_a_parse_error` (inline tests; empty/non-numeric/non-finite `result` ⇒ skipped, missing wrap fields ⇒ Err)
- [x] `nfl_5_snapshots_only_no_synthesized_flows` (venue_fixtures: every payload ⇒ exactly one snapshot)
- [x] `nfl_6_netflow_is_data_only_not_a_trade_input` (event_schema: no trade view ⇒ never a fill/feature input)
- [x] `nfl_7_*` cadence config tests (`netflow.rs`: example parses with 300s/250ms defaults, zero cadence + unknown fields rejected fail-closed)
- [x] `nfl_8_schema4_golden_pins_netflow_snapshot` (BDC-1/2 table restamped to schema 4 + proptest round-trip)
- [x] schema-4 golden restamp incl. `netflow_snapshot` vector (bdc_1/bdc_2/bdc_8)
- [x] BDC-3 suite green on the swap (full `cargo test --workspace`, 2026-08-18)

## Decisions

- 2026-08-24: Etherscan API **V1 → V2 migration** (venue-side deprecation).
  The V1 route (`https://api.etherscan.io/api`) now answers `status=0`
  "deprecated V1 endpoint" for every request, which surfaced as silent
  per-wallet fetch failures (tracing warns without a subscriber are dropped).
  The poller now targets `https://api.etherscan.io/v2/api` with `chainid=1`
  (Ethereum mainnet); query params unchanged. Pinned by
  `nfl_url_uses_v2_route_with_mainnet_chainid`. First live data: 2026-08-24,
  5 exchange USDT wallets @ 300s cadence.
- 2026-08-18 (owner sign-off via "implement all"): schema 3→4 amendment for
  specs 033/034 — append-only additions (CONV-20); schema-3 frames stay
  readable via the log reader's schema-3 arm.
- 2026-08-18: Etherscan over Alchemy/Infura — free tier is sufficient for a
  small watchlist at 300s cadence (~5 calls/s ceiling, ~1/4 that used),
  no account friction beyond the key, and the API is stable/public.
- 2026-08-18: snapshots not flows (NFL-5): a delta implies two observations
  and a cadence assumption; the snapshot is the honest primitive and lets
  research choose windows freely (same stance as spec 028's census).
- 2026-08-18: raw base units (NFL-4): decimal scaling is a research
  decision; storing the wire value kills a class of off-by-10^n bugs.
  `exch_ts_ns = 0` (CONV-4) is accepted — Etherscan has no per-observation
  timestamp.
- 2026-08-18: watchlist start: major exchange USDT reserve wallets (Binance,
  Coinbase, OKX, Bybit, Kraken) + ETH equivalents where published; the
  watchlist ships empty in `netflow.toml.example` and is filled by the
  operator (config, not code — PD-2 keeps keys and wallet lists out of the
  repo).

## Open questions

- None.