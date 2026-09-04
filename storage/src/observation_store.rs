//! Signal-observation Parquet store (research-lab hardening Phase 3, REL-8).
//!
//! Persists [`mp_features::SignalObservation`] rows to Parquet mirroring the
//! feature-store contract (`feature_store.rs`): zstd-6, footer key-value
//! metadata carrying the identity fingerprint, a deterministic content hash
//! suffix (`{date}-{hash}.parquet`), and the W-6 no-overwrite guard — an
//! identical re-write is a no-op, divergent content on the same path is a
//! hard error, never a silent clobber.
//!
//! One observation with `k` attached outcomes becomes `k` rows (one per
//! outcome; outcome columns null when none attached). This is a NEW artifact
//! with no legacy files, so the schema is fixed at birth; future changes bump
//! the footer `observation_schema_ver`.

use crate::StorageError;
use arrow::array::{
    Array, Float64Array, Int64Array, RecordBatch, StringArray, UInt16Array, UInt32Array,
    UInt64Array, UInt8Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use mp_core::{fnv1a_absorb, FNV1A_OFFSET, SymbolId};
use mp_features::data_quality::DataQualityState;
use mp_features::observation::{Direction, ObservationOutcome, SignalObservation};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::format::KeyValue;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Footer metadata keys.
pub const KV_IDENTITY_FINGERPRINT: &str = "identity_fingerprint";
pub const KV_OBSERVATION_SCHEMA_VER: &str = "observation_schema_ver";

/// Current observation Parquet schema version (bump = new columns are legal).
pub const OBSERVATION_SCHEMA_VER: u16 = 1;

fn observation_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("observation_id", DataType::UInt64, false),
        Field::new("signal_id", DataType::Utf8, false),
        Field::new("feature_version", DataType::UInt16, false),
        Field::new("data_schema_version", DataType::UInt16, false),
        Field::new("params_hash", DataType::Utf8, false),
        Field::new("cost_model_hash", DataType::Utf8, false),
        Field::new("ts_ns", DataType::Int64, false),
        Field::new("symbol_id", DataType::UInt32, false),
        Field::new("venue_code", DataType::UInt16, false),
        Field::new("direction", DataType::Int8, false),
        Field::new("quality", DataType::UInt8, false),
        Field::new("created_at_ns", DataType::Int64, false),
        Field::new("snapshot_json", DataType::Utf8, false),
        // Outcome columns — null when no outcome is attached for that row.
        Field::new("outcome_horizon_ns", DataType::Int64, true),
        Field::new("outcome_entry_price", DataType::Float64, true),
        Field::new("outcome_exit_price", DataType::Float64, true),
        Field::new("outcome_gross_return", DataType::Float64, true),
        Field::new("outcome_net_return", DataType::Float64, true),
        Field::new("outcome_mfe", DataType::Float64, true),
        Field::new("outcome_mae", DataType::Float64, true),
        Field::new("outcome_hit", DataType::UInt8, true),
    ]))
}

fn quality_code(q: DataQualityState) -> u8 {
    match q {
        DataQualityState::Healthy => 0,
        DataQualityState::InsufficientHistory => 1,
        DataQualityState::Stale => 2,
        DataQualityState::Invalid => 3,
    }
}

fn quality_from_code(code: u8) -> DataQualityState {
    match code {
        0 => DataQualityState::Healthy,
        1 => DataQualityState::InsufficientHistory,
        2 => DataQualityState::Stale,
        _ => DataQualityState::Invalid,
    }
}

fn direction_code(d: Direction) -> i8 {
    match d {
        Direction::Long => 1,
        Direction::Short => -1,
    }
}

fn direction_from_code(code: i8) -> Direction {
    if code < 0 {
        Direction::Short
    } else {
        Direction::Long
    }
}

