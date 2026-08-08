//! Offline feature materialization (spec 016 / POST_CLEAN_PLAN Phase 2).
//!
//! Runs the feature engine (mp-features, the SAME code a live runner uses —
//! FEA-4 one-code-path) over recorded event logs and writes every
//! [`FeatureUpdate`] to the FeatureStore Parquet layout
//! `{root}/{feature}/ver=N/venue={v}/symbol={s}/{date}.parquet` with the FEA-6
//! footer metadata (feature ver, engine git sha, params hash, symbols hash). A
//! changed params hash allocates a new `ver=N` directory (recorded data is
//! never overwritten, W-6); `resolve_version` makes re-materialization
//! idempotent.
//!
//! Determinism (MAT-5): the caller's log ORDER is ignored — paths are sorted
//! canonically and exact duplicates removed before anything is read, so
//! `(venue, symbol)` → [`SymbolId`] assignment, the k-way merge order and the
//! output bytes depend only on the input SET, never on argument order.
//! Log-local symbol ids are re-interned into ONE shared [`SymbolTable`] (ids
//! collide across logs — each log rebuilds its own table, EVT-8), all logs are
//! k-way merged by `(recv_ts_ns, stream_seq)` (EVT-5; ties break by source
//! index, which is canonical after sorting), and rows are sorted before
//! writing — identical inputs produce byte-identical Parquet.
//!
//! The shared symbol table is NOT self-describing (rows carry numeric ids
//! only), so every run persists an immutable, content-addressed snapshot at
//! `{root}/symbols/{hash}.json` (id → venue/venue_symbol rows in id order) and
//! records the hash in every Parquet footer (`symbols_hash`, FEA-6) — a
//! consumer resolves `symbol_id` by reading the snapshot the footer points at.
//! Re-runs over the same logs produce the same snapshot bytes (W-6 no-overwrite).
//!
//! RAM guard: every log is loaded into memory for the k-way merge (fine for a
//! day-scale corpus). Before reading anything, the total on-disk size of the
//! logs is checked against `MP_MATERIALIZE_MAX_BYTES` (default
//! [`DEFAULT_MAX_BACKFILL_BYTES`]) and the run fails closed with guidance when
//! exceeded — an accidental multi-week full-set backfill must not OOM the box.
//! Slice per day, or raise the cap deliberately for a known-large run. A
//! streaming merge is future work; until then the guard is the honest boundary.
//!
//! Pure orchestration: no wall clock (PD-3); the only timestamps are event
//! times from the logs.

