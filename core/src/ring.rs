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
//! Payload exclusion (audit H-7): the slot sequence alone only prevents a torn
//! value from being *returned*; the non-atomic payload copy itself must also
//! never overlap a producer write to the same address. Each slot therefore
//! carries a `busy` flag used as a tiny spinlock around every payload copy in
//! both directions — the producer waits out an in-flight consumer copy before
//! overwriting a slot, and consumers copy payloads under the same flag. This
//! gives a provable no-concurrent-access window (see the `Slot` SAFETY comment
//! for the happens-before argument) while keeping the public API unchanged.
//!
//! v1 scope: payloads are `Copy` (spec 001 Decisions). This keeps the
//! concurrent overwrite path sound (no drop-in-place of a value another thread
//! may be reading). Non-`Copy` events (book deltas) flow via the owned
//! log/channel path; a zero-copy arena ring is a later optimization.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

const EMPTY: u64 = u64::MAX;

struct Slot<T: Copy> {
    seq: AtomicU64,
    /// Payload-copy exclusion flag (audit H-7). `true` while a thread is
    /// copying the payload in (producer) or out (consumer) of `val`. This is
    /// the only thing that makes the non-atomic access to `val` race-free;
    /// see the SAFETY comment below for the full happens-before argument.
    busy: AtomicBool,
    val: UnsafeCell<MaybeUninit<T>>,
}

impl<T: Copy> Slot<T> {
    #[inline]
    fn lock_busy(&self) {
        while self
            .busy
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
    }

    #[inline]
    fn unlock_busy(&self) {
        self.busy.store(false, Ordering::Release);
    }
}

// SAFETY: `Ring` is only shared through `Arc<Ring<T>>`. One `Producer` writes a
// slot and publishes it with a `Release` store to `seq`; consumers `Acquire`-
// load `seq` before copying `val` and discard the value if the seq changed
// underneath (LMAX generation protocol, EVT-7). `T: Copy` means read = memcpy
// with no destructor racing a producer write. The `MaybeUninit` payload
// permits the initial "never-read-before-publish" state for any `T: Copy`
// without UB (fixes the previous `mem::zeroed()` which was invalid for types
// without an all-zero bit pattern).
//
// SAFETY (audit H-7, payload exclusion): the seq capture/re-check alone only
// prevents a torn value from being *returned*; the non-atomic payload access
// itself must never overlap a producer write to the same address. That is
// guaranteed by the per-slot `busy` spinlock, taken around EVERY payload copy
// in both directions:
//
//   producer push:  seq := EMPTY (Release) → lock busy (Acquire CAS) →
//                   payload write → unlock busy (Release) →
//                   seq := w (Release) → write_pos := w + 1 (Release)
//   consumer read:  seq load == cursor (Acquire) → lock busy (Acquire CAS) →
//                   payload read → seq re-check (Acquire) → unlock busy
//                   (Release)
//
// 1. Publish → read: the consumer reaches its payload read only after its
//    Acquire load of `seq` has read the value `w` from the producer's Release
//    store of `seq`, which is sequenced *after* the producer's payload write.
//    That Release store / Acquire load pair on `seq` makes the payload write
//    happen-before every consumer payload read of that generation.
// 2. Read → overwrite: the producer's payload write is sequenced between its
//    Acquire CAS on `busy` and its Release store on `busy`; the consumer's
//    payload read is sequenced between its Acquire CAS on `busy` and its
//    Release store on `busy`. The CAS/store pair is a correct spinlock, so the
//    two payload accesses are serialized: when the producer's CAS reads-from
//    the consumer's unlock (or vice versa), the Acquire establishes that the
//    earlier thread's payload access happens-before the later thread's. Hence
//    a consumer payload read and a producer payload write can NEVER execute
//    concurrently on the same slot — there is provably no concurrent
//    unsynchronized access window, and the non-atomic payload access is sound
//    (no data race, no UB, no torn value even transiently).
// 3. The post-read seq re-check (still inside the `busy` critical section)
//    discards a value whose generation advanced while the consumer was
//    waiting for `busy`; when the producer claimed the slot first, the
//    consumer's re-check necessarily observes EMPTY or the newer generation
//    (the producer's EMPTY store happens-before the consumer's re-check via
//    the `busy` handoff), so a wrong-generation value is never returned.
// 4. Liveness: `busy` is never held while waiting on anything else (hold time
//    is exactly one payload memcpy), so the spinlock cannot deadlock; the
//    producer's `push` may briefly spin while a slow consumer finishes copying
//    the slot it is about to overwrite — bounded by one memcpy, not by the
//    consumer's overall progress.

