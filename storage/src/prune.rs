//! Safe prune (STO-3, W-6). Deleting a source event log is allowed ONLY after
//! verifying its Parquet + manifest exist and the row counts match. This is the
//! human-run migration guard; the compactor never deletes.

use crate::{compactor, layout, parquet_trades, StorageError};
use mp_core::Venue;
use std::path::{Path, PathBuf};

/// Why a prune was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneRefusal {
    ManifestMissing,
    ParquetFileMissing {
        stream: String,
        symbol: String,
    },
    RowCountMismatch {
        stream: String,
        symbol: String,
        manifest: u64,
        parquet: u64,
    },
    /// C-2 (W-6): the raw log's CURRENT bytes do not hash to the Parquet
    /// footer's `source_log_hash` — the raw log grew (late venue data
    /// appended after compaction) or changed, so the Parquet is stale and
    /// deleting the raw log would lose events permanently.
    SourceLogHashMismatch {
        stream: String,
        symbol: String,
    },
    /// C-2 (W-6): the raw log's mtime is newer than the Parquet file's mtime.
    /// Belt to the hash check's braces: catches modifications the 32-bit
    /// source hash alone could mask (and re-touched files).
    RawLogNewerThanParquet {
        stream: String,
        symbol: String,
    },
    /// C-2 fail-closed: the Parquet footer carries no `source_log_hash` KV
    /// (STO-8), so provenance is unknowable. Never allow deletion on unknown
    /// provenance.
    FooterMissingSourceLogHash {
        stream: String,
        symbol: String,
    },
    /// The day's manifest carries NO stream for the symbol being pruned (B-2,
    /// audit 2026-09-03). Compaction is per-symbol but the manifest is per
    /// venue/day, and a clobbered/legacy manifest can list only the
    /// last-compacted symbol. Deleting this symbol's raw log on ANOTHER
    /// symbol's proof would be exactly the C-2 failure A-1 exists to close —
    /// refuse until the symbol's own streams are in the manifest.
    SymbolNotInManifest { symbol: String },
}

/// Verify that a venue/day is safely prunable: the manifest exists and, for
/// every Parquet-backed stream in the manifest, a Parquet file exists whose
/// row count matches the manifest's event count AND whose provenance still
/// matches the raw logs: the footer's `source_log_hash` equals the CRC-32 of
/// each existing raw day-file for that venue/symbol (the exact hashing the
/// compaction caller applied when the Parquet was written), and the raw log
/// is not newer than the Parquet. A raw log that no longer exists is skipped
/// (nothing left to lose) but the Parquet-existence and row-count checks
/// always apply. Returns `Ok(())` if safe to delete the source logs, or the
/// first refusal reason.
pub fn verify_prunable(root: &Path, venue: Venue, date: &str) -> Result<(), PruneRefusal> {
    verify_prunable_impl(root, venue, date, None)
}

/// [`verify_prunable`] scoped to one symbol: additionally requires the day
/// manifest to carry at least one stream for `symbol` before ANY proof is
/// accepted (B-2). `verify_prunable` alone iterates only the streams the
/// manifest lists, so on a clobbered/legacy manifest that lost the symbol's
/// entry (per-symbol compaction overwrites the shared venue/day manifest) it
/// would verify another symbol's Parquet+raw and green-light deleting THIS
/// symbol's raw log unexamined. mp-ops prune calls this; `symbol` is the raw
/// log being deleted.
pub fn verify_prunable_symbol(
    root: &Path,
    venue: Venue,
    date: &str,
    symbol: &str,
) -> Result<(), PruneRefusal> {
    verify_prunable_impl(root, venue, date, Some(symbol))
}

