# 012 — Zero-Copy Event Pipeline (Ring Buffer Integration)

## Purpose
Eliminate heap allocation per `EventEnvelope` on the producer (collector) hot path so the ring buffer can carry 1M+ events/sec across multiple venues without GC pressure or allocation jitter.

## Scope
In: arena allocator, `EventRef` (Copy type), encoding/decoding through arena, chunked memory management, ring buffer integration. Out: changing `EventEnvelope` layout, removing existing `SmallVec` fields, SIMD optimizations.

## Design

### Arena
A thread-local chunked memory region. Each chunk is 64 KB (matching CPU L1 cache line sizing). The producer writes into the current chunk sequentially; when full, a new chunk is appended.

```
Arena {
    id: ArenaId,         // globally unique, assigned from static counter
    chunks: Vec<Chunk>,
    head: AtomicUsize,   // write cursor in current chunk
}
```

`ArenaId` is a `u16` assigned from `AtomicU16` global counter at arena creation. It identifies the arena in `EventRef.arena_id`.

The `Arena` implements `std::io::Write` — `write(&buf)` copies bytes into the current chunk's free space and bumps `head`. When the chunk is full, the arena seals it and allocates the next chunk from the pool. This write impl is what `bincode::serialize_into(&mut arena)` calls — zero-copy from the serializer's perspective (the arena's bump allocator never calls the global allocator within a chunk).

### EventRef
A `Copy` type that encodes a pointer into the arena:

```rust
#[derive(Clone, Copy)]
pub struct EventRef {
    arena_id: u16,
    offset: u32,
    len: u32,
}
```

### Encoding/decoding
- **Encode** (producer side): `bincode::serialize_into(&mut arena as &mut dyn Write, &envelope)` — writes directly into arena chunk memory via the `Write` impl, no heap alloc.
- **Decode** (consumer side, on read from ring): given `EventRef{arena_id, offset, len}`, get the `&[u8]` slice from the arena and call `bincode::deserialize::<EventEnvelope>(slice)` — allocates the `EventEnvelope` on the consumer heap.

### Chunk lifecycle
- Producer appends to current chunk; if full, acquires next chunk from global pool.
- Each chunk has a generation counter. Consumers hold a generation watermark.
- When all consumers have passed a chunk (their cursors are beyond it), the chunk is returned to the free pool.
- At most `N_CONSUMERS * 2 + 1` chunks are live at any time (backpressure bound).

## Requirements
- **ZCP-1** `core/src/arena.rs` MUST define `Arena`, `EventRef`, and chunk management. `EventRef` MUST be `Copy`.
- **ZCP-2** Encoding `EventEnvelope` → arena MUST NOT allocate on the producer hot path. Only the `Arena`'s own bump-allocation within a chunk is permitted.
- **ZCP-3** `Ring<EventRef>` MUST compile and pass all existing ring tests. The ring API is unchanged; only the inner type changes.
- **ZCP-4** Decoding `EventRef` → `EventEnvelope` on the consumer side MAY allocate; this is a deliberate trade-off (pay at consumption, not production).
- **ZCP-5** Chunks MUST be freed when all consumers have passed them (generational watermark). Memory usage MUST be bounded and MUST NOT grow unboundedly under steady state.
- **ZCP-6** `EventRef` and `Arena` MUST be `Send + Sync` for use across Ring consumer threads.

## Acceptance criteria
- [ ] `Ring<EventRef>` compiles and passes existing ring tests
- [ ] Producer can push 1M events/sec without heap allocation (benchmark)
- [ ] Consumer decode produces byte-identical `EventEnvelope` to original (property test)
- [ ] Memory usage is bounded: old chunks are freed, not leaked (integration test measuring RSS)
- [ ] Test: `zcp_1_event_ref_is_copy` — proves `EventRef: Copy`
- [ ] Test: `zcp_2_arena_encode_decode_roundtrip` — 1M events, no allocs on push
- [ ] Test: `zcp_3_ring_carries_event_ref` — multi-consumer, no torn reads
- [ ] Test: `zcp_4_chunk_pruning_frees_memory` — measure RSS before/after

## Decisions
- 2026-07-19: Chunk size 64 KB, matching CPU cache line sizing.
- 2026-07-19: `bincode` for encoding (same format as event log — no double work).
- 2026-07-19: When to implement: Phase 2 (Perceive) — not blocking Phase 0.

## Open questions
- None.
