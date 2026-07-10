//! The normalization contract (COL-5..8). A venue adapter turns raw frames
//! into normalized [`EventEnvelope`]s, stamping `recv_ts_ns` (COL-5) and
//! emitting `Status::GapDetected` on a book sequence gap (COL-7).

use mp_core::{EventEnvelope, Venue};

/// Normalization outcome for one raw frame.
#[derive(Debug, thiserror::Error)]
pub enum NormError {
    #[error("parse: {0}")]
    Parse(String),
}

/// A stateful, per-venue normalizer. Holds book-sync state and the symbol
/// table for its venue.
pub trait Normalizer {
    fn venue(&self) -> Venue;

    /// Normalize one frame, pushing zero or more events into `out`. Unknown
    /// topics are ignored (`Ok`); malformed frames return [`NormError::Parse`]
    /// (COL-6 — caller counts and continues).
    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError>;

    /// Called on reconnect: book state is untrusted until a fresh snapshot, so
    /// mark all books desynced (COL-7).
    fn reset_books(&mut self);
}

/// Per-collector health/metrics counters (COL-10/11 data; HTTP export deferred
/// with the live transport).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthCounters {
    pub events_emitted: u64,
    pub messages_dropped: u64,
    pub gaps_detected: u64,
    pub reconnects: u64,
    pub book_resyncs: u64,
}
