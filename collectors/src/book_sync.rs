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
}
