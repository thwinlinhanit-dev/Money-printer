# 031 — Deribit Options Market-Data Recorder

## Purpose

Record Deribit's free public options market data (book, trades, vol-surface
inputs) NOW as a compounding private asset — even though options analytics and
trading are deferred (BACKLOG [v2]). Deribit public JSON-RPC needs no auth for
market data. Record-now, analyze-later (same "retention is the moat" logic as
Phase 0).

## Scope

In: a Deribit normalizer (`collectors`) subscribing to public WS channels
(book, trades, ticker) for options, emitting normalized Trade/Book events + a
NEW option-metadata schema addition (spec 001 amendment, owner sign-off),
writing to `data/raw/` + `cold/options/` Parquet. Out: options analytics (vol
surface, GEX, greeks — deferred to a later feature spec), options trading/
strategies (BACKLOG [v2], post-Phase-6), auth/private streams (PD-1), other
options venues.

## Design

```
Deribit public WS (no auth for market data)
   │  book.{instr}, trades.{instr}, ticker.{instr}.{iv}   (instr = "BTC-28JUN26-100000-C")
   ▼
collectors::deribit normalizer ──▶ parse instr name → {underlying, strike, expiry, type}
   │   Trade/Book events + option metadata (NEW OptionLeg / option fields)
   ├──▶ raw/deribit/{date}/{stream}.ndjson.zst   (verbatim pre-parse, COL-9)
   └──▶ cold/options/…/part.parquet   (append-only, W-6; manifest sampled=false)
```

Instrument names encode underlying + strike + expiry + type (e.g.,
`BTC-28JUN26-100000-C`). Option metadata (strike/expiry/type, greeks-at-record
if in the ticker) attaches to events via a NEW schema addition. cold/options/
partition: flat-with-metadata-columns (likely simpler for DuckDB/Polars query)
vs by-expiry/strike — decide at implementation.

## Requirements

- **OPT-1** MUST connect to Deribit public WS (no auth for market data),
  reconnect with jittered backoff (COL-1), emit `Status` events (COL-3),
  staleness watchdog (COL-2).
- **OPT-2** MUST normalize Deribit instrument names → `{underlying, strike,
  expiry, type}` and attach option metadata to events via a NEW schema
  addition (spec 001 amendment — `OptionLeg` metadata or option fields,
  `schema_ver` bump, CONV-20). ⚠️ schema amendment ⇒ owner sign-off (CLAUDE.md).
- **OPT-3** MUST capture raw frames verbatim pre-parse (COL-9) to
  `raw/deribit/{date}/…ndjson.zst` for re-normalization after schema/venue
  drift (pitfall #2).
- **OPT-4** MUST be deterministic in normalization (CONV-9..12); `recv_ts_ns`
  at socket read (COL-5); BTreeMap for instrument order (CONV-10); book sync
  per Deribit's documented algorithm (COL-7).
- **OPT-5** MUST write to `data/raw/` + `cold/options/` Parquet, append-only
  (W-6). Manifest per underlying/date with `sampled: false` (Deribit public
  book/trades are not throttled like Binance liq).
- **OPT-6** Scope is RECORDING ONLY. Vol surface, GEX, greeks analytics, and
  options strategies are OUT (deferred to a later feature spec + BACKLOG [v2]).
  Do not build analytics in this spec (BRAINSTORM pitfall #7: don't build the
  fun part first).
- **OPT-7** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16), checked
  in as `deribit.example.toml` (currencies BTC/ETH, instrument filter); binary
  in collectors supports `--currency`, `--config`, `--check-config`, `--version`
  (CONV-18).
- **OPT-8** Tests MUST use recorded Deribit fixtures in `testdata/`, no network
  (CONV-23); requirement-ID test names (CONV-21); proptest for instrument-name
  parsing + book reconstruction (CONV-22).
- **OPT-9** NaN/inf fail-closed (CONV-8); no panic on malformed venue input
  (CONV-15); no `unwrap` (CONV-13).
- **OPT-10** New external host (Deribit WS) + new event schema ⇒ owner
  sign-off before implementation (CLAUDE.md "add a venue per spec" is allowed,
  but the new schema variant + new egress are flagged for sign-off).

## Acceptance criteria

- [x] `opt_1_connects_reconnects_emits_status` (COL-1/3)
- [x] `opt_2_instrument_name_parses_to_option_metadata` (proptest, CONV-22)
- [x] `opt_3_raw_frames_captured_verbatim` (COL-9)
- [x] `opt_4_normalization_deterministic` (golden)
- [x] `opt_5_writes_raw_and_cold_options_append_only` (W-6; manifest `sampled=false`)
- [x] `opt_6_recording_only_no_analytics` (assert no vol/GEX code in this scope)
- [x] `opt_7_check_config_rejects_unknown_fields`
- [x] `opt_8_fixtures_no_network`

## Decisions

- 2026-08-04: New spec (BACKLOG `vol/options overlay (Deribit) [v2]` +
  brainstorm B7). Record-now, analyze-later — compounds a costly dataset for
  free today (Phase-0 thesis: retention is the moat).
- 2026-08-04: RECORDING ONLY (OPT-6) — analytics (vol surface, GEX, greeks)
  deferred to a later feature spec; options strategies are BACKLOG [v2]
  post-Phase-6. Don't build the fun part first (BRAINSTORM pitfall #7).
- 2026-08-04: Deribit public JSON-RPC, no auth for market data (confirmed). New
  venue (add-venue skill) + new event schema for option metadata ⇒ spec 001
  amendment (CONV-20) ⇒ owner sign-off (CLAUDE.md). Spec written now (PD-6).
- 2026-08-04: `cold/options/` partition layout (flat-with-metadata-columns vs
  by-expiry/strike) — decide at implementation; flat is likely simpler for
  query (DuckDB/Polars).
- 2026-08-05 (owner sign-off): IMPLEMENTED. `wss://www.deribit.com/ws/api/v2`
  public, no auth for market data (OPT-1). Channel parsing per
  `book.{instr}.{group}.{depth}.{interval}` (instr is the second dot-segment);
  instrument names `UNDERLYING-YYMMMDD-STRIKE-C/P` parse to `OptionLeg`
  (OPT-2, proptested). `TeeTransport` captures raw frames verbatim pre-parse
  (OPT-3). Book sync per Deribit's `change_id` continuity + snapshot resync
  (OPT-4/COL-7). Cold path: `cold/options/` flat-with-metadata-columns
  (decided at impl — kind discriminator + nullable columns; book levels as
  JSON text, raw frames stay verbatim), manifest `sampled: false` (OPT-5).
  Recording only — no vol/GEX/greeks analytics (OPT-6).

## Open questions

- Option metadata schema: new `OptionLeg` event variant vs option fields on
  Trade/Book — needs spec-001 amendment + owner sign-off.
- Instrument filter scope: all BTC/ETH options (large) vs near-the-money/
  near-expiry subset (manageable) — owner decision; default a subset to bound
  volume.
- Deribit public channel set for vol-surface reconstruction — confirm at
  implementation which channels suffice (book + trades likely; ticker optional).