/// Deterministic content hash over observations + outcomes (FNV-1a) — the
/// W-6 no-overwrite identity. Same observations ⇒ same hash, always.
pub fn observations_content_hash(obs: &[SignalObservation]) -> u64 {
    let mut h = FNV1A_OFFSET;
    for o in obs {
        h = fnv1a_absorb(h, &o.observation_id.to_le_bytes());
        h = fnv1a_absorb(h, o.identity.fingerprint().as_bytes());
        h = fnv1a_absorb(h, &o.timestamp_ns.to_le_bytes());
        h = fnv1a_absorb(h, &o.symbol.0.to_le_bytes());
        h = fnv1a_absorb(h, &crate::layout::venue_code(o.venue).to_le_bytes());
        h = fnv1a_absorb(h, &direction_code(o.direction).to_le_bytes());
        h = fnv1a_absorb(h, &quality_code(o.quality).to_le_bytes());
        h = fnv1a_absorb(h, &o.created_at_ns.to_le_bytes());
        for (k, v) in &o.feature_snapshot {
            h = fnv1a_absorb(h, k.as_bytes());
            h = fnv1a_absorb(h, &v.to_bits().to_le_bytes());
        }
        for oc in &o.outcomes {
            h = fnv1a_absorb(h, &oc.horizon_ns.to_le_bytes());
            h = fnv1a_absorb(h, &oc.entry_price.to_bits().to_le_bytes());
            h = fnv1a_absorb(h, &oc.exit_price.to_bits().to_le_bytes());
            h = fnv1a_absorb(h, &oc.gross_return.to_bits().to_le_bytes());
            h = fnv1a_absorb(h, &oc.net_return.to_bits().to_le_bytes());
            h = fnv1a_absorb(h, &oc.mfe.to_bits().to_le_bytes());
            h = fnv1a_absorb(h, &oc.mae.to_bits().to_le_bytes());
            h = fnv1a_absorb(h, &u8::from(oc.hit).to_le_bytes());
        }
    }
    h
}

