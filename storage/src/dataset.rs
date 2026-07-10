//! Dataset reader (STO-4, STO-5). Streams recorded trades in global
//! `(recv_ts_ns, stream_seq)` order and answers coverage/gaps from manifests
//! WITHOUT scanning data files — the honesty gate sim/research consult first.

use crate::{compactor, layout, parquet_trades, StorageError};
use mp_core::{EventEnvelope, Venue};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
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
        Ok(kway_merge(sources))
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

struct HeapItem {
    key: (i64, u64),
    src: usize,
    ev: EventEnvelope,
}
impl PartialEq for HeapItem {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key && self.src == o.src
    }
}
impl Eq for HeapItem {}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for HeapItem {
    fn cmp(&self, o: &Self) -> Ordering {
        o.key.cmp(&self.key).then(o.src.cmp(&self.src)) // min-heap
    }
}

fn kway_merge(mut sources: Vec<std::vec::IntoIter<EventEnvelope>>) -> Vec<EventEnvelope> {
    let mut heap = BinaryHeap::new();
    for (src, it) in sources.iter_mut().enumerate() {
        if let Some(ev) = it.next() {
            let key = ev.merge_key();
            heap.push(HeapItem { key, src, ev });
        }
    }
    let mut out = Vec::new();
    while let Some(item) = heap.pop() {
        if let Some(ev) = sources[item.src].next() {
            let key = ev.merge_key();
            heap.push(HeapItem {
                key,
                src: item.src,
                ev,
            });
        }
        out.push(item.ev);
    }
    out
}
