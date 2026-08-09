# 028 — Hyperliquid Whale Position Collector

## Purpose

Record Hyperliquid's public, on-chain, full-fidelity per-user perpetual
positions (entry, size, leverage, liquidation price) as a compounding private
asset — the unique free dataset no paid vendor gives. Real liquidation prices
from real positions are ground truth that calibrates the estimated liq bands
(spec 029). Positions become strategy-consumable features ONLY after event-
study grading (RES-4); copy-trading is explicitly rejected (BACKLOG).

## Scope

In: a collector polling Hyperliquid's public REST (api.hyperliquid.xyz, no auth)
for top positions + watchlist addresses, emitting a NEW `WhalePosition` event
variant (spec 001 amendment, owner sign-off), writing to `data/raw/` + cold
Parquet. Out: copy-trading (rejected), live strategy decisions from raw
positions (PD-4), tier-1 positioning ratios (separate spec), tier-3 on-chain
flows (separate spec), wallet-cohort grading (deferred to features 004 after
event study).

## Design

```
Hyperliquid public REST (api.hyperliquid.xyz, no auth)
   │  poll top-N positions (60s) + watchlist addrs (30s)
   ▼
collectors::hyperliquid normalizer ──▶ WhalePosition { venue, address (opaque 0x),
   symbol, size, entry, leverage, liq_price, exch_ts_ns, recv_ts_ns }
   ├──▶ data/raw/{date}_hyperliquid_positions.log   (append-only, W-6)
   └──▶ cold/positions/venue=hyperliquid/…/date=…/part.parquet  (manifest sampled=false: census)
```

Addresses recorded as opaque 0x identifiers (no external labels/PII). Cohort
grading (score wallets by realized PnL from our recorded history) is a later
features (004) step, after the RES-4 event study.

## Requirements

- **WHL-1** MUST poll Hyperliquid public REST (api.hyperliquid.xyz), no auth,
  no key (PD-2), reusing collectors `live-http` + `hyperliquid.rs`; rate-limited
  per Hyperliquid docs.
- **WHL-2** MUST emit a NEW `WhalePosition` event variant (spec 001 amendment,
  `schema_ver` bump, CONV-20). ⚠️ schema amendment ⇒ owner sign-off before
  implementation (CLAUDE.md; 004 Decisions).
- **WHL-3** MUST record addresses as opaque 0x identifiers; MUST NOT import
  external labels/PII. Cohort grading happens later in features (004) from our
  own recorded history.
- **WHL-4** MUST be deterministic in normalization (CONV-9..12); `recv_ts_ns`
  at socket read (COL-5); BTreeMap for address/symbol order (CONV-10).
- **WHL-5** Positions enter as DATA ONLY. They become strategy-consumable
  features ONLY after event-study grading (RES-4) in the feature engine (004).
  MUST NOT feed strategies directly (PD-4). Copy-trading is explicitly rejected
  (BACKLOG rejected list).
- **WHL-6** MUST write to `data/raw/` (daily logs) + `cold/positions/` Parquet
  (separate stream from trades), append-only (W-6). Manifest `sampled: false`
  (Hyperliquid positions are a census, unlike Binance throttled liq, COL-8).
- **WHL-7** Poll cadence config (default top-N 60s, watchlist 30s); a missed
  poll window emits `Status::GapDetected` (COL-6/INT).
- **WHL-8** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16), checked
  in as `whale_positions.toml.example`; binary in collectors supports
  `--symbols`, `--watchlist`, `--config`, `--check-config`, `--version`
  (CONV-18).
- **WHL-9** Tests MUST use recorded Hyperliquid REST fixtures in `testdata/`,
  no network (CONV-23); requirement-ID test names (CONV-21); proptest for
  `WhalePosition` serialization (CONV-22); NaN fail-closed (CONV-8); no panic
  on malformed input (CONV-15); no `unwrap` (CONV-13).

## Acceptance criteria

- [x] `whl_1_polls_public_rest_no_auth`
- [x] `whl_2_whaleposition_event_variant_roundtrips` (CONV-22 proptest)
- [x] `whl_3_addresses_are_opaque_no_external_labels`
- [x] `whl_4_normalization_is_deterministic` (golden)
- [x] `whl_5_positions_are_data_only_not_strategy_input` (assert no strategies-crate dep; PD-4)
- [x] `whl_6_writes_raw_and_cold_positions_append_only` (W-6; manifest `sampled=false`)
- [x] `whl_7_gap_detection_on_missed_poll`
- [x] `whl_8_check_config_rejects_unknown_fields`
- [x] `whl_9_fixtures_no_network_and_nan_fail_closed` (CONV-8/15/23)

## Decisions

- 2026-08-04: New spec (BACKLOG tier-2 "Hyperliquid whale position tracking").
  Hyperliquid API confirmed public, no-auth, with historical data + community
  SDKs + CCXT. Positions are fully on-chain ⇒ census, not sample (contrast
  Binance throttled liq, COL-8).
- 2026-08-04: Unique value = real liq prices from real positions ⇒ ground truth
  to calibrate spec 029 estimated liq bands. Cross-link 029.