fn verify_prunable_impl(
    root: &Path,
    venue: Venue,
    date: &str,
    require_symbol: Option<&str>,
) -> Result<(), PruneRefusal> {
    let manifest = match compactor::load_manifest(root, venue, date) {
        Ok(m) => m,
        Err(_) => return Err(PruneRefusal::ManifestMissing),
    };

    // B-2 per-symbol guard: the deleted raw log's own symbol must be present
    // in the day manifest. Without this, a manifest that lists only the
    // last-compacted symbol would let this symbol's raw log be deleted on
    // another symbol's proof (the exact C-2 failure A-1 was built to close).
    if let Some(symbol) = require_symbol {
        let owned = manifest
            .streams
            .keys()
            .any(|key| key.rsplit_once(':').is_some_and(|(_, s)| s == symbol));
        if !owned {
            return Err(PruneRefusal::SymbolNotInManifest {
                symbol: symbol.to_owned(),
            });
        }
    }

    for (key, stats) in &manifest.streams {
        let Some((stream, symbol)) = key.split_once(':') else {
            continue;
        };
        if !layout::has_parquet_partition(stream) {
            continue;
        }
        let path = layout::partition_file(root, stream, venue, symbol, date);
        if !path.exists() {
            return Err(PruneRefusal::ParquetFileMissing {
                stream: stream.to_owned(),
                symbol: symbol.to_owned(),
            });
        }
        let rows =
            count_parquet_rows(stream, &path).map_err(|_| PruneRefusal::ParquetFileMissing {
                stream: stream.to_owned(),
                symbol: symbol.to_owned(),
            })?;
        if rows != stats.events {
            return Err(PruneRefusal::RowCountMismatch {
                stream: stream.to_owned(),
                symbol: symbol.to_owned(),
                manifest: stats.events,
                parquet: rows,
            });
        }

        // C-2 provenance guard: refuse fail-closed when the footer lacks the
        // source hash — even if the raw logs are already gone — because
        // deletion on unknown provenance can never be certified (W-6).
        let footer_hash = match parquet_trades::read_source_hash(&path) {
            Ok(Some(h)) => h,
            Ok(None) => {
                return Err(PruneRefusal::FooterMissingSourceLogHash {
                    stream: stream.to_owned(),
                    symbol: symbol.to_owned(),
                })
            }
            // The footer is unreadable although the row scan succeeded —
            // treat the file as untrustworthy rather than prunable.
            Err(_) => {
                return Err(PruneRefusal::ParquetFileMissing {
                    stream: stream.to_owned(),
                    symbol: symbol.to_owned(),
                })
            }
        };

        // Raw day-files for this venue/symbol. Missing raw dir / missing file
        // = already deleted: nothing left to lose, skip the hash+mtime checks.
        for raw in raw_log_files(root, venue, date, symbol) {
            // Hash check first (precise provenance): the CURRENT raw bytes
            // must hash to exactly what the compaction caller hashed when it
            // wrote this Parquet (crc32fast over the whole file, {:08x} hex).
            let data = std::fs::read(&raw).map_err(|_| PruneRefusal::ParquetFileMissing {
                stream: stream.to_owned(),
                symbol: symbol.to_owned(),
            })?;
            let computed = format!("{:08x}", crc32_ieee(&data));
            if computed != footer_hash {
                return Err(PruneRefusal::SourceLogHashMismatch {
                    stream: stream.to_owned(),
                    symbol: symbol.to_owned(),
                });
            }
            // mtime check: a raw log modified after the Parquet was written
            // is a compact-me-again signal, not a deletable one.
            let raw_mtime = std::fs::metadata(&raw)
                .and_then(|m| m.modified())
                .map_err(|_| PruneRefusal::ParquetFileMissing {
                    stream: stream.to_owned(),
                    symbol: symbol.to_owned(),
                })?;
            let parquet_mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .map_err(|_| PruneRefusal::ParquetFileMissing {
                    stream: stream.to_owned(),
                    symbol: symbol.to_owned(),
                })?;
            if raw_mtime > parquet_mtime {
                return Err(PruneRefusal::RawLogNewerThanParquet {
                    stream: stream.to_owned(),
                    symbol: symbol.to_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Existing raw day-file candidates for one venue/symbol/date:
/// `<sibling raw dir>/{YYYYMMDD}_{venue token}_{symbol}.log` (see
/// [`layout::raw_log_dir`] / [`layout::raw_file_venue_tokens`]). Empty when
/// the raw corpus is absent (already deleted, or a cold root with no
/// sibling raw directory).
fn raw_log_files(root: &Path, venue: Venue, date: &str, symbol: &str) -> Vec<PathBuf> {
    let Some(raw_dir) = layout::raw_log_dir(root) else {
        return Vec::new();
    };
    let date_flat = date.replace('-', "");
    let mut out = Vec::new();
    for token in layout::raw_file_venue_tokens(venue) {
        let p = raw_dir.join(format!("{date_flat}_{token}_{symbol}.log"));
        if p.exists() {
            out.push(p);
        }
    }
    out
}

// ---- source-log hash --------------------------------------------------------

/// The canonical raw-log source hash: CRC-32 (IEEE) over the log's whole byte
/// content, `{:08x}`-hex-encoded. This is exactly what the compaction caller
/// (`mp-ops compact` → `compute_source_hash`, via `crc32fast`) feeds to
/// `compact_day`, and what [`crate::parquet_trades`] persists as the
/// `source_log_hash` footer KV (STO-8). Prune recomputes it over the CURRENT
/// raw bytes and refuses deletion on mismatch (C-2, W-6).
pub fn source_log_hash(data: &[u8]) -> String {
    format!("{:08x}", crc32_ieee(data))
}

/// CRC-32 (IEEE 802.3, reflected, poly `0xEDB88320`) — bit-for-bit identical
/// to `crc32fast::hash`. That is the exact primitive the compaction caller
/// (`mp-ops compact` → `compute_source_hash`) applies to the raw log's whole
/// byte content before handing the `{:08x}` hex string to `compact_day`
/// (STO-1/STO-8); the compactor persists it verbatim into the Parquet footer.
/// Replicated locally so the prune guard stays a pure-storage check: any
/// change to the caller's algorithm must land here in the same review, pinned
/// by `regression_audit28_hash_matches_caller_crc32fast` (which diffs this
/// against the real `crc32fast` dev-dependency).
fn crc32_ieee(data: &[u8]) -> u32 {
    static TABLE: [u32; 256] = build_crc32_table();
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = TABLE[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

fn count_parquet_rows(stream: &str, path: &Path) -> Result<u64, StorageError> {
    // Cheap: read back and count (small daily files). A metadata-only count is
    // a later optimization.
    let n = match stream {
        "trades" => crate::parquet_trades::read_trades(path)?.len(),
        "positions" => crate::parquet_positions::read_positions(path)?.len(),
        "macro" => crate::parquet_macro::read_macro(path)?.len(),
        "options" => crate::parquet_options::read_options(path)?.len(),
        _ => {
            return Err(StorageError::Refused(format!(
                "no parquet reader for {stream}"
            )))
        }
    };
    Ok(n as u64)
}
