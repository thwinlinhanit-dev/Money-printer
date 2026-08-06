# 027 — Historical Bootstrap from Binance Public Archive

## Purpose

Bootstrap a research-grade historical dataset TODAY from Binance's free public
archive (data.binance.vision / `s3://data.binance.vision`) so Phase 2/3
research and sim can proceed without waiting for the Phase-0 7-clean-day gate.
Written to a SEPARATE `cold/historical/` namespace — never touching live
`data/raw/` or `cold/trades/` (W-6) — with provenance `source:
external_archive` and fidelity `aggregated` (Binance aggTrades bucket
same-price/time trades ⇒ backtests are upper-bound on maker fills, SIM-2
trade-print rule).

## Scope

In: a batch ingester that downloads Binance historical aggTrades ZIP/CSV from
data.binance.vision (no auth, no key — PD-2), parses to canonical Trade events
(field mapping mirroring `collectors::binance` REST aggTrades, COL-25..27), and
writes zstd Parquet to `cold/historical/trades/venue=binance/symbol=…/date=…/`
matching the cold/trades schema (STO-1) + a historical manifest. Out: modifying
live raw/cold (W-6), the Dataset reader (additive spec-003 amendment — Open
question), live decision paths, klines/depth (deferred), non-Binance archives.

## Design

```
data.binance.vision (public, no auth) ──HTTP──▶ mp-bootstrap (storage, live-http feature)
                                                  │  parse aggTrades CSV → canonical Trade
                                                  ▼
                  cold/historical/trades/venue=binance/symbol=…/date=…/part.parquet
                  + cold/historical/manifests/  (source=external_archive, fidelity=aggregated)
```

aggTrades CSV columns `price, qty, quoteQty, time_ms, isBuyerMaker` → Trade
`{exch_ts_ns = time_ms·1e6, price, qty, aggressor = if isBuyerMaker {Sell} else {Buy}}`.
Parquet schema + layout match `cold/trades` (STO-1) so `/research` (Polars/
DuckDB, CONV-2) reads it directly; the Dataset reader (STO-4) needs an additive
amendment to consume it (Open question).

## Requirements

- **HBS-1** MUST download from data.binance.vision (or `s3://data.binance.vision`),
  no auth, no key (PD-2). HTTP behind the storage `live-http` feature (matching
  collectors' convention). New external egress — owner-approved 2026-08-05
  (CLAUDE.md safety table).
- **HBS-2** MUST write ONLY to `cold/historical/…`; MUST NEVER write to
  `data/raw/` or `cold/trades/` (W-6). Separate namespace = separate audit
  boundary; the live recorder's value is its own timestamps, which the archive
  lacks.
- **HBS-3** MUST label every file/manifest `source: external_archive` +
  `fidelity: aggregated` + `schema_ver` (CONV-20) so no consumer mistakes it
  for the live recorder's tick data.
- **HBS-4** MUST be idempotent: re-run skips or byte-identical rewrite via
  content-hash check (mirroring STO-1).
- **HBS-5** MUST be deterministic: same archive + config ⇒ byte-identical
  Parquet (BTreeMap/sorted, CONV-10; golden hash, CONV-12).
- **HBS-6** MUST parse aggTrades CSV → canonical Trade events with the field
  mapping above (mirrors `collectors::binance` REST aggTrades, COL-25..27). The
  parser is a small pure module in storage (no collectors dependency, to keep
  the dep graph clean, CONV-3).
- **HBS-7** MUST honor the Binance archive structure (monthly ZIPs of daily
  CSVs) and a config date range, resumable via a per-date completion marker.
- **HBS-8** MUST rate-limit downloads (default 1 req/s, configurable) and retry
  with backoff; errors via `thiserror`, no panic (CONV-13/15).
- **HBS-9** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16), checked
  in as `historical_bootstrap.toml.example`; binary `mp-bootstrap` in storage
  supports `--symbol`, `--start-date`, `--end-date`, `--config`, `--check-config`,
  `--version` (CONV-18).
- **HBS-10** Tests MUST use a sanitized aggTrades CSV fixture in `testdata/`,
  no network (CONV-23); requirement-ID test names (CONV-21); NaN fail-closed
  (CONV-8); no `unwrap` (CONV-13).

## Acceptance criteria

- [x] `hbs_1_downloads_to_separate_namespace`
- [x] `hbs_2_never_writes_live_raw_or_cold` (W-6: assert `data/raw/` + `cold/trades/` unchanged)
- [x] `hbs_3_provenance_and_fidelity_labeled`
- [x] `hbs_4_idempotent_rerun`
- [x] `hbs_5_deterministic_output` (golden hash)
- [x] `hbs_6_aggtrades_csv_parses_to_canonical_trades` (mirrors binance REST mapping)
- [x] `hbs_7_resumable_across_date_range`
- [x] `hbs_8_rate_limit_and_backoff` (mock-server fixture)
- [x] `hbs_9_check_config_rejects_unknown_fields`

## Decisions

- 2026-08-04: New spec. Unblocks Phase 2/3 research today instead of waiting 7
  clean days. data.binance.vision confirmed free, no-auth, public.
- 2026-08-04: Separate `cold/historical/` namespace (HBS-2) honors W-6 +
  `stream-gap.md` ("the gap is a fact"); never mix archive data with the live
  recorder's timestamped ticks.
- 2026-08-04: Fidelity `aggregated` (HBS-3) — aggTrades are bucketed; backtests
  on them are upper-bound on maker fills (SIM-2). Research must see this.