/// Write observations to `path` (creating parent dirs) with footer metadata.
/// One row per (observation, outcome); observations without outcomes write
/// one row with null outcome columns.
pub fn write_observations(path: &Path, obs: &[SignalObservation]) -> Result<u64, StorageError> {
    if obs.is_empty() {
        return Ok(0);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let identity_fp = obs[0].identity.fingerprint();
    let n_rows: usize = obs.iter().map(|o| o.outcomes.len().max(1)).sum();
    let mut oid = Vec::with_capacity(n_rows);
    let mut sid = Vec::with_capacity(n_rows);
    let mut fv = Vec::with_capacity(n_rows);
    let mut dv = Vec::with_capacity(n_rows);
    let mut ph = Vec::with_capacity(n_rows);
    let mut ch = Vec::with_capacity(n_rows);
    let mut ts = Vec::with_capacity(n_rows);
    let mut sym = Vec::with_capacity(n_rows);
    let mut ven = Vec::with_capacity(n_rows);
    let mut dir = Vec::with_capacity(n_rows);
    let mut qual = Vec::with_capacity(n_rows);
    let mut created = Vec::with_capacity(n_rows);
    let mut snap = Vec::with_capacity(n_rows);
    let mut hz: Vec<Option<i64>> = Vec::with_capacity(n_rows);
    let mut ep: Vec<Option<f64>> = Vec::with_capacity(n_rows);
    let mut xp: Vec<Option<f64>> = Vec::with_capacity(n_rows);
    let mut gr: Vec<Option<f64>> = Vec::with_capacity(n_rows);
    let mut nr: Vec<Option<f64>> = Vec::with_capacity(n_rows);
    let mut mfe: Vec<Option<f64>> = Vec::with_capacity(n_rows);
    let mut mae: Vec<Option<f64>> = Vec::with_capacity(n_rows);
    let mut hit: Vec<Option<u8>> = Vec::with_capacity(n_rows);

    for o in obs {
        // Every observation must carry the SAME identity (one fingerprint per
        // store/file — the footer and the no-overwrite key depend on it).
        debug_assert_eq!(o.identity.fingerprint(), identity_fp);
        let json = serde_json::to_string(&o.feature_snapshot)
            .map_err(|e| StorageError::Arrow(format!("snapshot serialize: {e}")))?;
        if o.outcomes.is_empty() {
            oid.push(o.observation_id);
            sid.push(o.identity.signal_id.clone());
            fv.push(o.identity.feature_version);
            dv.push(o.identity.data_schema_version);
            ph.push(o.identity.params_hash.clone());
            ch.push(o.identity.cost_model_hash.clone());
            ts.push(o.timestamp_ns);
            sym.push(o.symbol.0);
            ven.push(crate::layout::venue_code(o.venue));
            dir.push(direction_code(o.direction));
            qual.push(quality_code(o.quality));
            created.push(o.created_at_ns);
            snap.push(json);
            hz.push(None);
            ep.push(None);
            xp.push(None);
            gr.push(None);
            nr.push(None);
            mfe.push(None);
            mae.push(None);
            hit.push(None);
        } else {
            for oc in &o.outcomes {
                oid.push(o.observation_id);
                sid.push(o.identity.signal_id.clone());
                fv.push(o.identity.feature_version);
                dv.push(o.identity.data_schema_version);
                ph.push(o.identity.params_hash.clone());
                ch.push(o.identity.cost_model_hash.clone());
                ts.push(o.timestamp_ns);
                sym.push(o.symbol.0);
                ven.push(crate::layout::venue_code(o.venue));
                dir.push(direction_code(o.direction));
                qual.push(quality_code(o.quality));
                created.push(o.created_at_ns);
                snap.push(json.clone());
                hz.push(Some(oc.horizon_ns));
                ep.push(Some(oc.entry_price));
                xp.push(Some(oc.exit_price));
                gr.push(Some(oc.gross_return));
                nr.push(Some(oc.net_return));
                mfe.push(Some(oc.mfe));
                mae.push(Some(oc.mae));
                hit.push(Some(u8::from(oc.hit)));
            }
        }
    }

    let batch = RecordBatch::try_new(
        observation_schema(),
        vec![
            Arc::new(UInt64Array::from(oid)),
            Arc::new(StringArray::from(sid)),
            Arc::new(UInt16Array::from(fv)),
            Arc::new(UInt16Array::from(dv)),
            Arc::new(StringArray::from(ph)),
            Arc::new(StringArray::from(ch)),
            Arc::new(Int64Array::from(ts)),
            Arc::new(UInt32Array::from(sym)),
            Arc::new(UInt16Array::from(ven)),
            Arc::new(arrow::array::Int8Array::from(dir)),
            Arc::new(UInt8Array::from(qual)),
            Arc::new(Int64Array::from(created)),
            Arc::new(StringArray::from(snap)),
            Arc::new(Int64Array::from(hz)),
            Arc::new(Float64Array::from(ep)),
            Arc::new(Float64Array::from(xp)),
            Arc::new(Float64Array::from(gr)),
            Arc::new(Float64Array::from(nr)),
            Arc::new(Float64Array::from(mfe)),
            Arc::new(Float64Array::from(mae)),
            Arc::new(UInt8Array::from(hit)),
        ],
    )
    .map_err(|e| StorageError::Arrow(e.to_string()))?;

    let props = WriterProperties::builder()
        // SAFETY (CONV-13): zstd level 6 is in-range; try_new cannot fail.
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(6).unwrap()))
        .set_key_value_metadata(Some(vec![
            KeyValue::new(KV_IDENTITY_FINGERPRINT.into(), identity_fp.clone()),
            KeyValue::new(KV_OBSERVATION_SCHEMA_VER.into(), OBSERVATION_SCHEMA_VER.to_string()),
        ]))
        .build();

    let file = File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, observation_schema(), Some(props))
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .write(&batch)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .close()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    Ok(n_rows as u64)
}

/// W-6 no-overwrite guard: identical content is a no-op; divergent content on
/// the same path is a hard error.
pub fn write_observations_no_overwrite(
    path: &Path,
    obs: &[SignalObservation],
) -> Result<u64, StorageError> {
    if path.exists() {
        let existing = read_observations(path)?;
        if observations_content_hash(&existing) == observations_content_hash(obs) {
            return Ok(0);
        }
        return Err(StorageError::Parquet(format!(
            "refusing to overwrite {} with different observation content (W-6)",
            path.display()
        )));
    }
    write_observations(path, obs)
}

