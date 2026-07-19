# 013 — WebSocket Transport Backpressure Policy

## Purpose
Replace the current `try_send`-drops-silently pattern with a configurable backpressure policy so data gaps are detectable, recoverable, and policy-tunable per venue.

## Scope
In: `BackpressurePolicy` enum, channel capacity config, `Status::BackpressureDrop` event, per-venue policy config, reconnect-on-timeout for Block policy. Out: changes to WebSocket library, TLS, or connection lifecycle beyond backpressure.

## Design

### BackpressurePolicy
```rust
pub enum BackpressurePolicy {
    /// Block the WS read (stalls connection, venue may disconnect)
    Block,
    /// Drop oldest frames in channel (favors recency)
    DropOldest,
    /// Drop newest frames (favors completeness of old data)
    DropNewest,
    /// Grow channel unboundedly (OOM risk)
    Unbounded,
}
```

### Behavior per policy
- **Block**: `WsTransport::send` uses the channel's blocking `send` with configurable timeout (default 100ms). The producer blocks until the consumer frees a slot. If timeout expires, treat as disconnect: close transport, emit `Status::Disconnected`, begin reconnect loop. **No frames are ever dropped** under Block — the WS connection stalls instead.
- **DropOldest**: On channel full, drain one oldest frame from the channel, count it, push new frame. Emit `Status::BackpressureDrop { dropped: N }` once per drop batch (aggregated counter, not one event per dropped frame).
- **DropNewest**: On channel full, drop the incoming frame immediately. Same aggregated status event.
- **Unbounded**: Channel capacity is `usize::MAX`. Grow without bound (risk: OOM under sustained imbalance).

### Configuration
In the venue's section of `collectors.toml` (per-venue config, inline or top-level keys):
```toml
channel_capacity = 10_000
backpressure = "drop_oldest"
```

### Schema
Add to `StatusKind`:
```rust
pub enum StatusKind {
    Connected,
    Disconnected,
    GapDetected,
    Throttled,
    VenueHalt,
    Stale,
    BackpressureDrop { dropped: u64 },  // NEW
}
```
Bump `schema_ver` to 2.

## Requirements
- **BKP-1** `BackpressurePolicy` enum MUST be defined in `collectors/src/backpressure.rs`.
- **BKP-2** Policy MUST be selectable per-venue in the collector's TOML config.
- **BKP-3** Every drop batch MUST emit a `Status::BackpressureDrop { dropped: u64 }` event with the count of frames dropped in that batch, so downstream knows data is missing.
- **BKP-4** For `Block` policy: use `send` with timeout, not `try_send`. If timeout expires, treat as disconnect and reconnect.
- **BKP-5** Channel capacity MUST be configurable (default: 10,000 frames).

## Acceptance criteria
- [ ] All four policies compile and are tested
- [ ] `Status::BackpressureDrop` event exists in schema (schema_ver bump to 2)
- [ ] Test: `bkp_1_block_policy_never_drops` — slow consumer, verify no drops
- [ ] Test: `bkp_2_drop_oldest_favors_recency` — verify newest frame preserved
- [ ] Test: `bkp_3_drop_emits_status_event` — verify `Status::BackpressureDrop` with correct count
- [ ] Test: `bkp_4_block_timeout_triggers_reconnect` — mock slow consumer, verify reconnect
- [ ] Test: `bkp_5_unbounded_grows_under_load` — verify no drops, monitor memory

## Decisions
- 2026-07-19: Default policy: `DropOldest` for Phase 0 (survival over completeness).
- 2026-07-19: `Block` is recommended for Phase 2+ when L2 book integrity matters.
- 2026-07-19: Schema bump to 2 required for `StatusKind::BackpressureDrop`.

## Open questions
- None.
