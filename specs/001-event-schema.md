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

## Amendment 2026-08-14 — bincode-2 migration (codec, BDC)

The event-log codec (`core::codec`, `core::log`, `core::arena`) was the only
consumer of bincode; bincode 1.3.3 became unmaintained (RUSTSEC-2025-0141,
no patched version). The backend was swapped to **`bincode-next`** (the
maintained continuation of the bincode-2 line) with **`config::legacy()`**,
which is byte-identical to bincode 1.3.3's default encoding — the format
every recorded log has used since 2026-07. The on-disk format (frame
layout, `FORMAT_VER`, `SCHEMA_VER`, `schema_ver` dispatch) is deliberately
unchanged (BDC-8); there is no data migration and old files decode
identically (BDC-4/BDC-6). bincode 2 documents serde-bridge edge cases, so
the compatibility claim is enforced by **golden-bytes tests** (BDC-1/BDC-2),
not asserted: vectors were captured with pinned 1.3.3 before the swap and
committed in `core/tests/bincode_migration.rs` — never regenerate them, a
drift must fail the test.

| bincode 1 (pre-swap) | bincode-2 line (current) |
|---|---|
| `bincode::serialize(&e)` | `encode_to_vec(&e, legacy())` |
| `bincode::deserialize(bytes)` | `decode_from_slice(bytes, legacy())?.0` |

`core::codec::wire_config()` names the wire format in exactly one place and
every encode/decode routes through it (BDC-2, BDC-5 — the `usize` from
`decode_from_slice` is dropped, preserving bincode 1's trailing-bytes
tolerance; EVT-4 framing already bounds payloads). The public error surface
is unchanged: `CodecError::Encode(String)` / `CodecError::Decode(String)`
(BDC-6). Crate choice: `bincode-next` over crates.io `bincode` 2.0.2
because RUSTSEC-2025-0141 has no patched version — plain `bincode` 2.x
stays flagged and would keep the audit debt alive (BDC-7).

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
- **BDC-1** The workspace MUST depend on the maintained bincode-2 line
  (`bincode-next = { version = "3", features = ["serde"] }`); `bincode = "1.3"`
  MUST be removed from the workspace manifest and `Cargo.lock` (no dual
  bincode-1 dependency).
- **BDC-2** Every encode/decode in `core::codec`, `core::arena`, `core::log`
  (including the `EnvelopeV1` legacy path) and every test fixture in `core` /
  `storage` MUST use the bincode-2 serde entry points with `config::legacy()`.
  No bare v1 API (`bincode::serialize` / `bincode::deserialize`) MAY remain.
- **BDC-3** The wire format MUST be byte-identical to bincode 1.3.3 output
  for every serialized type: `EventEnvelope` (all `MarketEvent` variants),
  `SymbolMeta`, `EnvelopeV1` — proven by committed golden vectors, not by
  assertion.
- **BDC-4** Every existing recorded artifact MUST decode unchanged:
  schema-1/2/3 event frames, symbol-table frames, and arena `EventRef`
  payloads must yield identical `EventEnvelope` / `SymbolMeta` values.
- **BDC-5** The codec MUST drop the `usize` returned by `decode_from_slice`
  (bincode 1's trailing-bytes tolerance preserved; EVT-4 framing already
  bounds payloads).
- **BDC-6** The public error surface MUST stay `CodecError::Encode(String)` /
  `CodecError::Decode(String)`; only the inner message source changes.
- **BDC-7** The cargo-audit job MUST stop reporting RUSTSEC-2025-0141 after
  the swap (verified with local `cargo audit` before push; requires the
  `bincode-next` dependency — the advisory has no patched versions).
- **BDC-8** This spec MUST NOT change `FORMAT_VER`, `SCHEMA_VER`, the frame
  layout, or the `schema_ver` dispatch; the on-disk log format is frozen
  (EVT-4/EVT-8, CONV-20).
- **BDC-9** Workspace `rust-version` MUST be raised to ≥ the adopted line's
  MSRV in the same commit (the toolchain is `stable`, so no build impact; the
  declared floor stays honest). `bincode-next` 3.x declares `rust-version`
  1.90, so the floor went 1.80 → 1.90.

## Acceptance criteria
- [x] Property round-trip test per variant (EVT-3). `evt_3_envelope_roundtrip`, `evt_3_nan_sentinel_roundtrip`.
- [x] Torn-write recovery test: truncate a log mid-record, reopen, reader yields all whole records + WARN (EVT-4). `evt_4_torn_tail_recovered_on_open`, `evt_4_symbols_and_events_reload`.
- [x] K-way merge test across 2 venues × 2 days yields globally ordered stream (EVT-5). `evt_5_kway_merge_two_venues_two_days`.
- [x] Ring overrun test: slow consumer sees gap flag, no torn reads (EVT-7). `evt_7_ring_overrun_deterministic`, `evt_7_ring_no_torn_reads_under_contention`.
- [x] BookMirror gap test: out-of-sequence delta ⇒ stale until snapshot (EVT-9). `evt_9_book_gap_marks_stale_until_snapshot`, `evt_9_book_ignores_old_delta`.
- [x] EVT-2 heap-free Trade path proven by a counting allocator. `evt_2_trade_envelope_is_alloc_free`.
- [x] BDC-1/BDC-2 golden bytes: bincode-2-line encode with `config::legacy()`
  equals the committed bincode-1.3.3 hex and decodes back to the exact
  source value for all 14 `MarketEvent` variants + `SymbolMeta` +
  `EnvelopeV1`. `bdc_1_golden_bincode1_bytes_unchanged`,
  `bdc_2_golden_bytes_decode_with_legacy`.
