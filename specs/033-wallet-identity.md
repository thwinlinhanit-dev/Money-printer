# 033 — Wallet Identity in Trades (TradeWithAddr)

## Purpose

Keep the aggressor wallet address in recorded trades so research can grade
wallet-level edge: markout by wallet, wallet cohort persistence, and whale
taker-flow footprints (wallet-markout persistence is the strongest documented
crypto microstructure edge — ρ ≈ 0.52 daily, +13.2% R², arXiv 2608.04373).
Hyperliquid's public `trades` channel is the current producer; the address is
recorded as an opaque identifier and NEVER linked to external identity.

## Scope

In: a new `TradeWithAddr` event variant (spec 001 amendment, schema 3→4,
owner sign-off), Hyperliquid collector emitting it, a `trade_view()` unifier
so existing consumers match one arm. Out: wallet-cohort grading (deferred to
features 004 after an event study, same gate as WHL-5), exchange flows
(spec 034), any strategy input before RES-4 grading (PD-4).

## Design

```
Hyperliquid WS trades channel  ──▶  TradeWithAddr { price, qty, side, trade_id,
   (users[] first element)              taker_addr (opaque 0x, "" if absent) }
                                      │
                                      ▼
                        trade_view() — one arm for consumers that only need
                        price/qty/side/trade_id (schema-3 Trade included)
```

- `users[0]` is the taker wallet on Hyperliquid; `Side` is the aggressor
  ground truth (recorded as-is).
- Venues without wallet identity keep emitting `Trade`; both variants decode
  through `trade_view()` (WAL-6), so features/storage/sim are unchanged
  beyond one match arm.
- Append-only enum addition (CONV-20): old variant indices unchanged,
  schema-3 frames decode unchanged via the log reader's schema-3 arm.

## Requirements

- **WAL-1** MUST add `TradeWithAddr { price: f64, qty: f64, side: Side,
  trade_id: u64, taker_addr: String }` as an APPENDED `MarketEvent` variant
  (spec 001 amendment, `schema_ver` 3→4, CONV-20; owner sign-off before
  implementation).
- **WAL-2** MUST emit `TradeWithAddr` from the Hyperliquid collector with
  `taker_addr` = the first element of the frame's `users` array.
- **WAL-3** MUST record addresses as opaque 0x identifiers; MUST NOT import
  external labels/PII (same stance as WHL-3).
- **WAL-4** MUST record `""` when the venue payload omits the address —
  record nothing, never invent (mirrors Major #7 zero-price rule).
- **WAL-5** MUST keep `Trade` for venues without wallet identity; consumers
  MUST NOT assume every trade carries an address (`Option` semantics).
- **WAL-6** MUST provide `MarketEvent::trade_view() ->
  Option<(price, qty, side, trade_id, Option<&str>)>` so consumers that only
  need the common fields match a single arm; `None` for non-trade variants.
- **WAL-7** MUST be covered by: golden-bytes pin of the new variant (BDC-1/2
  table restamped to schema 4), proptest round-trip (CONV-22), fixture tests
  with recorded HL frames incl. a `users`-less frame asserting `""` (WAL-4,
  CONV-23 no network), and no-`unwrap` malformed-input tolerance (CONV-13/15).

## Acceptance criteria

- [x] `wal_2_users_first_element_is_taker_wallet` (venue_fixtures col_5: users `["0xabc123"]` asserted, users-less frame ⇒ `""`)
- [x] `wal_4_no_users_in_frame_records_empty_address` (venue_fixtures + fred_fixtures mac_1 HIP-3 path)
- [x] `wal_6_trade_view_unifies_trade_and_tradewithaddr` (features/storage/sim consumers all route through one arm)
- [x] `wal_7_schema4_golden_pins_trade_with_addr` (BDC-1/2 table restamped to schema 4 + proptest round-trip)
- [x] schema-4 golden restamp: all 16 variants pinned in `core/tests/bincode_migration.rs` (bdc_1/bdc_2/bdc_8), `liq_5`/`opt_10` pins bumped
- [x] BDC-3 suite green on the swap (full `cargo test --workspace`, 2026-08-18)

## Decisions

- 2026-08-18 (owner sign-off via "implement all"): schema 3→4 amendment for
  specs 033/034 — append-only variant additions only (CONV-20), so schema-3
  frames stay readable (log.rs reader arm `3 => decode`, no legacy struct).
  `SCHEMA_VER` pins (bdc_8, liq_5, opt_10) updated deliberately; `GOLDEN`
  restamped — existing entries changed ONLY in their leading `schema_ver`
  bytes (03→04), proven byte-for-byte in the test comment.
- 2026-08-18: `users[0]` chosen as taker wallet (HL documentation: the
  taker's address is the first element); empty-array / missing-key frames
  fall back to `""` (WAL-4) — a filter for research, never a fabrication.
- 2026-08-18: `trade_view()` selected over widening `Trade` (would change
  schema-3 bytes) or a second match everywhere (duplication): one unifier,
  `Option<&str>` keeps the address optional for legacy venues (WAL-5).
- 2026-08-18: consumer policy — `Trade`/`TradeWithAddr` are equivalent for
  price/qty/side consumers (features, storage, sim fills); wallet-aware
  research consumes `taker_addr` directly from the log (markout studies live
  in `research/`, graded per RES-4 before any feature promotion).

## Open questions

- None.