# 002 — Venue Collectors

## Purpose
24/7 processes that turn each venue's public WebSocket dialect into the
normalized event stream (spec 001) and the raw capture archive. The recorder
is the moat; a gap is a wound.

## Scope
In: WS connection management, normalization, book resync, raw capture, health.
Out: private/auth streams (spec 007 owns order streams), storage tiers (003).

## Design

One binary `collector`, one process per venue (config-selected), each running:

```
WS task(s) ──raw frames──▶ raw capture writer (NDJSON.zst, verbatim)
        │
        └──▶ normalizer ──▶ Ring<EventEnvelope> ──▶ event-log writer
                                             └──▶ (optional) NATS publisher
```

Subscriptions per symbol (as available per venue): trades, book deltas (+
snapshot channel or REST snapshot), mark/funding, open interest, liquidations.

### Venue notes (verify at implementation time — venues drift)
| Venue | Trades | Book | Funding/Mark | OI | Liq | Quirks |
|---|---|---|---|---|---|---|
| Binance Futures | `aggTrade` | `depth@100ms` + REST snapshot | `markPrice@1s` | REST poll 60s | `forceOrder` | liq stream throttled ~1/s ⇒ it is a SAMPLE; geo-blocked in US |
| Bybit | `publicTrade` | `orderbook.50/500` (snapshot+delta in-band) | `tickers` | `tickers` | `liquidation` | topic batch limits per conn |
| OKX | `trades` | `books` (snapshot+delta, checksum!) | `funding-rate`,`mark-price` | `open-interest` | `liquidation-orders` | verify book checksum |
| Hyperliquid | `trades` | `l2Book` snapshots | `activeAssetCtx` | same | via fills | snapshot-only book (no deltas) |
| Coinbase | `market_trades` | `level2` | — (spot) | — | — | auth-free channels only |
| Kraken Futures | `trade` | `book` | `ticker` | `ticker` | — | — |

## Requirements

### Connection management
- **COL-1** Each WS connection MUST auto-reconnect with jittered exponential
  backoff (base 1s, cap 60s, full jitter) and MUST resubscribe its topics.
- **COL-2** Per-stream staleness watchdog: no message for `stale_after_s`
  (default: trades 30s, book 10s, others 120s) ⇒ emit `Status::Stale`, force
  reconnect. Thresholds per venue-stream in config.
- **COL-3** Connect/disconnect/resubscribe MUST emit `Status` events into the
  normal event stream (they are data).
- **COL-4** Venue rate limits (connect rate, subscriptions per connection,
  REST weight) MUST be enforced client-side by a budget struct per venue;
  budgets defined in the venue adapter, not scattered.

### Normalization
- **COL-5** Every normalized event MUST carry `recv_ts_ns` stamped at socket
  read (before parse), and venue `exch_ts_ns` when present.
- **COL-6** Unknown/malformed messages: WARN + `messages_dropped` counter +
  continue (CONV-15). Ten consecutive parse failures on one stream ⇒ reconnect.
- **COL-7** Book maintenance MUST follow each venue's documented sync
  algorithm (e.g. Binance: buffer deltas, REST snapshot, drop deltas ≤
  snapshot seq, verify continuity; OKX: verify checksum). On any gap:
  emit `Status::GapDetected`, resync via snapshot, tag the new
  `BookSnapshot{reason: GapResync}`.
- **COL-8** Liquidation streams MUST be recorded as-is; the Binance sampling
  caveat MUST be documented in the symbol/venue metadata so research code can
  see it (a `sampled: bool` flag on the stream descriptor).

### Raw capture
- **COL-9** Raw frames MUST be captured verbatim (pre-parse) to
  `raw/{venue}/{date}/{stream}.ndjson.zst`, rotated daily, with `recv_ts_ns`
  prefix per line. Retention config-driven (default 14 days) — raw exists to
  re-normalize after venue schema drift.

### Health & ops hooks
- **COL-10** Collector MUST expose a local HTTP `/health` returning per-stream
  {last_msg_age, msg_rate_1m, gaps_today, reconnects_today} JSON — consumed by
  the dead-man switch (spec 009).
- **COL-11** Metrics counters (events by type, drops, gaps, reconnects, ring
  overruns) MUST be exported (prometheus text on `/metrics`).
- **COL-12** Graceful shutdown (SIGTERM): flush event log + raw capture,
  fsync, exit 0 within 5s.

### Testing
- **COL-13** Each venue adapter MUST have fixture tests: recorded real frames
  in `testdata/{venue}/` replayed through the normalizer, asserting exact
  normalized output (CONV-23). Include at least: trade, delta, snapshot,
  gap sequence, malformed frame.
- **COL-14** A `mock-venue` WS server harness MUST exist for integration
  tests: scripted disconnects, gaps, throttles ⇒ assert reconnect/resync
  behavior (COL-1, COL-7).

## Acceptance criteria
- [~] One venue (Bybit) normalizes trades+book+funding+OI+liq through a simulated disconnect/gap cycle — done at the normalizer+driver level (`col_1_reconnects_after_disconnect_with_backoff`, `col_7_*`); end-to-end over a *live* socket awaits the transport.
- [~] Fixture tests pass (COL-13) — `bybit_fixtures.rs` green, but fixtures are SYNTHETIC-representative, not real captures (see Decisions). Real-capture validation is the remaining COL-13 work.
- [~] Gap and reconnect counters move under mock-venue chaos (COL-14) — `collector_chaos.rs` green. `/health` + `/metrics` HTTP export (COL-10/11) deferred with the live transport.
- [ ] 24h soak on a real venue (needs the live WS transport → network dependency approval).

## Decisions
- 2026-07-10: first venue = Bybit (API-friendly, no US geo-block, in-band
  book snapshots). Binance second, from a non-US host.
- 2026-07-10 (impl): the collector is split into a **transport-agnostic core**
  (this crate) and a live WS transport (deferred). Adding a network dependency
  (tokio/tokio-tungstenite) is must-ask-first (CLAUDE.md); until approved, all
  logic is driven through the `Transport` trait and a `MockTransport`. This is
  also better design: venue logic is isolated from I/O and fully unit-testable.
- 2026-07-10 (impl): fixtures are SYNTHETIC frames built to documented Bybit v5
  shapes (knowledge cutoff), not real captures — COL-13 is only partially met
  until real frames are recorded and diffed. Flagged honestly (PD-5).
- 2026-07-10 (impl): book gap policy keyed on Bybit's `type:"snapshot"` reset +
  `u == prev+1` continuity; on gap → `Status::GapDetected`, drop deltas until
  the next snapshot (emitted with `reason: GapResync`).
- 2026-07-10 (impl): `Funding.interval_s` set to 0 (unknown) — Bybit's tickers
  stream omits it; fill from REST instrument info when that path exists.
- 2026-07-10 (impl): deferred within spec 002 — raw capture (COL-9), `/health`
  + `/metrics` HTTP (COL-10/11), graceful shutdown (COL-12), venue-wide
  connect/disconnect Status events (COL-3), and the `sampled` stream flag
  (COL-8) — all tracked, none silently dropped.

## Open questions
- NATS fan-out: required only when features run in a separate process — defer
  until spec 004 implementation chooses a topology.