use crate::feature_store::{date_str, materialize, FeatureMeta, FeatureRow};
use mp_core::{fnv1a_absorb, log::LogReader, SymbolTable, Venue, FNV1A_OFFSET};
use mp_features::{engine_from_config, FeatureEngine, FeaturesConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Default RAM-guard cap on total log bytes for one `materialize_logs` run
/// (≈ 5 full-set days at ~0.4 GiB/symbol/day; the merge holds every event in
/// RAM, and the engine + row buffers multiply that). Override with the
/// `MP_MATERIALIZE_MAX_BYTES` env var.
pub const DEFAULT_MAX_BACKFILL_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Outcome of one materialization run.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterializeStats {
    /// Raw events read across all logs.
    pub events_read: u64,
    /// Feature updates emitted by the engine (incl. bar closes).
    pub updates: u64,
    /// Rows written to Parquet.
    pub rows_written: u64,
    /// Parquet files written.
    pub files_written: u64,
    /// Non-finite feature outputs suppressed by the engine (FEA-5).
    pub nan_suppressed: u64,
    /// Feature names that produced at least one row, sorted.
    pub features: Vec<String>,
    /// Number of distinct (venue, symbol) pairs in the shared table.
    pub symbols: usize,
    /// Content hash of the persisted symbols snapshot
    /// (`{root}/symbols/{hash}.json`) — the id order this run's `symbol_id`s
    /// refer to. Non-empty whenever at least one log was processed.
    pub symbols_hash: String,
}

/// Materialize features from recorded event logs into the FeatureStore at
/// `root`. `logs` may span venues/symbols/days; each log contributes to the
/// shared engine state (symbol ids remapped so BTCUSDT on Binance and BTC on
/// Hyperliquid never collide).
///
/// The RAM guard reads `MP_MATERIALIZE_MAX_BYTES` (default
/// [`DEFAULT_MAX_BACKFILL_BYTES`]); use [`materialize_logs_limited`] to inject
/// a cap explicitly.
///
/// Errors are returned as `String` (cross-crate orchestration: LogReader,
/// config, and StorageError surfaces); callers print and exit non-zero.
pub fn materialize_logs(
    root: &Path,
    cfg: &FeaturesConfig,
    logs: &[PathBuf],
    git_sha: &str,
) -> Result<MaterializeStats, String> {
    let max_bytes = std::env::var("MP_MATERIALIZE_MAX_BYTES")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    materialize_logs_limited(root, cfg, logs, git_sha, max_bytes)
}

/// [`materialize_logs`] with an explicit RAM-guard cap. `max_bytes`:
/// - `None` — the default [`DEFAULT_MAX_BACKFILL_BYTES`] applies;
/// - `Some(n)` — runs with cap `n` (tests inject a tiny cap to prove the
///   guard fires; `Some(u64::MAX)` effectively disables it).
///
/// Logs are canonically sorted and exact-duplicate-removed here (single source
/// of truth — the CLI and any caller inherit order-independence for free), and
/// the guard fires BEFORE any log is opened. An empty log set is an error
/// (fail-closed; a "no-op success" would hide a bad invocation). Dedup is
/// exact `PathBuf` equality — two spellings of the same file (e.g.
/// `a/../b.log` vs `b.log`) still double-read.
pub fn materialize_logs_limited(
    root: &Path,
    cfg: &FeaturesConfig,
    logs: &[PathBuf],
    git_sha: &str,
    max_bytes: Option<u64>,
) -> Result<MaterializeStats, String> {
    // MAT-5 canonical input: sort paths byte-wise and drop exact duplicates
    // (a twice-passed log would otherwise double its events). Symbol interning
    // and the k-way merge then depend only on the input SET, never argument
    // order — `--log a --log b` ≡ `--log b --log a`.
    let mut logs: Vec<PathBuf> = logs.to_vec();
    logs.sort();
    logs.dedup();
    if logs.is_empty() {
        return Err("materialize: log set is empty".into());
    }

    // RAM guard — fail BEFORE loading anything. On-disk bytes is a
    // conservative proxy for the in-memory corpus (bincode logs are
    // uncompressed); the merge + engine + row buffers multiply it severalfold.
    let mut total_bytes = 0u64;
    for path in &logs {
        let len = std::fs::metadata(path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?
            .len();
        total_bytes += len;
    }
    let cap = max_bytes.unwrap_or(DEFAULT_MAX_BACKFILL_BYTES);
    if total_bytes > cap {
        return Err(format!(
            "materialize: input corpus is {total_bytes} bytes (cap {cap}); this run loads every \
             event into RAM and would likely OOM. Slice per day \
             (--log data/raw/<date>_*.log) or raise MP_MATERIALIZE_MAX_BYTES for a deliberate \
             large run (a streaming merge is future work)."
        ));
    }

    // Read every log and remap its local symbol ids onto one shared table.
    let mut symbols = SymbolTable::new();
    let mut sources: Vec<std::vec::IntoIter<mp_core::EventEnvelope>> = Vec::new();
    let mut events_read = 0u64;
    for path in &logs {
        let mut reader =
            LogReader::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut events = Vec::new();
        for ev in &mut reader {
            events.push(ev.map_err(|e| format!("read {}: {e}", path.display()))?);
        }
        events_read += events.len() as u64;
        // Log-local ids → shared ids (EVT-8): same (venue, symbol) is one id
        // everywhere, so cross-log feature state merges correctly.
        let metas = reader.symbols().to_vec();
        for ev in events.iter_mut() {
            let meta = metas.get(ev.symbol.0 as usize).ok_or_else(|| {
                format!(
                    "{}: symbol id {} has no metadata (corrupt log?)",
                    path.display(),
                    ev.symbol.0
                )
            })?;
            let shared = symbols.intern_default(meta.venue, &meta.venue_symbol);
            ev.symbol = shared;
        }
        sources.push(events.into_iter());
    }

    // Global stream order (EVT-5): a midnight-straddling or multi-log corpus
    // must reach the engine sorted, or bar buckets would regress.
    let merged = mp_core::log::merge_sorted_events(sources);
    let last_ts = merged.last().map(|e| e.recv_ts_ns).unwrap_or(0);

    let mut engine: FeatureEngine = engine_from_config(cfg).map_err(|e| e.to_string())?;
    let mut updates = engine.run(&merged);
    // End-of-stream: emit the final partial bars (engine docs mandate this for
    // offline loops, or the last bar per symbol is silently dropped).
    updates.extend(engine.finish(last_ts));
    let nan_suppressed = engine.nan_suppressed();

    // Build the symbols snapshot payload (rows carry numeric ids only, so the
    // mapping must ship WITH the store or the data is unresolvable — audit
    // 2026-08-06). Written to disk only AFTER every feature file succeeds
    // (below), so a failed run never leaves an orphan snapshot; skipped
    // entirely for an empty table.
    let (symbols_hash, symbols_json) = if symbols.is_empty() {
        (String::new(), None)
    } else {
        let rows: Vec<SymbolRow> = symbols
            .metas()
            .iter()
            .map(|m| SymbolRow {
                id: m.symbol_id.0,
                venue: m.venue.slug().to_string(),
                venue_symbol: m.venue_symbol.clone(),
            })
            .collect();
        let json = serde_json::to_vec_pretty(&rows)
            .map_err(|e| format!("serialize symbols snapshot: {e}"))?;
        let mut h = FNV1A_OFFSET;
        h = fnv1a_absorb(h, &json);
        (format!("{h:016x}"), Some(json))
    };

    // Group by (feature, venue, symbol), then by each row's own UTC date.
    let mut groups: BTreeMap<(String, Venue, u32, u16), Vec<FeatureRow>> = BTreeMap::new();
    for u in &updates {
        groups
            .entry((u.name.clone(), u.venue, u.symbol.0, u.ver))
            .or_default()
            .push(FeatureRow {
                symbol_id: u.symbol.0,
                venue_code: u.venue as u16,
                ts_ns: u.ts_ns,
                value: u.value,
                ver: u.ver,
            });
    }

    let params_hash = cfg.params_hash().map_err(|e| e.to_string())?;
    let mut stats = MaterializeStats {
        events_read,
        updates: updates.len() as u64,
        nan_suppressed,
        ..MaterializeStats::default()
    };
    for ((feature, venue, symbol_id, ver), mut rows) in groups {
        // MAT-5: deterministic output — sorted rows, stable tiebreak.
        rows.sort_by(|a, b| {
            a.ts_ns
                .cmp(&b.ts_ns)
                .then_with(|| a.value.to_bits().cmp(&b.value.to_bits()))
                .then_with(|| a.ver.cmp(&b.ver))
        });
        let venue_slug = venue.slug().to_string();
        let meta = FeatureMeta {
            feature_ver: ver,
            engine_git_sha: git_sha.to_string(),
            params_hash: params_hash.clone(),
            symbols_hash: symbols_hash.clone(),
        };
        // Partition by each row's own UTC date floor (a midnight-straddling
        // buffer lands rows on the days they happened).
        let mut by_date: BTreeMap<String, Vec<FeatureRow>> = BTreeMap::new();
        for r in &rows {
            by_date.entry(date_str(r.ts_ns)).or_default().push(*r);
        }
        for (date, day_rows) in by_date {
            materialize(
                root,
                &feature,
                &venue_slug,
                symbol_id,
                &date,
                &day_rows,
                &meta,
            )
            .map_err(|e| format!("materialize {feature}/{venue_slug}/{symbol_id}/{date}: {e}"))?;
            stats.rows_written += day_rows.len() as u64;
            stats.files_written += 1;
        }
        stats.features.push(feature);
    }
    stats.features.sort();
    stats.features.dedup();

    // Persist the shared symbol table LAST, after all feature files are on
    // disk (content-addressed and immutable, W-6): an identical table is a
    // byte-level no-op; a different table under the same hash would be a
    // collision and is refused.
    if let Some(json) = symbols_json {
        write_symbols_snapshot_file(root, &symbols_hash, &json)?;
    }
    stats.symbols = symbols.len();
    stats.symbols_hash = symbols_hash;
    Ok(stats)
}
/// One row of the persisted symbols snapshot: enough to resolve a numeric
/// `symbol_id` back to its `(venue, venue_symbol)`. Deliberately minimal — the
/// full [`SymbolMeta`] carries `f64::NAN` precision fields that serde_json
/// cannot round-trip (NaN serializes to `null`, which is not a valid f64).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRow {
    /// The shared-table id this row's `symbol_id` columns refer to.
    pub id: u32,
    /// Venue slug (e.g. "bybit").
    pub venue: String,
    /// Venue's own symbol string (e.g. "BTCUSDT").
    pub venue_symbol: String,
}

/// Write the already-serialized symbols snapshot to `{root}/symbols/{hash}.json`
/// — content-addressed and immutable (W-6): an identical table is a byte-level
/// no-op; a different table under the same hash would be a collision and is
/// refused. `hash`/`json` come from the caller so the hash is available to the
/// Parquet footers before the file is persisted.
fn write_symbols_snapshot_file(root: &Path, hash: &str, json: &[u8]) -> Result<(), String> {
    let dir = root.join("symbols");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{hash}.json"));
    if path.exists() {
        let existing = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if existing != json {
            return Err(format!(
                "symbols snapshot hash collision at {} (different content under the same hash — W-6)",
                path.display()
            ));
        }
        return Ok(()); // identical content: nothing to do
    }
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}
