//! Compactor (STO-1): event logs → partitioned trades Parquet + quality
//! manifest, idempotently. Re-running is a no-op when the source hash already
//! matches the written footer (STO-8). v1 writes the trades stream to Parquet;
//! other streams' Parquet is the same pattern (deferred — see spec Decisions),
//! while the manifest already covers *all* streams.

use crate::audit::RawLogAudit;
use crate::manifest::{self, QualityManifest};
use crate::{
    layout, parquet_macro, parquet_options, parquet_positions, parquet_trades, StorageError,
};
use mp_core::{EventEnvelope, MarketEvent, SymbolTable, Venue};
use std::collections::BTreeMap;
use std::path::Path;

/// Outcome of a compaction run. `trades_*` are legacy field names (spec 003
/// v1); specs 028/030/031 streams carry their own per-stream counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactStats {
    pub trades_files_written: u64,
    pub trades_files_skipped: u64,
    pub trade_rows: u64,
    /// Spec 028 (WHL-6): `cold/positions/` census files.
    pub positions_files_written: u64,
    pub positions_files_skipped: u64,
    pub position_rows: u64,
    /// Spec 030 (MAC-6): `cold/macro/` FRED files.
    pub macro_files_written: u64,
    pub macro_files_skipped: u64,
    pub macro_rows: u64,
    /// Spec 031 (OPT-5): `cold/options/` Deribit files.
    pub options_files_written: u64,
    pub options_files_skipped: u64,
    pub option_rows: u64,
}

/// Compact one venue/day's events, but only when the raw log's audit is clean
/// (INT-4).  A quarantined log never reaches cold storage: refusal is a hard
/// error, and no Parquet or manifest is written.
#[allow(clippy::too_many_arguments)]
pub fn compact_day_verified(
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
    audit: &RawLogAudit,
) -> Result<CompactStats, StorageError> {
    if !audit.is_clean() {
        let codes: Vec<&str> = audit.findings.iter().map(|f| f.code.as_str()).collect();
        return Err(StorageError::Refused(format!(
            "audit not clean for {}/{}: {}",
            layout::venue_slug(venue),
            date,
            codes.join(", ")
        )));
    }
    compact_day(
        root,
        venue,
        date,
        day_start_ns,
        day_end_ns,
        events,
        symbols,
        source_hash,
        compactor_version,
        created_ts_ns,
    )
}

/// Compact one venue/day's events. `events` must be the full day for `venue`,
/// in recv order. `hash` is a content hash of the source logs (STO-1/8).
/// Every Parquet-backed stream (trades + specs 028/030/031 positions/macro/
/// options) is written to its own `cold/<stream>/` partition (W-6); the
/// manifest covers ALL streams including the manifest-only ones.
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

    // LAB-5: count trade events before compaction so we can verify zero-row
    // writes against a non-zero input. A live HL day with `n_trades > 0` and
    // parquet_rows == 0 is a bug, not success.
    let n_trade_events = events
        .iter()
        .filter(|e| matches!(e.body, MarketEvent::Trade { .. }))
        .count();

    // Group Parquet-backed streams by (stream, symbol). BTreeMap ⇒
    // deterministic write order (CONV-10).
    let mut by_stream: BTreeMap<(&'static str, u32), Vec<EventEnvelope>> = BTreeMap::new();
    for e in &events {
        let stream = layout::stream_type_name(&e.body);
        if layout::has_parquet_partition(stream) {
            by_stream
                .entry((stream, e.symbol.0))
                .or_default()
                .push(e.clone());
        }
    }

    for ((stream, sym_id), mut evs) in by_stream {
        let name = symbols
            .get(mp_core::SymbolId(sym_id))
            .map(|m| m.venue_symbol.clone())
            .unwrap_or_else(|| format!("sym{sym_id}"));
        let path = layout::partition_file(root, stream, venue, &name, date);

        // Idempotency: skip if the existing file was built from the same source.
        if path.exists() {
            if let Ok(Some(existing)) = parquet_trades::read_source_hash(&path) {
                if existing == source_hash {
                    match stream {
                        "trades" => stats.trades_files_skipped += 1,
                        "positions" => stats.positions_files_skipped += 1,
                        "macro" => stats.macro_files_skipped += 1,
                        "options" => stats.options_files_skipped += 1,
                        _ => {}
                    }
                    continue;
                }
            }
        }
        evs.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
        let rows = match stream {
            "trades" => parquet_trades::write_trades(&path, &evs, compactor_version, source_hash)?,
            "positions" => {
                parquet_positions::write_positions(&path, &evs, compactor_version, source_hash)?
            }
            "macro" => parquet_macro::write_macro(&path, &evs, compactor_version, source_hash)?,
            "options" => {
                parquet_options::write_options(&path, &evs, compactor_version, source_hash)?
            }
            other => {
                return Err(StorageError::Refused(format!(
                    "stream {other} has no parquet writer"
                )))
            }
        };
        match stream {
            "trades" => {
                stats.trades_files_written += 1;
                stats.trade_rows += rows;
            }
            "positions" => {
                stats.positions_files_written += 1;
                stats.position_rows += rows;
            }
            "macro" => {
                stats.macro_files_written += 1;
                stats.macro_rows += rows;
            }
            "options" => {
                stats.options_files_written += 1;
                stats.option_rows += rows;
            }
            _ => {}
        }
    }

    // LAB-5: if the raw log decoded trade events but compaction wrote a
    // NEW trade parquet file with zero rows, refuse — "1 file written
    // (0 rows)" on a live day is a bug, not success. Idempotent re-runs
    // (trades_files_skipped > 0, nothing written) are exempt.
    if n_trade_events > 0 && stats.trade_rows == 0 && stats.trades_files_written > 0 {
        return Err(StorageError::Refused(format!(
            "LAB-5: {}/{}: raw log decoded {n_trade_events} trade(s) but \
             compact wrote 0 trade rows — refusing zero-row parquet on non-empty raw",
            layout::venue_slug(venue), date
        )));
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
