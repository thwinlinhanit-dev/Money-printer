# core

## Purpose

Foundation types shared across all crates: event schema (`MarketEvent`, `EventEnvelope`, `EventProvenance`), arena allocator, order book, binary codec, event log reader/writer, mode switch, symbol interning, and wall clock abstraction.

## Ownership

- `src/event.rs` — `MarketEvent` enum, `EventEnvelope`, `EventProvenance`, `Venue`, `SymbolId`, `StatusKind`, `SnapshotSource`
- `src/log.rs` — `EventLogWriter`/`EventLogReader` for the binary event log format
- `src/codec.rs` — binary encoding/decoding of events
- `src/book.rs` — `OrderBook` data structure
- `src/arena.rs` — arena allocator
- `src/symbol.rs` — symbol interning (`SymbolArena`, `SymbolMeta`)
- `src/exec.rs` — execution types
- `src/hash.rs` — hashing utilities
- `src/mode.rs` — system mode switching
- `src/ring.rs` — ring buffer
- `src/rng.rs` — RNG utilities
- `src/time.rs`, `src/wall_clock.rs` — time abstractions
- `src/bin/inspect_log.rs` — binary event log inspector

## Local Contracts

- Event log format: length-delimited binary frames with CRC32C checksums (spec 001, 014)
- `EventLogWriter` must flush after every write (spec 014)
- All timestamps are nanoseconds since Unix epoch as `i64`
- Symbol arena must be written to log header before any symbol-referencing events

## Verification

- `cargo test -p mp-core`
- `cargo test -p mp-core --test event_schema`

## Child DOX Index

None.
