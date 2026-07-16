//! Dataset reader (STO-4, STO-5). Streams recorded trades in global
//! `(recv_ts_ns, stream_seq)` order and answers coverage/gaps from manifests
//! WITHOUT scanning data files — the honesty gate sim/research consult first.

use crate::{compactor, layout, parquet_trades, StorageError};
use mp_core::{merge_sorted_events, EventEnvelope, Venue};
use std::path::{Path, PathBuf};

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
        let path = layout::partition_file(&self.root, "trades", venue, symbol, date);
        if !path.exists() {
            return Ok(Vec::new());
        }
        parquet_trades::read_trades(&path)
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

