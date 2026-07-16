//! Trades ↔ Parquet (STO-1, STO-4, STO-8). One file per venue/symbol/date,
//! sorted by `recv_ts_ns`, zstd-compressed, with footer KV metadata
//! (`schema_ver`, `compactor_version`, `source_log_hash`). Real Parquet so
//! research (Polars/DuckDB) reads the exact same bytes.

use crate::StorageError;
use arrow::array::{Float64Array, UInt32Array, UInt64Array, UInt8Array};
use arrow::array::{Int64Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema};
use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::format::KeyValue;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// Footer metadata keys (STO-8).
pub const KV_SCHEMA_VER: &str = "schema_ver";
pub const KV_COMPACTOR_VERSION: &str = "compactor_version";
pub const KV_SOURCE_LOG_HASH: &str = "source_log_hash";

fn trades_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("symbol_id", DataType::UInt32, false),
        Field::new("venue_code", DataType::UInt16, false),
        Field::new("exch_ts_ns", DataType::Int64, false),
        Field::new("recv_ts_ns", DataType::Int64, false),
        Field::new("stream_seq", DataType::UInt64, false),
        Field::new("price", DataType::Float64, false),
        Field::new("qty", DataType::Float64, false),
        Field::new("side", DataType::UInt8, false), // 0=Buy, 1=Sell
        Field::new("trade_id", DataType::UInt64, false),
    ]))
}

/// Write trade events (already for one venue/symbol/date, sorted by recv) to a
/// Parquet file with footer metadata. Non-trade events are ignored.
pub fn write_trades(
    path: &Path,
    events: &[EventEnvelope],
    compactor_version: &str,
    source_log_hash: &str,
) -> Result<u64, StorageError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (mut sym, mut ven, mut ex, mut rv, mut sq, mut px, mut qy, mut sd, mut ti) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    for e in events {
        if let MarketEvent::Trade {
            price,
            qty,
            side,
            trade_id,
        } = e.body
        {
            sym.push(e.symbol.0);
            ven.push(crate::layout::venue_code(e.venue));
            ex.push(e.exch_ts_ns);
            rv.push(e.recv_ts_ns);
            sq.push(e.stream_seq);
            px.push(price);
            qy.push(qty);
            sd.push(match side {
                Side::Buy => 0u8,
                Side::Sell => 1u8,
            });
            ti.push(trade_id);
        }
    }
    let n = sym.len() as u64;

    let batch = RecordBatch::try_new(
        trades_schema(),
        vec![
            Arc::new(UInt32Array::from(sym)),
            Arc::new(arrow::array::UInt16Array::from(ven)),
            Arc::new(Int64Array::from(ex)),
            Arc::new(Int64Array::from(rv)),
            Arc::new(UInt64Array::from(sq)),
            Arc::new(Float64Array::from(px)),
            Arc::new(Float64Array::from(qy)),
            Arc::new(UInt8Array::from(sd)),
            Arc::new(UInt64Array::from(ti)),
        ],
    )
    .map_err(|e| StorageError::Arrow(e.to_string()))?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3).unwrap()))
        .set_key_value_metadata(Some(vec![
            KeyValue::new(KV_SCHEMA_VER.into(), mp_core::SCHEMA_VER.to_string()),
            KeyValue::new(KV_COMPACTOR_VERSION.into(), compactor_version.to_string()),
            KeyValue::new(KV_SOURCE_LOG_HASH.into(), source_log_hash.to_string()),
        ]))
        .build();

    let file = File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, trades_schema(), Some(props))
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .write(&batch)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .close()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    Ok(n)
}

/// Read a `source_log_hash` from a trades Parquet footer without scanning data
/// (STO-1 idempotency check).
pub fn read_source_hash(path: &Path) -> Result<Option<String>, StorageError> {
    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    let kv = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .and_then(|kvs| {
            kvs.iter()
                .find(|kv| kv.key == KV_SOURCE_LOG_HASH)
                .and_then(|kv| kv.value.clone())
        });
    Ok(kv)
}

/// Read trades Parquet back into `EventEnvelope`s (STO-4).
pub fn read_trades(path: &Path) -> Result<Vec<EventEnvelope>, StorageError> {
    let file = File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| StorageError::Parquet(e.to_string()))?
        .build()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;

    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| StorageError::Arrow(e.to_string()))?;
        let sym = col_u32(&batch, 0)?;
        let ven = col_u16(&batch, 1)?;
        let ex = col_i64(&batch, 2)?;
        let rv = col_i64(&batch, 3)?;
        let sq = col_u64(&batch, 4)?;
        let px = col_f64(&batch, 5)?;
        let qy = col_f64(&batch, 6)?;
        let sd = col_u8(&batch, 7)?;
        let ti = col_u64(&batch, 8)?;
        for i in 0..batch.num_rows() {
            let venue = crate::layout::venue_from_code(ven.value(i))
                .ok_or_else(|| StorageError::Arrow("unknown venue code".into()))?;
            let side = if sd.value(i) == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            out.push(EventEnvelope::new(
                venue,
                SymbolId(sym.value(i)),
                ex.value(i),
                rv.value(i),
                sq.value(i),
                MarketEvent::Trade {
                    price: px.value(i),
                    qty: qy.value(i),
                    side,
                    trade_id: ti.value(i),
                },
            ));
        }
    }
    Ok(out)
}

// Column downcast helpers.
macro_rules! col {
    ($name:ident, $ty:ty) => {
        fn $name(b: &RecordBatch, i: usize) -> Result<&$ty, StorageError> {
            b.column(i)
                .as_any()
                .downcast_ref::<$ty>()
                .ok_or_else(|| StorageError::Arrow(format!("bad column type at {i}")))
        }
    };
}
col!(col_u32, UInt32Array);
col!(col_u16, arrow::array::UInt16Array);
col!(col_u64, UInt64Array);
col!(col_u8, UInt8Array);
col!(col_i64, Int64Array);
col!(col_f64, Float64Array);
