//! Slowly-changing symbol metadata (STO-9). Tick sizes change and instruments
//! delist; historical reads must resolve metadata *as of* the event time, not
//! as it is today.

use mp_core::{InstrumentKind, Venue};
use serde::{Deserialize, Serialize};

/// One versioned metadata row, valid over `[valid_from_ns, valid_to_ns)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolVersion {
    pub venue: Venue,
    pub venue_symbol: String,
    pub kind: InstrumentKind,
    pub tick_size: f64,
    pub step_size: f64,
    pub min_notional: f64,
    pub valid_from_ns: i64,
    /// `i64::MAX` for the currently-open version.
    pub valid_to_ns: i64,
}

/// An append-only SCD2 store keyed by `(venue, venue_symbol)`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SymbolScd2 {
    versions: Vec<SymbolVersion>,
}

impl SymbolScd2 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a new version, closing the previous open version for the same key
    /// at `valid_from_ns` (W-6: we add rows, never rewrite history).
    pub fn append(&mut self, mut v: SymbolVersion) {
        if v.valid_to_ns == 0 {
            v.valid_to_ns = i64::MAX;
        }
        for prev in self.versions.iter_mut().rev() {
            if prev.venue == v.venue
                && prev.venue_symbol == v.venue_symbol
                && prev.valid_to_ns == i64::MAX
            {
                prev.valid_to_ns = v.valid_from_ns;
                break;
            }
        }
        self.versions.push(v);
    }

    /// Resolve metadata for `(venue, symbol)` as of `ts_ns` (STO-9).
    pub fn as_of(&self, venue: Venue, symbol: &str, ts_ns: i64) -> Option<&SymbolVersion> {
        self.versions.iter().find(|v| {
            v.venue == venue
                && v.venue_symbol == symbol
                && ts_ns >= v.valid_from_ns
                && ts_ns < v.valid_to_ns
        })
    }

    pub fn versions(&self) -> &[SymbolVersion] {
        &self.versions
    }
}