- 2026-08-04: NOT copy trading (WHL-5, BACKLOG rejected). Whale data enters only
  as graded features (RES-4). Non-negotiable.
- 2026-08-04: Adds `WhalePosition` event variant ⇒ spec 001 amendment (CONV-20)
  ⇒ owner sign-off before implementation (CLAUDE.md). Spec written now (PD-6).
  Lives in collectors (reuses `hyperliquid.rs` + `live-http`).
- 2026-08-05 (owner sign-off): IMPLEMENTED. `clearinghouseState` per user
  confirmed at impl; leaderboard `type=leaderboard` + `timeWindow` returns
  ranked `{name, address, pnl}` — addresses only are kept (WHL-3). Poller
  wraps responses with the request address so the normalizer stays frame-based
  and deterministic (WHL-4). Cold path: `cold/positions/` via a dedicated
  Parquet writer, manifest `sampled: false`, prune-verified (WHL-6). NaN
  liq_price sentinel round-trips through Parquet (whl_6/whl_9). Compaction
  note: `mp-whale` writes `data/raw/{date}_hyperliquid_positions.log` (one log
  holding all coins), so the operator compacts it with
  `mp-ops compact --venue hyperliquid --symbol positions --date …` —
  `compact_day` groups the multi-coin content by symbol internally. Same
  pattern for 030 (`--venue fred --symbol macro`) and 031 (options events go
  through the deribit collector's own log).
- 2026-08-05 (impl): RES-4 study wired to the recorded positions — the
  features crate's `WhaleBandStudy` (spec 029 LIQ-6) replays the `mp-whale`
  positions log merged with the hyperliquid market log (mark/OI) and grades
  `liq.est_bands` against the real liq prices: `whale_study --log
  <hyperliquid.log> --log <positions.log>`. The binary remaps the two
  independent symbol-id spaces onto one canonical space (each collector run
  interns its own ids, EVT-8) before merging in recv order (EVT-5). Real liq
  prices now flow into  `band_accuracy` as required by WHL-5 (data → graded
  feature → strategy), closing the 029 cross-link. See spec 029 Decisions for
  the metric definition (sign-aware pairing, per-side coverage direction).
- 2026-08-05 (impl): study runs journal SIM-10 tracker records — `whale_study
  --run-id <ulid> --runs-dir <dir> [--git-sha <sha>]` appends one JSONL record
  per run to `runs/index.jsonl` (RES-4 "tracker-style run records", spec 010;
  same contract as the sim tracker, spec 005). Each RES-4 grading run is now
  reproducible evidence, not a terminal print.
- 2026-08-05 (impl): aggregate whale net positioning + deltas implemented in
  the feature engine (004) after the RES-4 gate (WHL-5): `whale.net.{venue}` /
  `whale.delta.{venue}` — per-symbol Σ signed position notional (size × entry)
  across recorded addresses, last-poll-per-address upsert, BTreeMap order
  (CONV-10), NaN size/entry fail-closed (CONV-8); `[whale_net]` venues +
  stale_after_ns params in features.toml (FEA-7). Eviction note: the
  `clearinghouseState` census omits closed positions (no tombstone event) and
  top-N leaderboard rotation stops polling an address, so the feature evicts
  any position not refreshed within `stale_after_ns` (default 10 min) —
  `whale.net` is the current census, never a graveyard of dead positions.
  Remaining from the tier-2 BACKLOG item: wallet-cohort grading.
- 2026-08-08 (judgment call, W-5 — found live): the leaderboard API moved. The
  original `POST /info {"type":"leaderboard","timeWindow":"7d"}` now returns
  HTTP 422 (type removed) and `GET /POST /leaderboard` return 404 — the whale
  had silently recorded only GapDetected statuses all day (the watchdog's
  data-flow check passed because status events keep the log growing). The
  replacement is the public stats-data bucket
  `GET https://stats-data.hyperliquid.xyz/Mainnet/leaderboard` (~34 MB,
  refreshed ~hourly, no auth) with `leaderboardRows[].ethAddress` +
  `windowPerformances` (per-window pnl/roi/vlm; keys day/week/month/allTime).
  `leaderboard_addresses` now ranks by the configured window's PnL
  ("1d"→day, "7d"→week, "30d"→month, else allTime). Because the payload is
  large and refreshes ~hourly, the whale caches the list and re-fetches it on
  a new `leaderboard_refresh_s` (default 3600); the 60s `top_poll_interval_s`
  re-polls the cached addresses' `clearinghouseState`. `clearinghouseState`
  (the per-address fetch) is unchanged. Note: WHL-1's "api.hyperliquid.xyz"
  wording now reads as "the public no-auth REST API" — the leaderboard host
  is stats-data.hyperliquid.xyz, the per-address fetch stays on
  api.hyperliquid.xyz.


## Open questions

- Exact Hyperliquid endpoint(s) for top-N positions + historical changes —
  verify at implementation (API drifts, pitfall #2). RESOLVED 2026-08-08: the
  top-N path is the public stats-data leaderboard GET (see Decisions), the
  per-user path is `info` `clearinghouseState` (stable).
- `WhalePosition` schema fields + spec-001 amendment text — owner sign-off.
- Poll cadence vs Hyperliquid rate limits — calibrate at implementation.
