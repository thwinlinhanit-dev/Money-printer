//! Whale positions ↔ Parquet (spec 028, WHL-6). One file per
//! venue/symbol/date, sorted by `recv_ts_ns`, zstd-compressed, with the same
//! footer KV metadata contract as trades (STO-8). Partition:
//! `cold/positions/venue=…/symbol=…/date=…/part-000.parquet` (separate stream
//! from trades, W-6). Manifest `sampled: false` — Hyperliquid positions are a
//! census, not a throttled sample (WHL-6).

use crate::StorageError;
use arrow::array::{Float64Array, Int64Array, RecordBatch, StringArray, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use mp_core::{EventEnvelope, MarketEvent, SymbolId};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

fn positions_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("symbol_id", DataType::UInt32, false),
        Field::new("venue_code", DataType::UInt16, false),
        Field::new("exch_ts_ns", DataType::Int64, false),
        Field::new("recv_ts_ns", DataType::Int64, false),
        Field::new("stream_seq", DataType::UInt64, false),
        Field::new("address", DataType::Utf8, false),
        Field::new("size", DataType::Float64, false),
        Field::new("entry", DataType::Float64, false),
        Field::new("leverage", DataType::Float64, false),
        Field::new("liq_price", DataType::Float64, false),
    ]))
}

/// Write `WhalePosition` events (one venue/symbol/date, sorted by recv) to a
/// Parquet file with footer metadata. Non-whale events are ignored.
pub fn write_positions(
    path: &Path,
    events: &[EventEnvelope],
    compactor_version: &str,
    source_log_hash: &str,
) -> Result<u64, StorageError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (mut sym, mut ven, mut ex, mut rv, mut sq, mut addr, mut size, mut entry, mut lev, mut liq) = (
        Vec::new(),
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
        if let MarketEvent::WhalePosition {
            address,
            size: s,
            entry: en,
            leverage,
            liq_price,
        } = &e.body
        {
            sym.push(e.symbol.0);
            ven.push(crate::layout::venue_code(e.venue));
            ex.push(e.exch_ts_ns);
            rv.push(e.recv_ts_ns);
            sq.push(e.stream_seq);
            addr.push(address.clone());
            size.push(*s);
            entry.push(*en);
            lev.push(*leverage);
            liq.push(*liq_price);
        }
    }
    let n = sym.len() as u64;

    let batch = RecordBatch::try_new(
        positions_schema(),
        vec![
            Arc::new(UInt32Array::from(sym)),
            Arc::new(arrow::array::UInt16Array::from(ven)),
            Arc::new(Int64Array::from(ex)),
            Arc::new(Int64Array::from(rv)),
            Arc::new(UInt64Array::from(sq)),
            Arc::new(StringArray::from(addr)),
            Arc::new(Float64Array::from(size)),
            Arc::new(Float64Array::from(entry)),
            Arc::new(Float64Array::from(lev)),
            Arc::new(Float64Array::from(liq)),
        ],
    )
    .map_err(|e| StorageError::Arrow(e.to_string()))?;

    let file = File::create(path)?;
    let props = crate::parquet_trades::writer_properties(compactor_version, source_log_hash);
    let mut writer = ArrowWriter::try_new(file, positions_schema(), Some(props))
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .write(&batch)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .close()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    Ok(n)
}

/// Read whale positions Parquet back into `EventEnvelope`s (WHL-6 roundtrip).
pub fn read_positions(path: &Path) -> Result<Vec<EventEnvelope>, StorageError> {
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
        let addr = col_str(&batch, 5)?;
        let size = col_f64(&batch, 6)?;
        let entry = col_f64(&batch, 7)?;
        let lev = col_f64(&batch, 8)?;
        let liq = col_f64(&batch, 9)?;
        for i in 0..batch.num_rows() {
            let venue = crate::layout::venue_from_code(ven.value(i))
                .ok_or_else(|| StorageError::Arrow("unknown venue code".into()))?;
            out.push(EventEnvelope::new(
                venue,
                SymbolId(sym.value(i)),
                ex.value(i),
                rv.value(i),
                sq.value(i),
                MarketEvent::WhalePosition {
                    address: addr.value(i).to_owned(),
                    size: size.value(i),
                    entry: entry.value(i),
                    leverage: lev.value(i),
                    liq_price: liq.value(i),
                },
            ));
        }
    }
    Ok(out)
}

// Column downcast helpers (mirrors parquet_trades).
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
col!(col_i64, Int64Array);
col!(col_f64, Float64Array);
col!(col_str, StringArray);