/// Shared broadcast ring. Construct with [`Ring::with_capacity`], then take one
/// [`Producer`] and any number of [`Consumer`]s.
pub struct Ring<T: Copy> {
    mask: u64,
    slots: Box<[Slot<T>]>,
    /// Next index the producer will write (also the count of items ever pushed).
    write_pos: AtomicU64,
}

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
                // Never read before the producer publishes a real value via
                // seq, so no initialized `T` is materialized at construction.
                busy: AtomicBool::new(false),
                val: UnsafeCell::new(MaybeUninit::uninit()),
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
        // H-7: wait out any consumer still copying the previous generation of
        // this slot before overwriting the payload (see Slot SAFETY §2).
        slot.lock_busy();
        unsafe {
            slot.val.get().write(MaybeUninit::new(v));
        }
        slot.unlock_busy();
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
            // H-7: hold the slot's payload-copy lock across the payload read
            // so the producer cannot overwrite it concurrently (Slot SAFETY
            // §2). This lock, not the seq re-check, is what makes the
            // non-atomic read race-free; the re-check below only discards a
            // value whose generation advanced while we waited for the lock.
            slot.lock_busy();
            let val = unsafe {
                // SAFETY: seq1 == cursor (checked above) means the producer has
                // published this slot, so the payload was initialized, and the
                // seq Release store is sequenced after the payload write (Slot
                // SAFETY §1). We hold `busy`, so no producer write can overlap
                // this memcpy; assume_init reads it as T: Copy.
                (*slot.val.get()).assume_init()
            };
            // Re-check: if the slot's generation advanced while we waited for
            // the lock, the payload we read is the wrong generation — discard
            // rather than return a duplicate/skipped value (EVT-7).
            if slot.seq.load(Ordering::Acquire) != self.cursor {
                slot.unlock_busy();
                continue;
            }
            slot.unlock_busy();
            self.cursor += 1;
            return Ok(Some(val));
        }
    }

    /// Next index this consumer will read.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H-7 stress: two threads hammering push/try_recv. The producer pushes
    /// its own write index as the payload, so any torn/duplicated/skipped
    /// payload copy would show up as a non-exact or non-monotonic value at the
    /// consumer. Exercises the overwrite path (small capacity, consumer falls
    /// behind), the busy-lock contention path, and overrun resync.
    #[test]
    fn regression_audit28_h7_ring_stress_push_try_recv_two_threads() {
        const N: u64 = 100_000;
        let ring = Ring::<u64>::with_capacity(16);
        let mut p = ring.producer();
        let mut c = ring.consumer();
        let done = Arc::new(AtomicBool::new(false));

        let done_w = done.clone();
        let producer = std::thread::spawn(move || {
            for i in 0..N {
                p.push(i);
            }
            done_w.store(true, Ordering::Release);
        });

        let mut received: u64 = 0;
        let mut skipped: u64 = 0;
        let mut last: Option<u64> = None;
        loop {
            let cur = c.cursor();
            match c.try_recv() {
                Ok(Some(v)) => {
                    // The payload of index `cur` must be exactly `cur`: proves
                    // the consumer read the value published for the generation
                    // it claimed — no torn copy, no wrong-generation value.
                    assert_eq!(v, cur, "payload/index mismatch at cursor {cur}");
                    if let Some(l) = last {
                        assert!(v > l, "non-monotonic read: {v} after {l}");
                    }
                    last = Some(v);
                    received += 1;
                }
                Ok(None) => {
                    if done.load(Ordering::Acquire) && c.cursor() >= N {
                        break;
                    }
                    std::hint::spin_loop();
                }
                Err(Overrun::Overrun { skipped: s }) => skipped += s,
            }
        }
        producer.join().unwrap();
        // Every pushed item is either received or accounted for by an overrun.
        assert_eq!(received + skipped, N);
        assert_eq!(c.cursor(), N);
    }

    /// H-7 stress, two concurrent consumers: both must observe exact,
    /// strictly-increasing payload/index pairs from the same ring while the
    /// producer overwrites slots — the second consumer additionally forces
    /// cross-consumer `busy` contention on the same slots.
    #[test]
    fn regression_audit28_h7_ring_stress_two_consumers() {
        const N: u64 = 50_000;
        let ring = Ring::<u64>::with_capacity(8);
        let mut p = ring.producer();
        let mut c1 = ring.consumer();
        let mut c2 = ring.consumer();
        let done = Arc::new(AtomicBool::new(false));

        let done_w = done.clone();
        let producer = std::thread::spawn(move || {
            for i in 0..N {
                p.push(i);
            }
            done_w.store(true, Ordering::Release);
        });

        fn drain(done: &AtomicBool, c: &mut Consumer<u64>) -> (u64, u64) {
            const N: u64 = 50_000;
            let mut received = 0u64;
            let mut skipped = 0u64;
            let mut last: Option<u64> = None;
            loop {
                let cur = c.cursor();
                match c.try_recv() {
                    Ok(Some(v)) => {
                        assert_eq!(v, cur, "payload/index mismatch at cursor {cur}");
                        if let Some(l) = last {
                            assert!(v > l, "non-monotonic read: {v} after {l}");
                        }
                        last = Some(v);
                        received += 1;
                    }
                    Ok(None) => {
                        if done.load(Ordering::Acquire) && c.cursor() >= N {
                            break;
                        }
                        std::hint::spin_loop();
                    }
                    Err(Overrun::Overrun { skipped: s }) => skipped += s,
                }
            }
            (received, skipped)
        }

        let done2 = done.clone();
        let h2 = std::thread::spawn(move || {
            let (received, skipped) = drain(&done2, &mut c2);
            (received, skipped, c2.cursor())
        });

        let (r1, s1) = drain(&done, &mut c1);
        let (r2, s2, cur2) = h2.join().unwrap();
        producer.join().unwrap();
        assert_eq!(r1 + s1, N);
        assert_eq!(c1.cursor(), N);
        assert_eq!(r2 + s2, N);
        assert_eq!(cur2, N);
    }
}
