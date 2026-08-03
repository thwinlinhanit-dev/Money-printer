//! Shared order-book sequence tracker (COL-7). Each venue maps its own update
//! ids onto this: a snapshot resets it; a delta is applied only if contiguous,
//! otherwise it signals a gap and desyncs until the next snapshot.

/// Why a snapshot was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapKind {
    Init,
    Resync,
}

/// What to do with a delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaAction {
    Apply,
    /// Sequence gap — emit `Status::GapDetected`, drop until next snapshot.
    Gap,
    /// Stale/duplicate or not-yet-initialized — drop silently.
    Drop,
}

/// Per-symbol book sequence state.
#[derive(Debug, Clone, Copy, Default)]
pub struct BookSync {
    last: u64,
    init: bool,
    desynced: bool,
}

impl BookSync {
    pub fn on_snapshot(&mut self, seq: u64) -> SnapKind {
        let was = self.init || self.desynced;
        self.last = seq;
        self.init = true;
        self.desynced = false;
        if was {
            SnapKind::Resync
        } else {
            SnapKind::Init
        }
    }

    /// `first`/`last` are the delta's inclusive sequence range. Venues that give
    /// a single update id pass it as both.
    pub fn on_delta(&mut self, first: u64, last: u64) -> DeltaAction {
        if !self.init || self.desynced {
            return DeltaAction::Drop;
        }
        if last <= self.last {
            return DeltaAction::Drop; // stale/duplicate
        }
        if first > self.last + 1 {
            self.desynced = true;
            return DeltaAction::Gap;
        }
        self.last = last;
        DeltaAction::Apply
    }

    /// Force desync (e.g. on reconnect).
    pub fn desync(&mut self) {
        self.desynced = true;
    }

    pub fn expected_next(&self) -> u64 {
        self.last + 1
    }

    /// True when the book is desynced and the driver must re-seed from a
    /// fresh REST snapshot (spec 020 / COL-24). The binary polls this each
    /// loop iteration and acts immediately (no sleeping inline).
    pub fn needs_reseed(&self) -> bool {
        self.init && self.desynced
    }

    /// Wire the documented Binance depth-continuity rule (spec 020):
    /// returns `DeltaAction::Apply` iff the delta's `u` is contiguous with
    /// the current state (`u == expected_next()`), else `Gap` (and desyncs).
    /// The range-and-`pu` version lives on [`BinanceBookSync`].
    pub fn on_delta_with_pu(&mut self, first: u64, last: u64) -> DeltaAction {
        if !self.init || self.desynced {
            return DeltaAction::Drop;
        }
        if last != self.last + 1 {
            self.desynced = true;
            return DeltaAction::Gap;
        }
        let _ = first;
        self.last = last;
        DeltaAction::Apply
    }
}

/// Binance `depthUpdate` continuity state (COL-23/24, spec 020). This is the
/// documented algorithm, not the generic next+1 check other venues use:
///
/// 1. After a snapshot (`seed`), deltas are buffered/dropped while their
///    final update id `u` is `<= snapshot_lastUpdateId` (already covered).
/// 2. The first delta accepted after the snapshot must **straddle** it:
///    `U <= snapshot_lastUpdateId <= u`.
/// 3. Every subsequent delta must satisfy `pu == prev_u`.
///
/// Any mismatch in (2)/(3) desyncs and asks the driver to re-seed from REST.
#[derive(Debug, Clone, Copy, Default)]
pub struct BinanceBookSync {
    /// `u` of the last applied delta, or the snapshot `lastUpdateId`.
    prev_u: u64,
    /// Snapshot `lastUpdateId`; waiting for the first straddling delta.
    snapshot_last: u64,
    seeded: bool,
    desynced: bool,
}

impl BinanceBookSync {
    /// Seed from a REST snapshot; deltas until the straddle are dropped.
    pub fn seed(&mut self, snapshot_last_update_id: u64) {
        self.snapshot_last = snapshot_last_update_id;
        self.prev_u = snapshot_last_update_id;
        self.seeded = true;
        self.desynced = false;
    }

    /// One `depthUpdate`: `(U, u, pu)`. See struct docs for the rule.
    pub fn on_delta(&mut self, first_u: u64, last_u: u64, pu: u64) -> DeltaAction {
        if !self.seeded || self.desynced {
            return DeltaAction::Drop;
        }
        if last_u <= self.snapshot_last {
            return DeltaAction::Drop; // fully covered by the snapshot
        }
        if self.prev_u == self.snapshot_last {
            // First delta after the snapshot must straddle it.
            if first_u > self.snapshot_last {
                self.desynced = true;
                return DeltaAction::Gap;
            }
        } else if pu != self.prev_u {
            self.desynced = true;
            return DeltaAction::Gap;
        }
        self.prev_u = last_u;
        DeltaAction::Apply
    }

    pub fn needs_reseed(&self) -> bool {
        self.seeded && self.desynced
    }

    pub fn desync(&mut self) {
        self.desynced = true;
    }
}
