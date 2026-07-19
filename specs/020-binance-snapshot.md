# 020 — Binance REST Snapshot Seeding

## Purpose
Fix incomplete book reconstruction by fetching a REST depth snapshot on connect (instead of synthetic-seeding from the first delta), and validate delta continuity against the snapshot's `pu` (previous update ID).

## Scope
In: REST snapshot fetch, rate limit awareness, snapshot-before-deltas ordering, sequence continuity check, gap detection + re-fetch. Out: other venue snapshot logic, WebSocket implementation details.

## Design

### Snapshot fetch
```rust
async fn fetch_depth_snapshot(symbol: &str, limit: u16) -> Result<BookSnapshot, Error> {
    // GET /api/v3/depth?symbol={symbol}&limit={limit}  (spot)
    // Or     /fapi/v1/depth?symbol={symbol}&limit={limit}  (futures)
    // Parse JSON, normalize to BookSnapshot
}
```

Uses `reqwest` with `rustls` (feature-gated behind `live-http`, same pattern as `live-ws`).

### Rate limits
- Binance REST rate limits differ by endpoint:
  - Spot `/api/v3/depth`: weight 5 (limit≤100), weight 25 (limit≤500), weight 50 (limit≤1000).
  - Futures `/fapi/v1/depth`: weight 2 (limit≤50), weight 5 (limit≤100), weight 50 (limit≤5000).
- The endpoint (spot vs futures) must match the WebSocket connection endpoint. If using spot WS (`stream.binance.com:9443`), use spot REST; if futures WS (`fstream.binance.com`), use futures REST.
- Use `RateBudget` (already exists in collectors) to track and wait.
- On rate limit hit: exponential backoff, WARN log.

### Snapshot-before-deltas ordering
On WebSocket connect:
1. Fetch REST snapshot → `BookSnapshot` with `seq = lastUpdateId`.
2. Store snapshot as current book state.
3. Process incoming `depthUpdate` messages that have `pu >= snapshot.lastUpdateId`.
4. Skip deltas with `U <= lastUpdateId` (already included in snapshot).

### Continuity check
- Each `depthUpdate` has `pu` (previous update ID) and `u` (current update ID).
- After snapshot: verify first delta has `pu == snapshot.lastUpdateId`.
- On mismatch: emit `Status::GapDetected`, clear book, re-fetch snapshot.
- On subsequent deltas: verify `pu == previous_u`. On mismatch: gap detected, re-fetch.

### Snapshot caching
- Cache snapshot for 5 seconds.
- On reconnect storm (rapid disconnects), reuse cached snapshot to avoid rate limit hits.

## Requirements
- **COL-20** REST snapshot fetch MUST be implemented in `collectors/src/binance.rs` (or `collectors/src/snapshot.rs`).
- **COL-21** Snapshot fetch MUST respect rate limits via `RateBudget`.
- **COL-22** Snapshot MUST be fetched before processing deltas on WebSocket connect.
- **COL-23** Snapshot seq MUST match WebSocket `pu` for continuity.
- **COL-24** If snapshot/delta mismatch: emit `Status::GapDetected`, re-fetch snapshot.

## Acceptance criteria
- [ ] REST snapshot fetch implemented and tested
- [ ] Test: `col_20_snapshot_matches_documented_shape` — fixture from real API response
- [ ] Test: `col_21_rate_limit_respected` — verify budget consumption
- [ ] Test: `col_22_snapshot_before_deltas` — verify ordering
- [ ] Test: `col_23_seq_continuity_verified` — mock snapshot seq=100, delta pu=100, applies
- [ ] Test: `col_24_mismatch_triggers_gap` — mock snapshot seq=100, delta pu=105, gap detected
- [ ] Integration: run against Binance testnet, verify book integrity

## Decisions
- 2026-07-19: Use `reqwest` with `rustls` (feature-gated, like `live-ws`).
- 2026-07-19: Snapshot limit: 100 (Binance max for free tier).
- 2026-07-19: Cache snapshot for 5 seconds to avoid duplicate fetches on reconnect storm.

## Open questions
- Should we also implement snapshot seeding for other venues (e.g., Bybit REST)? Deferred until Phase 2.
