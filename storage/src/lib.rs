//! mp-storage — Parquet cold store, quality manifests, dataset reader (spec 003).
//!
//! Turns the event log (spec 001) into a permanent, queryable, quality-tracked
//! dataset. Cold Parquet is the research substrate; **quality manifests are what
//! make backtests trustworthy** — every read path consults them (SIM-6).
//!
//! v1 slice: trades → Parquet, the full manifest honesty layer, dataset reader
//! with coverage/gaps, SCD2 symbol as-of, and safe prune. Other streams'
//! Parquet and the optional ClickHouse warm store are the same pattern, tracked
//! in spec 003 Decisions.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod compactor;
pub mod dataset;
pub mod feature_store;
pub mod layout;
pub mod manifest;
pub mod parquet_trades;
pub mod prune;
pub mod scd2;

/// Storage errors.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet: {0}")]
    Parquet(String),
    #[error("arrow: {0}")]
    Arrow(String),
}

pub use compactor::{compact_day, CompactStats};
pub use dataset::Dataset;
pub use feature_store::{
    materialize, read_feature_meta, read_features, resolve_version, FeatureMeta, FeatureRow,
    StreamingFeatureStore,
};
pub use manifest::{derive_manifest, Gap, GapKind, QualityManifest, StreamStats};
pub use prune::{verify_prunable, PruneRefusal};
pub use scd2::{SymbolScd2, SymbolVersion};
