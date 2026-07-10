//! Safe prune (STO-3, W-6). Deleting a source event log is allowed ONLY after
//! verifying its Parquet + manifest exist and the row counts match. This is the
//! human-run migration guard; the compactor never deletes.

use crate::{compactor, layout, parquet_trades, StorageError};
use mp_core::Venue;
use std::path::Path;

/// Why a prune was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneRefusal {
    ManifestMissing,
    TradesFileMissing {
        symbol: String,
    },
    RowCountMismatch {
        symbol: String,
        manifest: u64,
        parquet: u64,
    },
}

/// Verify that a venue/day is safely prunable: the manifest exists and, for
/// every symbol with trades in the manifest, a Parquet file exists whose row
/// count matches the manifest's event count. Returns `Ok(())` if safe to delete
/// the source logs, or the first refusal reason.
pub fn verify_prunable(root: &Path, venue: Venue, date: &str) -> Result<(), PruneRefusal> {
    let manifest = match compactor::load_manifest(root, venue, date) {
        Ok(m) => m,
        Err(_) => return Err(PruneRefusal::ManifestMissing),
    };

    for (key, stats) in &manifest.streams {
        let Some(symbol) = key.strip_prefix("trades:") else {
            continue;
        };
        let path = layout::partition_file(root, "trades", venue, symbol, date);
        if !path.exists() {
            return Err(PruneRefusal::TradesFileMissing {
                symbol: symbol.to_owned(),
            });
        }
        let rows = count_parquet_rows(&path).map_err(|_| PruneRefusal::TradesFileMissing {
            symbol: symbol.to_owned(),
        })?;
        if rows != stats.events {
            return Err(PruneRefusal::RowCountMismatch {
                symbol: symbol.to_owned(),
                manifest: stats.events,
                parquet: rows,
            });
        }
    }
    Ok(())
}

fn count_parquet_rows(path: &Path) -> Result<u64, StorageError> {
    // Cheap: read back and count (small daily files). A metadata-only count is a
    // later optimization.
    Ok(parquet_trades::read_trades(path)?.len() as u64)
}
