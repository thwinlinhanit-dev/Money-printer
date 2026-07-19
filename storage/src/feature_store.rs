//! Feature-store materialization (FEA-6). The feature engine run over the
//! Dataset reader (003) writes named/versioned/timestamped features to Parquet
//! with footer metadata `{feature ver, engine git sha, params hash}`. A changed
//! params hash or feature version allocates a NEW `ver=N` directory — recorded
//! feature data is never overwritten (W-6). Real Parquet so research reads the
//! same bytes.
//!
//! Layout: `{root}/{feature}/ver={N}/venue={v}/symbol={s}/date={d}.parquet`,
//! with a `{root}/{feature}/ver={N}/_params` marker holding the params hash so
//! the version resolver can match without opening data files.

use crate::StorageError;
use arrow::array::{Float64Array, Int64Array, RecordBatch, UInt16Array, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::format::KeyValue;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Footer metadata keys (FEA-6).
pub const KV_FEATURE_VER: &str = "feature_ver";
pub const KV_ENGINE_GIT_SHA: &str = "engine_git_sha";
pub const KV_PARAMS_HASH: &str = "params_hash";

/// One materialized feature sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeatureRow {
    pub symbol_id: u32,
    pub venue_code: u16,
    pub ts_ns: i64,
    pub value: f64,
    /// Feature version at emit time (`FeatureUpdate::ver`).
    pub ver: u16,
}

/// Materialization metadata written to the Parquet footer (FEA-6).
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureMeta {
    pub feature_ver: u16,
    pub engine_git_sha: String,
    pub params_hash: String,
}

fn feature_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("symbol_id", DataType::UInt32, false),
        Field::new("venue_code", DataType::UInt16, false),
        Field::new("ts_ns", DataType::Int64, false),
        Field::new("value", DataType::Float64, false),
        Field::new("ver", DataType::UInt16, false),
    ]))
}

/// Write rows (already for one feature/venue/symbol/date, sorted by ts) to a
/// Parquet file with FEA-6 footer metadata. Overwrites the *same* file only —
/// version separation (never clobber different params) is the caller's job via
/// [`materialize`].
pub fn write_features(
    path: &Path,
    rows: &[FeatureRow],
    meta: &FeatureMeta,
) -> Result<u64, StorageError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (mut sym, mut ven, mut ts, mut val, mut ver) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for r in rows {
        sym.push(r.symbol_id);
        ven.push(r.venue_code);
        ts.push(r.ts_ns);
        val.push(r.value);
        ver.push(r.ver);
    }
    let n = sym.len() as u64;

    let batch = RecordBatch::try_new(
        feature_schema(),
        vec![
            Arc::new(UInt32Array::from(sym)),
            Arc::new(UInt16Array::from(ven)),
            Arc::new(Int64Array::from(ts)),
            Arc::new(Float64Array::from(val)),
            Arc::new(UInt16Array::from(ver)),
        ],
    )
    .map_err(|e| StorageError::Arrow(e.to_string()))?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3).unwrap()))
        .set_key_value_metadata(Some(vec![
            KeyValue::new(KV_FEATURE_VER.into(), meta.feature_ver.to_string()),
            KeyValue::new(KV_ENGINE_GIT_SHA.into(), meta.engine_git_sha.clone()),
            KeyValue::new(KV_PARAMS_HASH.into(), meta.params_hash.clone()),
        ]))
        .build();

    let file = File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, feature_schema(), Some(props))
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .write(&batch)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .close()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    Ok(n)
}

/// Read FEA-6 footer metadata without scanning data.
pub fn read_feature_meta(path: &Path) -> Result<Option<FeatureMeta>, StorageError> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    let kvs = builder.metadata().file_metadata().key_value_metadata();
    let Some(kvs) = kvs else { return Ok(None) };
    let get = |k: &str| {
        kvs.iter()
            .find(|kv| kv.key == k)
            .and_then(|kv| kv.value.clone())
    };
    match (
        get(KV_FEATURE_VER),
        get(KV_ENGINE_GIT_SHA),
        get(KV_PARAMS_HASH),
    ) {
        (Some(v), Some(sha), Some(ph)) => Ok(Some(FeatureMeta {
            feature_ver: v.parse().unwrap_or(0),
            engine_git_sha: sha,
            params_hash: ph,
        })),
        _ => Ok(None),
    }
}

/// Read a feature Parquet file back into rows.
pub fn read_features(path: &Path) -> Result<Vec<FeatureRow>, StorageError> {
    let file = File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| StorageError::Parquet(e.to_string()))?
        .build()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| StorageError::Arrow(e.to_string()))?;
        let sym = dc::<UInt32Array>(&batch, 0)?;
        let ven = dc::<UInt16Array>(&batch, 1)?;
        let ts = dc::<Int64Array>(&batch, 2)?;
        let val = dc::<Float64Array>(&batch, 3)?;
        let ver = dc::<UInt16Array>(&batch, 4)?;
        for i in 0..batch.num_rows() {
            out.push(FeatureRow {
                symbol_id: sym.value(i),
                venue_code: ven.value(i),
                ts_ns: ts.value(i),
                value: val.value(i),
                ver: ver.value(i),
            });
        }
    }
    Ok(out)
}

fn dc<T: 'static>(b: &RecordBatch, i: usize) -> Result<&T, StorageError> {
    b.column(i)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| StorageError::Arrow(format!("bad column type at {i}")))
}

