//! Compactor (STO-1): event logs → partitioned trades Parquet + quality
//! manifest, idempotently. Re-running is a no-op when the source hash already
//! matches the written footer (STO-8). v1 writes the trades stream to Parquet;
//! other streams' Parquet is the same pattern (deferred — see spec Decisions),
//! while the manifest already covers *all* streams.

use crate::manifest::{self, QualityManifest};
use crate::{layout, parquet_trades, StorageError};
use mp_core::{EventEnvelope, MarketEvent, SymbolTable, Venue};
use std::collections::BTreeMap;
use std::path::Path;

/// Outcome of a compaction run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactStats {
    pub trades_files_written: u64,
    pub trades_files_skipped: u64,
    pub trade_rows: u64,
}

/// Compact one venue/day's events. `events` must be the full day for `venue`,
/// in recv order. `hash` is a content hash of the source logs (STO-1/8).
#[allow(clippy::too_many_arguments)]
pub fn compact_day(
    root: &Path,
    venue: Venue,
    date: &str,
    day_start_ns: i64,
    day_end_ns: i64,
    events: Vec<EventEnvelope>,
    symbols: &SymbolTable,
    source_hash: &str,
    compactor_version: &str,
    created_ts_ns: i64,
) -> Result<CompactStats, StorageError> {
    let mut stats = CompactStats::default();

    // Group trades by symbol.
    let mut by_symbol: BTreeMap<u32, Vec<EventEnvelope>> = BTreeMap::new();
    for e in &events {
        if matches!(e.body, MarketEvent::Trade { .. }) {
            by_symbol.entry(e.symbol.0).or_default().push(e.clone());
        }
    }

    for (sym_id, mut trades) in by_symbol {
        let name = symbols
            .get(mp_core::SymbolId(sym_id))
            .map(|m| m.venue_symbol.clone())
            .unwrap_or_else(|| format!("sym{sym_id}"));
        let path = layout::partition_file(root, "trades", venue, &name, date);

        // Idempotency: skip if the existing file was built from the same source.
        if path.exists() {
            if let Ok(Some(existing)) = parquet_trades::read_source_hash(&path) {
                if existing == source_hash {
                    stats.trades_files_skipped += 1;
                    continue;
                }
            }
        }
        trades.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
        let rows = parquet_trades::write_trades(&path, &trades, compactor_version, source_hash)?;
        stats.trades_files_written += 1;
        stats.trade_rows += rows;
    }

    // Manifest for ALL streams (STO-2).
    let m = manifest::derive_manifest(
        layout::venue_slug(venue),
        date,
        day_start_ns,
        day_end_ns,
        events.into_iter(),
        |id| {
            symbols
                .get(id)
                .map(|meta| meta.venue_symbol.clone())
                .unwrap_or_else(|| format!("sym{}", id.0))
        },
        compactor_version,
        created_ts_ns,
    );
    let mpath = layout::manifest_file(root, venue, date);
    manifest::write_manifest(&mpath, &m)?;

    Ok(stats)
}

/// Load the manifest written for a venue/day.
pub fn load_manifest(
    root: &Path,
    venue: Venue,
    date: &str,
) -> Result<QualityManifest, StorageError> {
    let mpath = layout::manifest_file(root, venue, date);
    manifest::read_manifest(&mpath).map_err(StorageError::Io)
}