- [x] BDC-3 existing suite passes unchanged on the swapped codec — full
  `cargo test --workspace` green (2026-08-14).
- [x] BDC-4 no v1 API remains: grep for `bincode::serialize` /
  `bincode::deserialize` in `core/` + `storage/` sources returns nothing and
  `Cargo.lock` has no `bincode` package. `bdc_4_no_v1_bincode_api_remains`.
- [x] BDC-5 `cargo audit` (2026-08-14) reports no RUSTSEC-2025-0141; only
  the known `paste` RUSTSEC-2024-0436 debt remains. `bdc_5_audit_no_longer_flags_bincode`.
- [x] BDC-6 real-corpus readback (operator step, not CI — data is never
  committed, W-6): `data/collected.eventlog` (2026-07-16, pre-swap): 1088
  events, recv_ts 1784163765996808900 → 1784163806360752700 ns; live feed
  `data/raw/20260814_hyperliquid_BTC.log` (written by the still-running
  pre-swap collector): 27850 events across Trade/BookSnapshot/Funding/
  MarkPrice/OpenInterest/Status — both decode clean. `bdc_6_real_corpus_readback_matches`
  (`#[ignore]`d; run `cargo test -p mp-core --test bincode_migration --
  --ignored bdc_6`).

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
- 2026-08-14 (BDC, crate choice): **`bincode-next`** (maintained continuation
  of the bincode-2 line, wire-compatible, same API/configs) over crates.io
  `bincode` 2.0.2. RUSTSEC-2025-0141 has no patched version, so plain
  `bincode` 2.x stays flagged — the migration's purpose is clearing that
  advisory. If the owner prefers the canonical crate name, `bincode` 2.0.2
  is API- and format-identical and BDC-7 simply stays unsatisfied; all other
  requirements hold either way. The `core::codec` seam keeps a future crate
  swap to one file.
- 2026-08-14 (BDC, wire format): frozen at the bincode-1 byte layout via
  `config::legacy()`. No `FORMAT_VER` bump, no re-encode migration — W-6:
  nothing to rewrite, originals stay put. `config::standard()` (varint ints,
  small enum tags, varint lengths) is a real wire break (see Appendix —
  measured ~29% payload reduction); deferred: a future amendment if log size
  or replay cost ever justifies it.
- 2026-08-14 (BDC, serde bridge over bincode-derive): every serialized type
  already derives serde and EVT-3 property tests run through serde; adding
  bincode `Encode`/`Decode` derives to ~20 types buys nothing. `SmallVec`'s
  serde impls and any future serde attributes keep working through the
  serde bridge.
- 2026-08-14 (BDC, golden vectors are the arbiter): bincode 2 documents
  known edge cases in its serde bridge. If any vector diverges under
  `config::legacy()`, fix the affected type with an explicit compat shim
  (decode legacy bytes → normalize), never by switching wire configs.
- 2026-08-14 (BDC, MSRV): workspace `rust-version` 1.80 → 1.90 to match the
  adopted line's actual floor — `bincode-next` 3.1.1 declares
  `rust-version` 1.90 (edition 2024). The draft said 1.85 (bincode 2.0.2's
  MSRV); 1.90 is the honest floor for `bincode-next`. `stable` toolchain
  (1.96) is unaffected.

## Appendix (draft) — future wire format: `config::standard()` (varint)

Status: **decision draft, 2026-08-14 — not binding.** No requirements are
introduced here (deliberately: CONV-21 needs an ID-bearing test per
requirement, and this wire break is not being implemented). If adopted, it
becomes a new amendment to this spec with its own requirement prefix
(registered in `specs/README.md`) and ID-bearing acceptance tests.

### What changes on the wire

`config::standard()` = little-endian + **variable integer encoding** +
no limit (unlike LEB128, bincode 2's varint is a marker scheme: `u < 251` →
1 byte; `251–2^16` → 3; `2^16–2^32` → 5; `2^32–2^64` → 9; signed ints
zigzag-transformed first). `config::legacy()` (current, BDC-3) is fixed-width:
all integers fixed, enum discriminants as `u32`, lengths/usize as `u64`.

- **f64/f32 are unchanged** (8/4 bytes fixed) — and they dominate book
  payloads, which caps the win.
- **Savings**: enum tags (`u32` 4 B → 1 B), small `u32`/`u64` (stream_seq,
  trade_id, conn, symbol ids, lengths, `Option` tags), `String`/`Vec` length
  prefixes, zigzagged small negative `i64`.