/// Partitioned write: one `{date}-{content_hash}.parquet` per UTC date of the
/// observations' OWN `timestamp_ns` (midnight-straddling batches land rows on
/// the days they happened). Layout:
/// `{root}/observations/{identity_fingerprint}/date={date}-{hash}.parquet`.
/// Returns the paths written.
pub fn partitioned_write(
    root: &Path,
    observations: &[SignalObservation],
) -> Result<Vec<PathBuf>, StorageError> {
    if observations.is_empty() {
        return Ok(Vec::new());
    }
    let fp = observations[0].identity.fingerprint();
    let base = root.join("observations").join(&fp);
    std::fs::create_dir_all(&base)?;
    let mut by_date: BTreeMap<String, Vec<SignalObservation>> = BTreeMap::new();
    for o in observations {
        by_date
            .entry(crate::feature_store::date_str(o.timestamp_ns))
            .or_default()
            .push(o.clone());
    }
    let mut out = Vec::new();
    for (d, group) in by_date {
        let hash = observations_content_hash(&group);
        let path = base.join(format!("date={d}-{hash:016x}.parquet"));
        write_observations_no_overwrite(&path, &group)?;
        out.push(path);
    }
    Ok(out)
}

/// Read observations back from a Parquet file written by this module.
pub fn read_observations(path: &Path) -> Result<Vec<SignalObservation>, StorageError> {
    let file = File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| StorageError::Parquet(e.to_string()))?
        .build()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    let mut out: Vec<SignalObservation> = Vec::new();
    // Group rows by observation_id: one observation may span k outcome rows.
    // Order is FIRST-SEEN row order (the writer's observation order), never a
    // numeric-id sort — deterministic and replay-faithful.
    let mut pending: BTreeMap<u64, usize> = BTreeMap::new();
    for batch in reader {
        let batch = batch.map_err(|e| StorageError::Arrow(e.to_string()))?;
        let oid = dc::<UInt64Array>(&batch, 0)?;
        let sid = dc::<StringArray>(&batch, 1)?;
        let fv = dc::<UInt16Array>(&batch, 2)?;
        let dv = dc::<UInt16Array>(&batch, 3)?;
        let ph = dc::<StringArray>(&batch, 4)?;
        let ch = dc::<StringArray>(&batch, 5)?;
        let ts = dc::<Int64Array>(&batch, 6)?;
        let sym = dc::<UInt32Array>(&batch, 7)?;
        let ven = dc::<UInt16Array>(&batch, 8)?;
        let dir = dc::<arrow::array::Int8Array>(&batch, 9)?;
        let qual = dc::<UInt8Array>(&batch, 10)?;
        let created = dc::<Int64Array>(&batch, 11)?;
        let snap = dc::<StringArray>(&batch, 12)?;
        let hz = dc::<Int64Array>(&batch, 13)?;
        let ep = dc::<Float64Array>(&batch, 14)?;
        let xp = dc::<Float64Array>(&batch, 15)?;
        let gr = dc::<Float64Array>(&batch, 16)?;
        let nr = dc::<Float64Array>(&batch, 17)?;
        let mfe = dc::<Float64Array>(&batch, 18)?;
        let mae = dc::<Float64Array>(&batch, 19)?;
        let hit = dc::<UInt8Array>(&batch, 20)?;
        for i in 0..batch.num_rows() {
            let id = oid.value(i);
            let venue = crate::layout::venue_from_code(ven.value(i))
                .ok_or_else(|| StorageError::Arrow("unknown venue code".into()))?;
            let snapshot: BTreeMap<String, f64> = serde_json::from_str(snap.value(i))
                .map_err(|e| StorageError::Arrow(format!("snapshot parse: {e}")))?;
            let idx = match pending.get(&id) {
                Some(&idx) => idx,
                None => {
                    let idx = out.len();
                    out.push(SignalObservation {
                        observation_id: id,
                        identity: mp_features::signal_identity::SignalResearchIdentity {
                            signal_id: sid.value(i).to_owned(),
                            feature_version: fv.value(i),
                            data_schema_version: dv.value(i),
                            params_hash: ph.value(i).to_owned(),
                            cost_model_hash: ch.value(i).to_owned(),
                        },
                        timestamp_ns: ts.value(i),
                        symbol: SymbolId(sym.value(i)),
                        venue,
                        direction: direction_from_code(dir.value(i)),
                        feature_snapshot: snapshot,
                        quality: quality_from_code(qual.value(i)),
                        created_at_ns: created.value(i),
                        outcomes: Vec::new(),
                    });
                    pending.insert(id, idx);
                    idx
                }
            };
            if !hz.is_null(i) {
                out[idx].outcomes.push(ObservationOutcome {
                    horizon_ns: hz.value(i),
                    entry_price: ep.value(i),
                    exit_price: xp.value(i),
                    gross_return: gr.value(i),
                    net_return: nr.value(i),
                    mfe: mfe.value(i),
                    mae: mae.value(i),
                    hit: hit.value(i) != 0,
                });
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::Venue;
    use mp_features::observation::{Direction, ObservationEngine, ObservationOutcome};
    use mp_features::signal_identity::SignalResearchIdentity;

    const T0: i64 = 1_784_505_600_000_000_000; // fixed deterministic instant

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("obsstore-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn sample_obs(n: u64, with_outcomes: bool) -> Vec<SignalObservation> {
        let mut e = ObservationEngine::with_default_quality(
            SignalResearchIdentity::new("whale_imb", 1, "params-1", "cost-1"),
            T0,
        );
        for i in 0..5 {
            e.on_feature_update(&mp_features::FeatureUpdate {
                symbol: SymbolId(1),
                feature: SymbolId(9),
                name: "funding.rate".into(),
                venue: Venue::Bybit,
                value: 0.0001 * i as f64,
                ts_ns: T0 + i,
                ver: 1,
            });
        }
        let mut obs = Vec::new();
        for i in 0..n {
            let mut o = e
                .record(
                    T0 + 10 + i as i64,
                    SymbolId(1),
                    Venue::Bybit,
                    Direction::Long,
                    BTreeMap::from([("funding.rate".into(), 0.0004)]),
                )
                .expect("healthy");
            if with_outcomes {
                o.outcomes.push(ObservationOutcome {
                    horizon_ns: 3_600_000_000_000,
                    entry_price: 100.0,
                    exit_price: 101.0,
                    gross_return: 0.01,
                    net_return: 0.009,
                    mfe: 0.02,
                    mae: -0.005,
                    hit: true,
                });
            }
            obs.push(o);
        }
        obs
    }

    #[test]
    fn rel_5_parquet_roundtrip_preserves_observations() {
        let dir = tmp("roundtrip");
        let obs = sample_obs(3, true);
        let path = dir.join("test.parquet");
        let n = write_observations(&path, &obs).unwrap();
        assert_eq!(n, 3, "one row per outcome");
        let back = read_observations(&path).unwrap();
        assert_eq!(back, obs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rel_5_roundtrip_without_outcomes_keeps_null_columns() {
        let dir = tmp("nooutcome");
        let obs = sample_obs(2, false);
        let path = dir.join("test.parquet");
        write_observations(&path, &obs).unwrap();
        let back = read_observations(&path).unwrap();
        assert_eq!(back, obs);
        assert!(back.iter().all(|o| o.outcomes.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rel_8_content_hash_is_deterministic_and_w6_guard_holds() {
        let dir = tmp("w6");
        let obs = sample_obs(2, true);
        let path = dir.join("obs.parquet");
        write_observations(&path, &obs).unwrap();
        // Identical re-write: no-op, not an error.
        assert_eq!(write_observations_no_overwrite(&path, &obs).unwrap(), 0);
        // Divergent content on the same path: hard error (W-6).
        let mut changed = obs.clone();
        changed[0].timestamp_ns += 1;
        assert!(write_observations_no_overwrite(&path, &changed).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rel_10_partitioned_write_and_read_back() {
        let dir = tmp("partitioned");
        let mut obs = sample_obs(2, true);
        // Move one observation across midnight (deterministic dates).
        obs[1].timestamp_ns = T0 + 86_400_000_000_000_i64;
        let paths = partitioned_write(&dir, &obs).unwrap();
        assert_eq!(paths.len(), 2, "one file per date");
        let mut all = Vec::new();
        for p in &paths {
            all.extend(read_observations(p).unwrap());
        }
        // Every observation comes back exactly once, with the identity intact.
        let mut expect: Vec<u64> = obs.iter().map(|o| o.observation_id).collect();
        let mut got: Vec<u64> = all.iter().map(|o| o.observation_id).collect();
        expect.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, expect);
        assert!(all.iter().all(|o| o.identity.fingerprint() == obs[0].identity.fingerprint()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}