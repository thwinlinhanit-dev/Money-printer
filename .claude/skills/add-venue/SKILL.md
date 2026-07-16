---
name: add-venue
description: Add a new exchange/venue — market-data collector adapter (spec 002) and optionally a trading adapter (spec 007). Use for "add OKX", "support Hyperliquid", "record from Binance".
---

# Add a Venue

Governing specs: `002-collectors.md` (data side), `007-execution.md` (trading
side — separate task, only when asked), `001` (the schema you normalize into).

## Procedure (market data)

1. **Research the venue's CURRENT public API** (WS endpoints, topics, book
   sync algorithm, rate limits, ping/pong rules). The table in spec 002 is a
   starting point that drifts — verify against live docs, and record what you
   verified (URLs + date) in the adapter's module doc comment.

2. **Capture fixtures first.** Write/extend the small capture utility to
   record ~5 minutes of raw frames per stream into `testdata/{venue}/`
   (sanitize nothing — these are public feeds). Include: normal traffic, a
   book snapshot, and (synthesize if needed) a gap/out-of-sequence case and a
   malformed frame. Tests never hit the network afterwards (CONV-23).

3. **Implement the adapter** in `collectors/src/venues/{venue}.rs`:
   - topic subscription builder (respect per-connection topic limits, COL-4)
   - frame → `EventEnvelope` normalizer (field names per spec 001, exactly;
     `recv_ts_ns` stamped pre-parse, COL-5)
   - book sync per the venue's documented algorithm (COL-7) — this is the
     hard 20%; checksum venues (OKX) must verify checksums
   - rate-limit budget struct (COL-4), staleness thresholds (COL-2) in config
   - stream descriptor flags: mark sampled streams honestly (COL-8 — e.g.
     Binance forceOrder)

4. **Fixture tests (COL-13):** replay each fixture through the normalizer,
   assert exact normalized events; gap fixture ⇒ `Status::GapDetected` +
   resync; malformed ⇒ WARN + skip, never panic (CONV-15).

5. **Chaos integration (COL-14):** wire the venue into the mock-venue
   harness; scripted disconnect/gap/throttle must produce correct reconnect,
   resubscribe, and resync behavior.

6. **Soak:** run the real collector for 24h (or as long as the session
   allows — note actual duration), then verify: BookMirror replays clean,
   gaps only where Status events explain them, `/health` and `/metrics`
   sane. Report the soak results; do not claim the acceptance criterion met
   without one.

7. Update spec 002's venue table with anything you learned (quirks are
   institutional memory), update `specs/README.md` if status changes, commit
   with COL-x IDs (W-2).

## Trading adapter (only when explicitly tasked — it can lose money)
Follow spec 007: testnet first (EXE-12 pattern), idempotent submit proven by
the chaos test (EXE-3/5), UNKNOWN drill (EXE-4), `oms doctor` checks for the
venue's key-permission endpoint (EXE-10). PD-1 applies: you wire and test
against testnet/mock; the human turns anything live.

## Venue gotchas checklist
- [ ] exchange ts field units (ms vs µs vs ns) — normalize to ns
- [ ] price/qty as strings vs numbers; scientific notation
- [ ] book qty semantics: absolute (replace) vs delta (add) — spec 001 wants absolute new_qty
- [ ] snapshot in-band (Bybit) vs REST (Binance) vs snapshot-only (Hyperliquid)
- [ ] ping/pong: who initiates, payload format, timeout
- [ ] symbol naming (BTCUSDT vs BTC-USDT vs coin-margined variants) → SymbolMeta
- [ ] geo-blocks and testnet availability