- **Growth**: `u64` ≥ 2^32 costs 9 B (vs 8 today); `u32` ≥ 2^16 costs 5 B.
  `exch_ts_ns`/`recv_ts_ns` at 2026 scale (~1.78e18 ≈ 2^60.6) encode as
  **9 B — 1 B more** than fixed-width today.

### Measured impact (real corpus, 2026-08-14)

Payload size of the current envelope re-encoded with each config (measured
via a throwaway harness over `data/`; per-event averages):

| corpus | legacy | standard | delta |
|---|---|---|---|
| Trade-only day, 1088 events (2026-07-16) | 94.0 B/ev | 54.5 B/ev | **−42.0%** |
| Mixed live day, 30 347 events (2026-08-14, schema 3) | 149.2 B/ev | 103.8 B/ev | **−30.4%** |
| BookSnapshot subset | 768 → 707.9 B/ev | | **−7.8%** |
| Funding / MarkPrice / OpenInterest / Status / Trade | | | −43.8% / −37.5% / −36.6% / −33.1% / −30.7% |
| SymbolMeta snapshot (2 rows) | 203 B | 124 B | **−38.9%** |

File-level: the live day's 4.86 MB log → est. 3.47 MB (−28.7%; frame header
9 B/event + `schema_ver` 2 B are untouched by the codec swap). Warehouse
scale: `data/raw` holds 22.3 GB for ~28 days (~0.8 GB/day) → ~29% ≈ 6.5
GB/month reclaimed. Book-heavy venues (binance BTCUSDT at ~1.3 GB/day)
land near the −8% BookSnapshot end; small-event days nearer −40%.

### What it costs (the break)

1. **`FORMAT_VER` 1 → 2** (physical layer only). `schema_ver` stays 3 — it is
   the semantic envelope layout (CONV-20); `format_ver` is the physical
   codec. Today both `LogReader::open` and `scan_valid_len` hard-reject
   `fver != FORMAT_VER` (`LogError::BadFormat`), so the reader gains a
   dual-path: `fver == 1` → `legacy()` decode (exactly today's path),
   `fver == 2` → `standard()` decode. Frame envelope
   (`kind|len|crc`), file header, and `schema_ver` dispatch are unchanged.
2. **No data migration required for readability** — same pattern as the
   CONV-20 schema bumps: files written before cutover stay legacy-encoded
   and decode via the `fver == 1` path indefinitely. The BDC-1/BDC-2 golden
   vectors remain the legacy path's law; add `fver == 2` golden vectors
   (captured with pinned `standard()` before the flip, same procedure as
   BDC-1/2) for the new path.
3. **Writers flip at cutover**: all daily files after cutover are
   `fver == 2`. Mixed-format reads are per-file, so replay and the daily
   determinism check work unchanged.
4. **Optional re-encode migration (W-6 write-new-verify)** — rewrite each
   legacy file to `fver == 2` to reclaim the ~29% on historical data and
   eventually retire the legacy decode path. Cost: 2× disk headroom, a
   one-pass tool whose per-event `decode(encode(v)) == v` check doubles as a
   determinism check, and a cutover window. Not needed for correctness.
5. **CPU** — measured 2026-08-14 (`core/benches/codec_wire.rs`, criterion;
   `cargo bench -p mp-core --bench codec_wire`). Trade and BookDelta
   envelopes encode+decode in the sub-microsecond range under BOTH configs;
   decode is the cheap side (~110–160 ns/event) with no reproducible config
   gap. `standard()` encode is ~2× faster on trades (206 vs 398 ns/event —
   fewer bytes written: 65 vs 102 B) and within noise on book deltas (~255
   ns/event both). At replay scale (0.8 GB/day ≈ ~5 M events/day) decode is
   ≈1 s/day either way — replay CPU does not justify the flip; the disk win
   above is the only material effect. The dev laptop's run-to-run variance
   is high (up to 2× between consecutive runs), so the bench is a manual
   check, not a CI gate — re-run locally before any flip decision.
6. **Corruption**: varint misparse desyncs further than fixed-width, but
   frames are CRC32-bounded and length-prefixed (EVT-4) — no new risk.

### Recommendation

**Deferred (status quo).** The ~29% disk win is real but not binding: the
9 B timestamp fields and f64-pair book payloads erode it, and the cost is
concentrated in the reader dual-path + a second golden-vector set, not in
data movement. Revisit triggers:

- **disk budget** — raw data approaching available storage;
- **replay cost** — if log decode shows up in sim runtime profiles;
- **bundling** — if a format change is needed anyway (e.g. slimming the
  9 B/event frame header, or enabling bincode fingerprints), fold the
  varint flip in for a single `FORMAT_VER` bump. Priced in
  `docs/DESIGN-frame-header-slimming.md`: the header is 1.6% of warehouse
  bytes; the only realistic cut (len u32→u24, 9→8 B) adds ~1 point to the
  bundled win — a companion, not a motivation; u16 len is off the table
  (real 68 888 B frames) and crc16 is a contract downgrade.

## Open questions
- None.
