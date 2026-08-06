//! Dataset reader (STO-4, STO-5). Streams recorded trades in global
//! `(recv_ts_ns, stream_seq)` order and answers coverage/gaps from manifests
//! WITHOUT scanning data files — the honesty gate sim/research consult first.

use crate::{compactor, layout, parquet_macro, parquet_options, parquet_positions, StorageError};
use mp_core::{merge_sorted_events, EventEnvelope, Venue};
use std::path::{Path, PathBuf};

/// Helper passed to [`Dataset::day`] for the trades reader (avoids a cycle in
/// module paths — trades lives in `parquet_trades`).
fn parquet_trades_day(path: &Path) -> Result<Vec<EventEnvelope>, StorageError> {
    crate::parquet_trades::read_trades(path)
}

/// A read-only view over the cold store rooted at `root`.
pub struct Dataset {
    root: PathBuf,
}

impl Dataset {
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read all trades for `(venue, symbol, date)`, sorted by recv order.
    pub fn trades_day(
        &self,
        venue: Venue,
        symbol: &str,
        date: &str,
    ) -> Result<Vec<EventEnvelope>, StorageError> {
        self.day("trades", venue, symbol, date, parquet_trades_day)
    }

    /// Spec 028 (WHL-6): whale positions for `(venue, symbol, date)`.
    pub fn positions_day(
        &self,
        venue: Venue,
        symbol: &str,
        date: &str,
    ) -> Result<Vec<EventEnvelope>, StorageError> {
        self.day("positions", venue, symbol, date, |p| {
            parquet_positions::read_positions(p)
        })
    }

    /// Spec 030 (MAC-6): FRED macro points for `(venue, series, date)`.
    pub fn macro_day(
        &self,
        venue: Venue,
        symbol: &str,
        date: &str,
    ) -> Result<Vec<EventEnvelope>, StorageError> {
        self.day("macro", venue, symbol, date, |p| {
            parquet_macro::read_macro(p)
        })
    }

    /// Spec 031 (OPT-5): Deribit options for `(venue, symbol, date)`.
    pub fn options_day(
        &self,
        venue: Venue,
        symbol: &str,
        date: &str,
    ) -> Result<Vec<EventEnvelope>, StorageError> {
        self.day("options", venue, symbol, date, |p| {
            parquet_options::read_options(p)
        })
    }

    /// Generic day read for a Parquet-backed stream; empty when no file.
    fn day(
        &self,
        stream: &str,
        venue: Venue,
        symbol: &str,
        date: &str,
        read: impl FnOnce(&std::path::Path) -> Result<Vec<EventEnvelope>, StorageError>,
    ) -> Result<Vec<EventEnvelope>, StorageError> {
        let path = layout::partition_file(&self.root, stream, venue, symbol, date);
        if !path.exists() {
            return Ok(Vec::new());
        }
        read(&path)
    }

    /// Read trades across several `(venue, symbol, date)` partitions, merged into
    /// one globally ordered stream by `(recv_ts_ns, stream_seq)` (STO-4).
    pub fn trades_merged(
        &self,
        parts: &[(Venue, &str, &str)],
    ) -> Result<Vec<EventEnvelope>, StorageError> {
        let mut sources: Vec<std::vec::IntoIter<EventEnvelope>> = Vec::new();
        for (v, s, d) in parts {
            sources.push(self.trades_day(*v, s, d)?.into_iter());
        }
        // Use the shared k-way merge from mp_core (EVT-5 / STO-4) rather than
        // a private copy — one merge impl, one tie-break policy (Major #6).
        Ok(merge_sorted_events(sources))
    }

    /// Coverage of a stream for a venue/day, read from the manifest only (STO-5).
    pub fn coverage(
        &self,
        venue: Venue,
        date: &str,
        stream_key: &str,
    ) -> Result<Option<f64>, StorageError> {
        let m = compactor::load_manifest(&self.root, venue, date)?;
        Ok(m.coverage(stream_key))
    }

    /// Gaps of a stream for a venue/day, from the manifest only (STO-5).
    pub fn gaps(
        &self,
        venue: Venue,
        date: &str,
        stream_key: &str,
    ) -> Result<Vec<crate::manifest::Gap>, StorageError> {
        let m = compactor::load_manifest(&self.root, venue, date)?;
        Ok(m.gaps(stream_key).to_vec())
    }
}
