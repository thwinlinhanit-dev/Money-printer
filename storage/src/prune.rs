//! Safe prune (STO-3, W-6). Deleting a source event log is allowed ONLY after
//! verifying its Parquet + manifest exist and the row counts match. This is the
//! human-run migration guard; the compactor never deletes.

use crate::{compactor, layout, StorageError};
use mp_core::Venue;
use std::path::Path;

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
}

/// Verify that a venue/day is safely prunable: the manifest exists and, for
/// every Parquet-backed stream in the manifest, a Parquet file exists whose
/// row count matches the manifest's event count. Returns `Ok(())` if safe to
/// delete the source logs, or the first refusal reason.
pub fn verify_prunable(root: &Path, venue: Venue, date: &str) -> Result<(), PruneRefusal> {
    let manifest = match compactor::load_manifest(root, venue, date) {
        Ok(m) => m,
        Err(_) => return Err(PruneRefusal::ManifestMissing),
    };

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
    }
    Ok(())
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
