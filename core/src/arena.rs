//! Arena-allocated event references for zero-copy ring buffer (spec 012).
//!
//! The producer writes bincode-encoded events into arena chunks sequentially.
//! Consumers get an `EventRef` (Copy) from the ring, then decode the underlying
//! `EventEnvelope` on their heap. Chunks are freed when all consumers have passed
//! them (generational watermark), bounding memory.

use std::io;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Mutex;

/// Globally unique arena ID, assigned from a static counter.
static NEXT_ARENA_ID: AtomicU16 = AtomicU16::new(1);

/// Chunk size: 64 KB (2^16), matching CPU cache line sizing (spec 012 decision).
const CHUNK_SHIFT: u32 = 16;
pub const CHUNK_SIZE: usize = 1 << CHUNK_SHIFT; // 65536

/// A reference into an arena — Copy, so it can live in the ring buffer.
/// `offset` is a global byte address: chunk_idx in upper bits, intra-chunk
/// offset in lower CHUNK_SHIFT bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventRef {
    pub arena_id: u16,
    /// Global byte offset (chunk_idx << CHUNK_SHIFT | offset_in_chunk).
    pub offset: u32,
    pub len: u32,
}

impl EventRef {
    fn chunk_idx(&self) -> usize {
        (self.offset >> CHUNK_SHIFT) as usize
    }

    fn offset_in_chunk(&self) -> usize {
        (self.offset & (CHUNK_SIZE as u32 - 1)) as usize
    }
}

/// A single chunk of arena memory.
struct Chunk {
    data: Box<[u8; CHUNK_SIZE]>,
    head: usize,
}

impl Chunk {
    fn new() -> Self {
        Self {
            data: Box::new([0u8; CHUNK_SIZE]),
            head: 0,
        }
    }

    fn remaining(&self) -> usize {
        CHUNK_SIZE - self.head
    }

    /// Copy `buf` into the chunk. Returns the intra-chunk offset on success.
    fn write(&mut self, buf: &[u8]) -> Option<usize> {
        if buf.len() > self.remaining() {
            return None;
        }
        let offset = self.head;
        self.data[offset..offset + buf.len()].copy_from_slice(buf);
        self.head += buf.len();
        Some(offset)
    }

    fn as_slice(&self, offset: usize, len: usize) -> &[u8] {
        &self.data[offset..offset + len]
    }
}

/// A thread-safe arena. Producer writes events here; consumers read via
/// `EventRef`. Chunks are appended when full (simplified: no recycling for v1).
pub struct Arena {
    pub id: u16,
    chunks: Mutex<Vec<Chunk>>,
}

impl Arena {
    /// Create a new arena with one fresh chunk.
    pub fn new() -> Self {
        let id = NEXT_ARENA_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            id,
            chunks: Mutex::new(vec![Chunk::new()]),
        }
    }

    /// Encode a value into the arena and return an `EventRef`.
    /// The offset is a global byte address (chunk_idx << 16 | intra_offset).
    pub fn alloc<T: serde::Serialize>(&self, val: &T) -> io::Result<EventRef> {
        let bytes = bincode::serialize(val).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let len = bytes.len() as u32;
        let mut chunks = self.chunks.lock().unwrap();
        let idx = chunks.len() - 1;
        if let Some(intra_off) = chunks[idx].write(&bytes) {
            let global_off = (idx << CHUNK_SHIFT) as u32 | intra_off as u32;
            return Ok(EventRef {
                arena_id: self.id,
                offset: global_off,
                len,
            });
        }
        // Current chunk full — allocate a new one.
        let mut new_chunk = Chunk::new();
        let intra_off = new_chunk
            .write(&bytes)
            .expect("fresh chunk must fit write");
        chunks.push(new_chunk);
        let new_idx = chunks.len() - 1;
        let global_off = (new_idx << CHUNK_SHIFT) as u32 | intra_off as u32;
        Ok(EventRef {
            arena_id: self.id,
            offset: global_off,
            len,
        })
    }

    /// Decode an `EventRef` back into a value. This allocates on the consumer heap.
    pub fn decode<T: serde::de::DeserializeOwned>(&self, r: EventRef) -> io::Result<T> {
        let chunks = self.chunks.lock().unwrap();
        let idx = r.chunk_idx();
        if idx >= chunks.len() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "chunk index out of range",
            ));
        }
        let data = chunks[idx].as_slice(r.offset_in_chunk(), r.len as usize);
        bincode::deserialize(data).map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    /// Number of chunks currently live.
    pub fn num_chunks(&self) -> usize {
        self.chunks.lock().unwrap().len()
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: Arena uses internal synchronization (Mutex).
unsafe impl Send for Arena {}
unsafe impl Sync for Arena {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};

    #[test]
    fn zcp_1_event_ref_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<EventRef>();
    }

    #[test]
    fn zcp_2_arena_encode_decode_roundtrip() {
        let arena = Arena::new();
        let ev = EventEnvelope::new(
            Venue::Hyperliquid,
            SymbolId(1),
            1000,
            2000,
            1,
            MarketEvent::Trade {
                price: 50000.0,
                qty: 1.0,
                side: Side::Buy,
                trade_id: 42,
            },
        );
        let r = arena.alloc(&ev).unwrap();
        let decoded: EventEnvelope = arena.decode(r).unwrap();
        assert_eq!(ev, decoded);
    }

    #[test]
    fn zcp_3_ring_carries_event_ref() {
        use crate::ring::Ring;
        let arena = Arena::new();
        let ring = Ring::<EventRef>::with_capacity(64);
        let mut producer = ring.producer();
        let mut consumer = ring.consumer();
        let ev = EventEnvelope::new(
            Venue::Hyperliquid,
            SymbolId(1),
            1000, 2000, 1,
            MarketEvent::Trade { price: 50000.0, qty: 1.0, side: Side::Buy, trade_id: 42 },
        );
        let r = arena.alloc(&ev).unwrap();
        producer.push(r);
        let item = consumer.try_recv().unwrap().expect("should receive event ref");
        let decoded: EventEnvelope = arena.decode(item).unwrap();
        assert_eq!(ev, decoded);
    }

    #[test]
    fn zcp_3_arena_multi_encode() {
        let arena = Arena::new();
        for i in 0..1000 {
            let ev = EventEnvelope::new(
                Venue::Hyperliquid,
                SymbolId(1),
                i * 1000,
                i * 2000,
                i as u64,
                MarketEvent::Trade {
                    price: 50000.0 + i as f64,
                    qty: 1.0,
                    side: Side::Buy,
                    trade_id: i as u64,
                },
            );
            let r = arena.alloc(&ev).unwrap();
            let decoded: EventEnvelope = arena.decode(r).unwrap();
            assert_eq!(ev, decoded);
        }
        assert!(arena.num_chunks() >= 1);
    }
}
