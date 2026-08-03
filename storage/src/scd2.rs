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

/// Error returned by [`SymbolScd2::append`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Scd2AppendError {
    /// `valid_from_ns` is before the current version's start for the key:
    /// appending here would produce a negative-length closed interval.
    #[error(
        "out-of-order append for {venue_symbol}: new valid_from_ns {new_valid_from_ns} < prior {prior_valid_from_ns}"
    )]
    OutOfOrder {
        venue_symbol: String,
        prior_valid_from_ns: i64,
        new_valid_from_ns: i64,
    },
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
    ///
    /// Returns `Err` and records nothing when `valid_from_ns` is strictly
    /// before the current version's `valid_from_ns`: appending there would
    /// produce a negative-length closed interval on the prior row (invalid
    /// history). Backfills must arrive in `valid_from_ns` order or be rejected.
    pub fn append(&mut self, mut v: SymbolVersion) -> Result<(), Scd2AppendError> {
        if v.valid_to_ns == 0 {
            v.valid_to_ns = i64::MAX;
        }
        let mut open_idx = None;
        for (i, prev) in self.versions.iter().enumerate().rev() {
            if prev.venue == v.venue && prev.venue_symbol == v.venue_symbol {
                if prev.valid_to_ns != i64::MAX || v.valid_from_ns < prev.valid_from_ns {
                    return Err(Scd2AppendError::OutOfOrder {
                        venue_symbol: prev.venue_symbol.clone(),
                        prior_valid_from_ns: prev.valid_from_ns,
                        new_valid_from_ns: v.valid_from_ns,
                    });
                }
                open_idx = Some(i);
                break;
            }
        }
        if let Some(i) = open_idx {
            self.versions[i].valid_to_ns = v.valid_from_ns;
        }
        self.versions.push(v);
        Ok(())
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
