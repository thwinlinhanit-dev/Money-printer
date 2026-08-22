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

pub mod analytics;
pub mod audit;
pub mod compactor;
pub mod cross_venue;
pub mod dataset;
pub mod determinism;
pub mod feature_store;
pub mod historical;
pub mod layout;
pub mod manifest;
pub mod materialize;
pub mod migrate;
pub mod parquet_macro;
pub mod parquet_options;
pub mod parquet_positions;
pub mod parquet_trades;
pub mod promotion;
pub mod prune;
pub mod scd2;

/// Live Binance-archive download (spec 027 HBS-1/HBS-8) — gated on the
/// `live-http` feature (owner-approved 2026-08-05) so the offline core builds
/// with no network stack.
#[cfg(feature = "live-http")]
pub mod historical_download;

/// Storage errors.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet: {0}")]
    Parquet(String),
    #[error("arrow: {0}")]
    Arrow(String),
    #[error("refused: {0}")]
    Refused(String),
}

pub use audit::{
    audit_raw_log, scorecard, AuditConfig, DailyScorecard, RawLogAudit, RecordingBursts,
};
pub use compactor::{compact_day, compact_day_verified, CompactStats};
pub use cross_venue::{
    app_version, config_hash, detect, findings_file, parse_config, version_string, write_findings,
    Classification, CohortMember, CrossVenueConfig, Finding, Findings, SymbolCohort,
    VeracityFinding,
};
pub use dataset::Dataset;
pub use determinism::{determinism_file, load_determinism, write_determinism, DeterminismArtifact};
pub use feature_store::{
    materialize, read_feature_meta, read_features, resolve_version, FeatureMeta, FeatureRow,
    StreamingFeatureStore,
};
pub use historical::{
    bootstrap_day, day_complete, historical_manifest_file, historical_trades_file, parse_aggtrades,
    parse_historical_config, BootstrapStats, FileHistoricalSource, HistoricalConfig,
    HistoricalSource, MockHistoricalSource,
};
#[cfg(feature = "live-http")]
pub use historical_download::{unzip_single_csv, BinanceVisionSource, RetryPolicy, Throttle};
pub use manifest::{derive_manifest, Gap, GapKind, QualityManifest, StreamStats};
pub use materialize::{
    load_logs_merged, materialize_logs, materialize_logs_limited, stream_logs_merged, LoadedLogs,
    MaterializeStats, StreamedMergedLogs, SymbolRow, DEFAULT_MAX_BACKFILL_BYTES,
};
pub use migrate::{migrate_log, MigrateError, MigrateOutcome};
pub use promotion::{
    check_promotion, check_promotion_determinism, check_promotion_n, PromotionVerdict,
    REQUIRED_CONSECUTIVE_CLEAN_DAYS,
};
pub use prune::{verify_prunable, PruneRefusal};
pub use scd2::{Scd2AppendError, SymbolScd2, SymbolVersion};