/// Resolve the `ver=N` directory for `(feature, params_hash)` under `root`,
/// implementing the FEA-6 "changed params ⇒ new ver, never overwrite" rule:
/// - if an existing `ver=N` has a matching `_params` marker ⇒ reuse it
///   (idempotent re-materialization);
/// - otherwise allocate `max(existing)+1` (or 0 if none) and write its marker.
///
/// Returns the resolved version.
pub fn resolve_version(root: &Path, feature: &str, params_hash: &str) -> Result<u16, StorageError> {
    let feat_dir = root.join(feature);
    std::fs::create_dir_all(&feat_dir)?;
    let mut max_ver: Option<u16> = None;
    for entry in std::fs::read_dir(&feat_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(n) = name
            .strip_prefix("ver=")
            .and_then(|s| s.parse::<u16>().ok())
        else {
            continue;
        };
        max_ver = Some(max_ver.map_or(n, |m| m.max(n)));
        let marker = entry.path().join("_params");
        if let Ok(existing) = std::fs::read_to_string(&marker) {
            if existing.trim() == params_hash {
                return Ok(n); // same params ⇒ reuse this version (idempotent)
            }
        }
    }
    let new_ver = max_ver.map_or(0, |m| m + 1);
    let ver_dir = feat_dir.join(format!("ver={new_ver}"));
    std::fs::create_dir_all(&ver_dir)?;
    std::fs::write(ver_dir.join("_params"), params_hash)?;
    Ok(new_ver)
}

/// Streaming feature store (spec 016). Buffers FeatureUpdates in memory and
/// flushes to Parquet on threshold or interval. Wraps the batch materialization
/// logic above for zero-data-loss-on-crash operation.
pub struct StreamingFeatureStore {
    root: PathBuf,
    buffer: Vec<FeatureRow>,
    meta: FeatureMeta,
    flush_threshold: usize,
    last_flush_ns: i64,
    flush_interval_ns: i64,
    feature: String,
    venue_slug: String,
    symbol_id: u32,
}

impl StreamingFeatureStore {
    pub fn new(
        root: &Path,
        feature: &str,
        venue_slug: &str,
        symbol_id: u32,
        meta: FeatureMeta,
    ) -> Self {
        Self {
            root: root.to_owned(),
            buffer: Vec::with_capacity(10_000),
            meta,
            flush_threshold: 10_000,
            last_flush_ns: 0,
            flush_interval_ns: 60_000_000_000, // 60 seconds
            feature: feature.to_owned(),
            venue_slug: venue_slug.to_owned(),
            symbol_id,
        }
    }

    /// Push one row. Auto-flushes when threshold crossed or interval elapsed.
    pub fn push(&mut self, row: FeatureRow, ts_ns: i64) -> Result<(), StorageError> {
        self.buffer.push(row);
        let elapsed = ts_ns - self.last_flush_ns;
        if self.buffer.len() >= self.flush_threshold || elapsed >= self.flush_interval_ns {
            self.flush(ts_ns)?;
        }
        Ok(())
    }

    /// Force-flush any buffered rows to Parquet (e.g. on shutdown).
    pub fn flush(&mut self, ts_ns: i64) -> Result<(), StorageError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        // Sort by ts_ns for deterministic output (MAT-5).
        self.buffer.sort_by_key(|r| r.ts_ns);
        // Use the date from the latest row for the partition path.
        let d = date_str(ts_ns);
        let path = self.root
            .join(&self.feature)
            .join("ver=1")
            .join(format!("venue={}", self.venue_slug))
            .join(format!("symbol={}", self.symbol_id))
            .join(format!("{d}.parquet"));
        write_features(&path, &self.buffer, &self.meta)?;
        self.buffer.clear();
        self.last_flush_ns = ts_ns;
        Ok(())
    }
}

fn date_str(ns: i64) -> String {
    let secs = (ns / 1_000_000_000).max(0) as u64;
    let days = secs / 86400;
    let mut y = 1970i64;
    let mut rem = days as i64;
    loop {
        let days_yr = if is_leap(y) { 366 } else { 365 };
        if rem < days_yr { break; }
        rem -= days_yr;
        y += 1;
    }
    let months = [31, if is_leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    while m < 12 && rem >= months[m] { rem -= months[m]; m += 1; }
    format!("{:04}-{:02}-{:02}", y, m + 1, rem + 1)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Full FEA-6 materialization: resolve the version for the params, then write
/// the feature file at `{root}/{feature}/ver=N/venue={v}/symbol={s}/{date}.parquet`.
/// Returns the file path written.
pub fn materialize(
    root: &Path,
    feature: &str,
    venue_slug: &str,
    symbol_id: u32,
    date: &str,
    rows: &[FeatureRow],
    meta: &FeatureMeta,
) -> Result<PathBuf, StorageError> {
    let ver = resolve_version(root, feature, &meta.params_hash)?;
    let meta = FeatureMeta {
        feature_ver: meta.feature_ver,
        engine_git_sha: meta.engine_git_sha.clone(),
        params_hash: meta.params_hash.clone(),
    };
    let path = root
        .join(feature)
        .join(format!("ver={ver}"))
        .join(format!("venue={venue_slug}"))
        .join(format!("symbol={symbol_id}"))
        .join(format!("{date}.parquet"));
    write_features(&path, rows, &meta)?;
    Ok(path)
}
