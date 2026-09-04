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
use mp_core::{EventEnvelope, SymbolTable, Venue};
use std::collections::BTreeMap;
use std::path::Path;

/// Options controlling a compaction run.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompactOptions {
    /// Rewrite Parquet files even when the existing footer's
    /// `source_log_hash` already matches the current raw log. The default
    /// (false) keeps the STO-8 idempotency skip; `force: true` is the repair
    /// path (B-5, audit 2026-09-03) — rebuilds hollow/stale files written by
    /// a buggy writer without deleting them first, and is safe because the
    /// writer is a pure function of (events, version, hash).
    pub force: bool,
}

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
    compact_day_verified_opts(
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
        audit,
        CompactOptions::default(),
    )
}

/// [`compact_day_verified`] with explicit [`CompactOptions`] (B-5 repair
/// path: `force` rebuilds a day whose Parquet already carries the matching
/// source hash — e.g. hollow files written by the schema-4 bug).
#[allow(clippy::too_many_arguments)]
pub fn compact_day_verified_opts(
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
    opts: CompactOptions,
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
    compact_day_opts(
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
        opts,
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
    compact_day_opts(
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
        CompactOptions::default(),
    )
}

/// [`compact_day`] with explicit [`CompactOptions`] (B-5 repair path: see
/// [`CompactOptions::force`]).
#[allow(clippy::too_many_arguments)]
pub fn compact_day_opts(
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
    opts: CompactOptions,
) -> Result<CompactStats, StorageError> {
    let mut stats = CompactStats::default();

    // LAB-5: count trade events before compaction so we can verify zero-row
    // writes against a non-zero input. A live HL day with `n_trades > 0` and
    // parquet_rows == 0 is a bug, not success. Schema-agnostic: schema-4
    // hyperliquid trades decode as `TradeWithAddr`, so the canary must count
    // `trade_view()` events, not only the schema-3 variant (B-4, audit
    // 2026-09-03 — the Trade-only count is exactly why the hollow days
    // shipped without tripping LAB-5).
    let n_trade_events = events
        .iter()
        .filter(|e| e.body.trade_view().is_some())
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
        // `force` (B-5 repair) overrides the skip so hollow/stale files are
        // rebuilt from the current raw bytes.
        if path.exists() {
            if let Ok(Some(existing)) = parquet_trades::read_source_hash(&path) {
                if existing == source_hash && !opts.force {
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
    let mut m = manifest::derive_manifest(
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
    // Merge, don't overwrite: `layout::manifest_file` is per venue/date with
    // NO symbol component, but compaction runs per symbol (one raw log per
    // symbol). An unconditional overwrite would drop every stream of the
    // earlier-compacted symbol from the day's manifest — silently starving
    // STO-2 coverage/gap reads of that symbol and, worse, letting the A-1
    // prune gate verify one symbol's raw log against another symbol's proof
    // (B-2, audit 2026-09-03). This run's streams win on key collision (the
    // freshest stats for the same source bytes); streams we did not see this
    // run are preserved.
    if let Ok(existing) = manifest::read_manifest(&mpath) {
        if existing.venue == m.venue && existing.date == m.date {
            for (key, stats) in existing.streams {
                m.streams.entry(key).or_insert(stats);
            }
        }
    }
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
