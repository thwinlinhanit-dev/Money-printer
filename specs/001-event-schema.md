# 001 — Event Schema (core types)

## Purpose
The single normalized vocabulary every component speaks. Collectors produce
it, storage persists it, features/strategies/sim consume it. Get this right
and every venue looks the same downstream.

## Scope
In: market events, envelope, symbol metadata, event log format, ring buffer
API. Out: order/execution events (spec 007), feature events (spec 004).

## Design

### Envelope
Every event is wrapped in:

| field | type | notes |
|---|---|---|
| `schema_ver` | u16 | starts at 1 (CONV-20) |
| `venue` | enum `Venue` | `BinanceFutures, Bybit, Okx, Hyperliquid, Coinbase, KrakenFutures, Deribit, Fred` |
| `symbol` | `SymbolId` (u32) | interned; string form in symbol table only |
| `exch_ts_ns` | i64 | exchange-reported time (0 if venue omits) |
| `recv_ts_ns` | i64 | local receive time, from `WallClock` at socket read |
| `stream_seq` | u64 | venue sequence if provided, else collector-assigned monotonic |
| `body` | `MarketEvent` | one of the variants below |

### MarketEvent variants

```
Trade        { price: f64, qty: f64, side: Side /*aggressor*/, trade_id: u64 }
BookDelta    { bids: Vec<(f64 /*price*/, f64 /*new_qty; 0=remove*/)>,
               asks: Vec<(f64, f64)>, first_seq: u64, last_seq: u64 }
BookSnapshot { bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>, seq: u64,
               depth: u16, reason: SnapshotReason /*Init|GapResync|Periodic*/ }
Funding      { rate: f64, interval_s: u32, next_funding_ts_ns: i64 }
MarkPrice    { mark: f64, index: f64 /*NaN if absent*/ }
OpenInterest { oi_contracts: f64, oi_notional: f64 /*NaN if absent*/ }
Liquidation  { price: f64, qty: f64, side: Side /*side being liquidated*/ }
IndexPrice   { index: f64 }
Status       { kind: StatusKind /*Connected|Disconnected|GapDetected|
               Throttled|VenueHalt|Stale|BackpressureDrop{dropped}*/, detail: SmallString }

— schema 2→3 additions (spec 028/030/031, append-only — see Decisions) —
WhalePosition { address: String /*opaque 0x id*/, size: f64 /*signed*/, entry: f64,
                leverage: f64, liq_price: f64 /*NaN if null*/ }
MacroPoint    { series_id: String, value: f64, date: i64 /*UTC midnight ns*/ }
OptionTrade   { leg: OptionLeg, price: f64, qty: f64, side: Side, trade_id: u64 }
OptionBook    { leg: OptionLeg, bids: Levels, asks: Levels, change_id: u64,
                is_snapshot: bool }
OptionTicker  { leg: OptionLeg, mark_iv: f64, mark_price: f64, underlying_price: f64,
                open_interest: f64, greeks: Option<OptionGreeks> }

OptionLeg = { underlying: String, strike: f64, expiry_ts_ns: i64, kind: Call|Put }
OptionGreeks = { delta, gamma, theta, vega: f64 }
```

`Side = Buy | Sell`. Status events flow through the same pipe: gaps and
disconnects are data (they cluster with volatility) and sim needs them.

### Symbol metadata table (`core::SymbolMeta`)
`symbol_id, venue, venue_symbol (string), base, quote, kind (Spot|Perp|Future),
tick_size, step_size, min_notional, contract_multiplier, listed_ts_ns,
delisted_ts_ns (i64::MAX if active)`. Loaded from a checked-in
`symbols.toml` + refreshed by collectors (STO-9 governs persistence).

### Event log (the durable stream)
Append-only length-prefixed binary records:
`[u32 len][u16 schema_ver][bincode(EventEnvelope)]`, one file per
`venue/date`, rotated at UTC midnight, fsync'd every N ms (config).
This raw log is the replay input for sim and the daily determinism check.

### Ring buffer (the hot path)
`core::Ring<T>`: single-producer multi-consumer, fixed capacity (power of 2),
lock-free; consumers hold cursors; slow consumers detect overrun via
generation counters and MUST treat overrun as a gap (emit Status::GapDetected
downstream), never block the producer.

## Requirements
- **EVT-1** `core` crate MUST define envelope, variants, `Venue`, `Side`,
  `SymbolMeta` exactly as above; field names are law (no synonyms).
- **EVT-2** All events MUST be `Copy`-cheap or arena/smallvec-backed; no heap
  allocation per `Trade` on the hot path (book deltas may allocate; SmallVec ≤ 8 inline).