- 2026-08-04: Binary in storage, `live-http` feature-gated; CSV parser is a
  small pure storage module (mirrors `collectors::binance` mapping, no
  cross-crate dep, CONV-3). New egress host ⇒ owner sign-off before
  implementation (CLAUDE.md).
- 2026-08-04 (impl, CORE ONLY): transport-agnostic core implemented —
  `storage/src/historical.rs` (`HistoricalSource` + `MockHistoricalSource` +
  `FileHistoricalSource`, `parse_aggtrades`, `bootstrap_day`),
  `storage/src/bin/mp-bootstrap.rs`, `storage/tests/historical.rs`
  (hbs_2..hbs_7 + hbs_9 pass; clippy/fmt clean; full mp-storage suite green).
  The LIVE Binance-archive auto-download (HBS-1) and its rate-limit/backoff
  (HBS-8) were GATED on owner sign-off (network dependency, CLAUDE.md) and are
  now APPROVED + implemented (2026-08-05 entry below). Until then the binary
  read a manually-downloaded local aggTrades CSV (`--input`) via
  `FileHistoricalSource` (no network). Judgment calls (W-5): the daily
  aggTrades CSV is parsed positionally
  `[price, qty, quote_qty, time_ms, is_buyer_maker, is_best_match]`
  (data.binance.vision format); `is_buyer_maker=true ⇒ Sell` aggressor (mirrors
  `collectors::binance` REST `m`, COL-25..27); the row index doubles as
  `stream_seq` and `trade_id` (the CSV omits the agg id); historical
  `recv_ts_ns == exch_ts_ns` (the archive has no local receive clock — honest
  replay). Outputs land in a SEPARATE `cold/historical/` namespace (HBS-2/W-6),
  labeled `source: external_archive`, `fidelity: aggregated` (HBS-3).
- 2026-08-05 (owner approval + HBS-1/HBS-8 implemented): OWNER APPROVED the
  `live-http` network dependency (data.binance.vision — no auth, no key, PD-2;
  the CLAUDE.md safety-table egress sign-off). HBS-1 + HBS-8 are now
  implemented and 027 is `implemented`. What landed:
  `storage/Cargo.toml[features].live-http = ["dep:reqwest", "dep:zip"]` (gated —
  the offline core still builds with no network stack, HBS-2..7/HBS-9/HBS-10
  unchanged); `storage/src/historical_download.rs` (`BinanceVisionSource`
  fetches the archive's daily aggTrades ZIP at
  `{base}/data/{class}/daily/aggTrades/{sym}/{sym}-aggTrades-{date}.zip`,
  `class = "futures/um"` by default to match `Venue::BinanceFutures`;
  `Throttle` = deterministic token-slot rate limiter over injected time,
  default 1 req/s, configurable (HBS-8); `RetryPolicy` = deterministic
  full-jitter exponential backoff, default 3 retries over [1 s, 30 s] — retries
  are ONLY for transient faults, a 404 fails immediately and never fabricates a
  day; `unzip_single_csv` fail-closes if a ZIP has ≠ 1 CSV, CONV-8/15);
  `HistoricalConfig` gains `download_rate_per_sec | download_max_retries |
  download_backoff_base_ms | download_backoff_cap_ms | download_base_url |
  download_data_class` (HBS-9; inert for the offline path, same TOML valid in
  both build modes); `day_complete()` — the HBS-7 per-date completion marker
  (parquet + manifest both present); `mp-bootstrap` learns `--date` single OR
  `--start-date/--end-date` inclusive ranges with a no-chrono civil-calendar
  forward iterator and a completion skip BEFORE any fetch (resumable,
  zero-network re-run); `--version` reports `mp-bootstrap` via the shared
  `app_version` (it used to borrow `mp-cross-venue`'s string). Tests: the
  `live_http` module in `storage/tests/historical.rs` (`hbs_1_downloads_to_
  separate_namespace`, `hbs_8_retries_transient_http_with_backoff`,
  `hbs_8_not_found_is_not_retried`, `hbs_7_completion_marker_tracks_completed_
  days`) plus `Throttle`/`RetryPolicy` unit tests — all offline against a LOCAL
  mock HTTP server + in-memory ZIP fixture (CONV-23). Run the gated suite with
  `cargo test -p mp-storage --features live-http`; the plain
  `cargo test -p mp-storage` offline suite stays green. Judgment calls (W-5):
  we use the archive's *daily* aggTrades ZIP per (symbol, date) rather than
  sweeping monthly ZIPs — each daily ZIP holds exactly one CSV, giving exact
  per-date completion markers, bounded downloads, and true resumability
  (HBS-7's "monthly ZIPs of daily CSVs" is honored as the archive's
  daily/monthly structure); `recv_ts_ns == exch_ts_ns` stays (no local receive
  clock on the archive — honest replay); the wall clock is read ONLY at the
  HTTP boundary (`now_ns`), everything else (URL, throttle, backoff) stays pure
  (HBS-5, CONV-10/11).

## Open questions

- Dataset reader (STO-4) consuming `cold/historical/` needs an additive
  spec-003 amendment (multi-root/namespace). Until then, `/research` (Python,
  not a decision path, CONV-2) reads the Parquet directly.
- klines/depth-snapshot bootstrap (deferred) — depth snapshots are periodic
  (lower fidelity); separate decision.
- data.binance.vision reachability from the recorder host (Binance geo-blocks
  US IPs for live; the archive may differ) — verify at implementation.
