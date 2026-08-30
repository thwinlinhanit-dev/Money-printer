# 014 — Event Log Fsync Policy

## Purpose
Prevent data loss from crashes by adding automatic, configurable fsync to `EventLogWriter`. Currently `sync()` exists but is never called automatically.

## Scope
In: `FsyncPolicy` config struct, auto-fsync on event count and time thresholds, `sync_data()` (faster) vs `sync_all()`, SIGTERM graceful shutdown hook. Out: CRC-32 (IEEE, via crc32fast) torn-write detection (already handled by EVT-4), filesystem-level durability beyond `fsync`.

## Design

### FsyncPolicy
```rust
pub struct FsyncPolicy {
    /// Fsync every N events (0 = disabled)
    pub every_n_events: u64,
    /// Fsync every N nanoseconds (0 = disabled)
    pub every_ns: i64,
    /// Fsync on graceful shutdown (SIGTERM)
    pub on_sigterm: bool,
}
```

### Auto-fsync
`EventLogWriter::append()` tracks:
- `events_since_fsync: u64` — incremented per append
- `last_fsync_ns: i64` — wall-clock timestamp of last fsync

After every append, if either threshold is crossed, schedule an async `sync_data()` call (`tokio::fs::File::sync_data()` on the tokio runtime, or `spawn_blocking(move || file.sync_data())`). `sync_data()` is preferred over `sync_all()` — we don't need metadata sync for append-only logs. Reset counters after completion.

### SIGTERM hook
- Register a signal handler at startup via `ctrlc` crate or `tokio::signal`.
- On SIGTERM: flush any pending writes, `sync_data()`, close file, exit cleanly.
- Handler must be registered before any file I/O begins.

### Config
```toml
[fsync]
every_n_events = 1000
every_sec = 10       # converted to ns internally (10_000_000_000)
on_sigterm = true
```

## Requirements
- **FSP-1** `FsyncPolicy` MUST be defined in `core/src/storage.rs` (or a dedicated `core/src/fsync.rs`).
- **FSP-2** `EventLogWriter::append()` MUST track event count and time since last fsync.
- **FSP-3** `append()` MUST check thresholds and auto-fsync when crossed, using `sync_data()` by default.
- **FSP-4** SIGTERM handler MUST flush and fsync on graceful shutdown.
- **FSP-5** Fsync MUST NOT block the event loop: use `tokio::fs::File::sync_data()` or `spawn_blocking` for `sync_data()`, not `sync_all()`, unless configured otherwise.

## Acceptance criteria
- [x] `FsyncPolicy` exists and is configurable — `core/src/log.rs` (`FsyncPolicy` + `set_fsync_policy`); the `[fsync]` TOML section is collector-config scope (spec 019)
- [x] Test: `fsp_1_every_n_events_triggers_fsync` — append N+1 events, verify fsync count (`core/src/log.rs` tests)
- [x] Test: `fsp_2_time_based_fsync` — event-time clock, verify fsync at interval (`core/src/log.rs` tests)
- [x] Test: `fsp_3_sigterm_handler_fsyncs` — shutdown hook durability verified via leaked-writer test (`core/src/log.rs` tests); OS signal wiring in collector binaries (spec 002/019)
- [x] Test: `fsp_4_fsync_does_not_block_append` — zero fsync decisions below thresholds (`core/src/log.rs` tests)
- [x] Test: `fsp_5_crash_recovery_loses_at_most_n_events` — kill -9 simulation, boundary verified (`core/src/log.rs` tests)

## Decisions
- 2026-07-19: Default: `every_n_events: 1000`, `every_ns: 10_000_000_000` (10s), `on_sigterm: true`.
- 2026-07-19: Use `sync_data()` (faster) by default — metadata sync not needed for append-only log.
- 2026-07-19: SIGTERM handler via `ctrlc` crate or custom signal hook.
- 2026-08-28: `FsyncPolicy` lives in `core/src/log.rs`, not `core/src/storage.rs`/`fsync.rs` — the spec's named paths are stale; the event log and its policy belong together (single home, single test module).
- 2026-08-28: The cadence is paced on **event time** (`recv_ts_ns`), not the wall clock (CONV-5: no wall-clock reads on the write path). The time interval is measured from the first appended event after the policy takes effect (`time_armed` anchor); writing `fsp_2` exposed that the old `last_fsync_ns > 0` guard made a pure time-based policy (`every_n_events: 0`) never fsync at all — fixed by the anchor.
- 2026-08-28: The fsync is **synchronous** inside `append()` (a bounded stall per call), not `tokio::fs`/`spawn_blocking` as the Design section sketched: the collector write loop is synchronous `std::fs`, and FSP-5's intent is honored as "no fsync below the thresholds, `sync_data` (never `sync_all`) at the boundaries". An async redesign is deferred until a write loop actually needs it.
- 2026-08-28: FSP-4's OS signal wiring lives in the collector binaries (spec 002/019): core exposes `sync_on_shutdown()`; `mp-collector` wires shutdown fsync today, the other collector binaries follow.

## Open questions
- None.
