//! Single-producer, multi-consumer broadcast ring (EVT-6, EVT-7).
//!
//! Every consumer sees every item independently (broadcast, not work-steal).
//! A slow consumer that falls more than `capacity` behind is *overrun*: it is
//! told exactly how many items it missed and resynced to the oldest still-live
//! item, and it never blocks the producer (spec 002 requires overrun be
//! treated as a gap downstream). Detection uses the LMAX-Disruptor per-slot
//! sequence protocol; a read whose slot sequence changes underneath it is
//! discarded as an overrun rather than returned torn (EVT-7).
//!
//! v1 scope: payloads are `Copy` (spec 001 Decisions). This keeps the
//! concurrent overwrite path sound (no drop-in-place of a value another thread
//! may be reading). Non-`Copy` events (book deltas) flow via the owned
//! log/channel path; a zero-copy arena ring is a later optimization.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const EMPTY: u64 = u64::MAX;

struct Slot<T: Copy> {
    seq: AtomicU64,
    val: UnsafeCell<T>,
}

/// Shared broadcast ring. Construct with [`Ring::with_capacity`], then take one
/// [`Producer`] and any number of [`Consumer`]s.
pub struct Ring<T: Copy> {
    mask: u64,
    slots: Box<[Slot<T>]>,
    /// Next index the producer will write (also the count of items ever pushed).
    write_pos: AtomicU64,
}

// SAFETY: access is disciplined — a single Producer writes each slot then
// publishes via `seq` (Release); consumers read `seq` (Acquire) before and
// after touching the value and discard the read on any change. `T: Send` makes
// moving values across threads sound; `T: Copy` avoids concurrent drops.
unsafe impl<T: Copy + Send> Send for Ring<T> {}
unsafe impl<T: Copy + Send> Sync for Ring<T> {}

/// Result of a consumer read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overrun {
    /// The consumer fell behind by `skipped` items, now resynced to the oldest
    /// live item. Treat as a gap (emit `Status::GapDetected` downstream).
    Overrun { skipped: u64 },
}

impl<T: Copy> Ring<T> {
    /// Create a ring with capacity rounded up to a power of two (min 2).
    pub fn with_capacity(cap: usize) -> Arc<Self> {
        let cap = cap.next_power_of_two().max(2);
        let mut v = Vec::with_capacity(cap);
        for _ in 0..cap {
            v.push(Slot {
                seq: AtomicU64::new(EMPTY),
                // The initial value is never returned: a slot is only readable
                // after the producer publishes a real value with seq == index.
                val: UnsafeCell::new(unsafe { std::mem::zeroed() }),
            });
        }
        Arc::new(Self {
            mask: (cap - 1) as u64,
            slots: v.into_boxed_slice(),
            write_pos: AtomicU64::new(0),
        })
    }

    /// Capacity (number of slots).
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Take the sole producer handle. Call once; a second call also yields a
    /// producer but using two concurrently violates the single-producer
    /// contract (undefined). Kept non-`Clone` to make misuse deliberate.
    pub fn producer(self: &Arc<Self>) -> Producer<T> {
        Producer { ring: self.clone() }
    }

    /// Create a consumer starting at the oldest currently-live item.
    pub fn consumer(self: &Arc<Self>) -> Consumer<T> {
        let w = self.write_pos.load(Ordering::Acquire);
        let cap = self.capacity() as u64;
        let cursor = w.saturating_sub(cap);
        Consumer {
            ring: self.clone(),
            cursor,
        }
    }

    #[inline]
    fn push(&self, v: T) {
        let w = self.write_pos.load(Ordering::Relaxed); // sole producer
        let slot = &self.slots[(w & self.mask) as usize];
        // Mark slot in-flight so a mid-flight reader at the previous occupant's
        // index sees the sequence change and discards its read (EVT-7).
        slot.seq.store(EMPTY, Ordering::Release);
        unsafe {
            *slot.val.get() = v;
        }
        slot.seq.store(w, Ordering::Release);
        self.write_pos.store(w + 1, Ordering::Release);
    }
}

/// Producer handle (one per ring).
pub struct Producer<T: Copy> {
    ring: Arc<Ring<T>>,
}

impl<T: Copy> Producer<T> {
    /// Publish one item. Never blocks; overwrites the oldest slot when full.
    pub fn push(&mut self, v: T) {
        self.ring.push(v);
    }

    /// Total items pushed so far.
    pub fn pushed(&self) -> u64 {
        self.ring.write_pos.load(Ordering::Acquire)
    }
}

/// Consumer handle (many per ring); each sees the full stream independently.
pub struct Consumer<T: Copy> {
    ring: Arc<Ring<T>>,
    cursor: u64,
}

impl<T: Copy> Consumer<T> {
    /// Try to read the next item.
    ///
    /// - `Ok(Some(v))` — next item in sequence.
    /// - `Ok(None)` — caught up, nothing new yet.
    /// - `Err(Overrun{skipped})` — fell behind; resynced to the oldest live
    ///   item. Retry to continue from there.
    pub fn try_recv(&mut self) -> Result<Option<T>, Overrun> {
        let ring = &self.ring;
        let cap = ring.capacity() as u64;
        loop {
            let w = ring.write_pos.load(Ordering::Acquire);
            if self.cursor >= w {
                return Ok(None);
            }
            // Oldest still-live index. If our cursor is older, we were overrun.
            let oldest = w.saturating_sub(cap);
            if self.cursor < oldest {
                let skipped = oldest - self.cursor;
                self.cursor = oldest;
                return Err(Overrun::Overrun { skipped });
            }
            let slot = &ring.slots[(self.cursor & ring.mask) as usize];
            let seq1 = slot.seq.load(Ordering::Acquire);
            if seq1 != self.cursor {
                // Slot is mid-write or already advanced past us → overrun; loop
                // to recompute against the latest write_pos.
                let oldest = ring.write_pos.load(Ordering::Acquire).saturating_sub(cap);
                if self.cursor < oldest {
                    let skipped = oldest - self.cursor;
                    self.cursor = oldest;
                    return Err(Overrun::Overrun { skipped });
                }
                continue;
            }
            let val = unsafe {
                // Prevent the compiler from reordering the non-atomic read
                // before the Acquire load of seq (or after the re-check).
                std::sync::atomic::compiler_fence(Ordering::Acquire);
                let v = *slot.val.get();
                std::sync::atomic::compiler_fence(Ordering::Acquire);
                v
            };
            // Re-check: if the slot moved while we copied, discard (EVT-7).
            if slot.seq.load(Ordering::Acquire) != self.cursor {
                continue;
            }
            self.cursor += 1;
            return Ok(Some(val));
        }
    }

    /// Next index this consumer will read.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
}