- **EVT-3** Serialization MUST round-trip: `decode(encode(e)) == e` for every
  variant (property test, CONV-22).
- **EVT-4** Event log writer MUST be append-only, crash-safe (a torn final
  record is detected via length prefix + CRC32 and truncated on open, WARN).
- **EVT-5** Event log reader MUST stream a `venue/date` range in
  `(recv_ts_ns, stream_seq)` order across files, merging venues via k-way merge.
- **EVT-6** `Ring<T>` MUST support ≥ 1M events/sec single-producer with 3
  consumers on commodity hardware (bench test, not unit-timed assert —
  criterion benchmark checked for regression manually).
- **EVT-7** Overrun MUST be detectable by consumers deterministically
  (generation counter), and MUST NOT corrupt records mid-read.
- **EVT-8** `SymbolId` interning MUST be stable within a run and persisted in
  the event log header so replays resolve identically.
- **EVT-9** A `BookMirror` utility MUST reconstruct the order book from
  Snapshot + Deltas, validating `first_seq/last_seq` continuity; on gap it
  MUST mark itself stale and refuse reads until the next snapshot.

## Acceptance criteria
- [x] Property round-trip test per variant (EVT-3). `evt_3_envelope_roundtrip`, `evt_3_nan_sentinel_roundtrip`.
- [x] Torn-write recovery test: truncate a log mid-record, reopen, reader yields all whole records + WARN (EVT-4). `evt_4_torn_tail_recovered_on_open`, `evt_4_symbols_and_events_reload`.
- [x] K-way merge test across 2 venues × 2 days yields globally ordered stream (EVT-5). `evt_5_kway_merge_two_venues_two_days`.
- [x] Ring overrun test: slow consumer sees gap flag, no torn reads (EVT-7). `evt_7_ring_overrun_deterministic`, `evt_7_ring_no_torn_reads_under_contention`.
- [x] BookMirror gap test: out-of-sequence delta ⇒ stale until snapshot (EVT-9). `evt_9_book_gap_marks_stale_until_snapshot`, `evt_9_book_ignores_old_delta`.
- [x] EVT-2 heap-free Trade path proven by a counting allocator. `evt_2_trade_envelope_is_alloc_free`.

## Decisions
- 2026-07-10: bincode over protobuf for the internal log — single-language
  ecosystem, speed; Parquet (spec 003) is the interchange format for research.
- 2026-07-10 (impl): `SmallString` aliased to `String` in v1 — Status events
  are not on the per-trade hot path, so EVT-2's no-alloc rule is unaffected;
  an inline-string optimization is deferred.
- 2026-07-10 (impl): `trade_id` is `u64`; venues with string/u128 trade ids
  are hashed/truncated at the collector boundary (revisit if collisions seen).
- 2026-07-10 (impl): `Ring<T>` is `T: Copy` in v1 (sound concurrent overwrite
  without drop-in-place races). Non-`Copy` events (book deltas) flow via the
  owned log/channel path; a zero-copy arena ring is a later optimization.
  EVT-6's 1M/s target is a criterion bench checked manually, not a unit assert.
- 2026-07-10 (impl): event log gains an 8-byte magic + `format_ver` header and
  a per-frame CRC32 (`kind|len|crc|payload` framing) — realizes EVT-4's
  crash-safety and EVT-8's in-log symbol snapshots.
- 2026-08-03 (audit follow-up): `SCHEMA_VER` 1→2 added `provenance` to
  `EventEnvelope` (INT-1).  The reader keeps a backward-compatible decode for
  schema-1 frames (`EnvelopeV1` layout, provenance synthesized empty) so
  historical recordings stay readable; writes always stamp the current
  version.  See spec 024 Decisions for the promotion policy.
- 2026-08-05 (spec 028/030/031, owner sign-off): `SCHEMA_VER` 2→3, APPEND-ONLY
  (CONV-20 — old variant indices unchanged, so schema-2 frames stay readable):
  `Venue::{Deribit, Fred}` appended; `InstrumentKind::TradFiSynthetic` added
  (HIP-3 macro metadata, spec 030 MAC-1); new variants `WhalePosition`
  (028 WHL-2), `MacroPoint` (030 MAC-3),  `OptionTrade`/`OptionBook`/
  `OptionTicker` + `OptionLeg`/`OptionGreeks` types (031 OPT-2). Because the
  bump only *appended* enum variants (bincode maps variants by index, and the
  envelope layout never changed), schema-2 frames decode with the current
  types — no legacy `EnvelopeV2` struct is needed; proven by a golden compat
  test. Writes always stamp the current version.

## Open questions
- None.
