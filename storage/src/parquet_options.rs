//! Options market data ↔ Parquet (spec 031, OPT-5). One file per
//! venue/symbol/date, sorted by `recv_ts_ns`, zstd-compressed, with the same
//! footer KV metadata contract as trades (STO-8). Partition:
//! `cold/options/venue=…/symbol=…/date=…/part-000.parquet` (separate stream
//! from trades, W-6). Manifest `sampled: false` — Deribit public book/trades
//! are not throttled like Binance liq (OPT-5).
//!
//! All three option event kinds (`OptionTrade` / `OptionBook` /
//! `OptionTicker`) share one flat schema (spec 031 "flat-with-metadata-columns"
//! decision): a `kind` discriminator column plus nullable per-kind columns.
//! Book levels are stored as JSON text columns (`bids`/`asks`) — DuckDB/Polars
//! can `json_extract` them, and the verbatim raw frames (OPT-3) always allow
//! full re-materialization. Missing values use null (greeks absent ⇒ null).

use crate::StorageError;
use arrow::array::{
    Array, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, UInt32Array,
    UInt64Array, UInt8Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use mp_core::{EventEnvelope, MarketEvent, OptionKind, Side, SymbolId};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// `kind` discriminator values (column 5).
pub const KIND_TRADE: u8 = 0;
pub const KIND_BOOK: u8 = 1;
pub const KIND_TICKER: u8 = 2;

fn options_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("symbol_id", DataType::UInt32, false),
        Field::new("venue_code", DataType::UInt16, false),
        Field::new("exch_ts_ns", DataType::Int64, false),
        Field::new("recv_ts_ns", DataType::Int64, false),
        Field::new("stream_seq", DataType::UInt64, false),
        Field::new("kind", DataType::UInt8, false), // 0=Trade 1=Book 2=Ticker
        // OptionLeg metadata (OPT-2) — every option row carries the full leg.
        Field::new("underlying", DataType::Utf8, false),
        Field::new("strike", DataType::Float64, false),
        Field::new("expiry_ts_ns", DataType::Int64, false),
        Field::new("option_kind", DataType::UInt8, false), // 0=Call 1=Put
        // Trade-only.
        Field::new("price", DataType::Float64, true),
        Field::new("qty", DataType::Float64, true),
        Field::new("side", DataType::UInt8, true), // 0=Buy 1=Sell
        Field::new("trade_id", DataType::UInt64, true),
        // Book-only (levels as JSON text; raw frames verbatim via OPT-3).
        Field::new("change_id", DataType::UInt64, true),
        Field::new("is_snapshot", DataType::Boolean, true),
        Field::new("bids", DataType::Utf8, true),
        Field::new("asks", DataType::Utf8, true),
        // Ticker-only (greeks nullable; absent ⇒ null).
        Field::new("mark_iv", DataType::Float64, true),
        Field::new("mark_price", DataType::Float64, true),
        Field::new("underlying_price", DataType::Float64, true),
        Field::new("open_interest", DataType::Float64, true),
        Field::new("delta", DataType::Float64, true),
        Field::new("gamma", DataType::Float64, true),
        Field::new("theta", DataType::Float64, true),
        Field::new("vega", DataType::Float64, true),
    ]))
}

fn opt_kind_code(k: OptionKind) -> u8 {
    match k {
        OptionKind::Call => 0,
        OptionKind::Put => 1,
    }
}

fn opt_kind_from_code(c: u8) -> Option<OptionKind> {
    match c {
        0 => Some(OptionKind::Call),
        1 => Some(OptionKind::Put),
        _ => None,
    }
}

fn levels_json(levels: &mp_core::Levels) -> String {
    // SAFETY: Levels is a SmallVec of (f64, f64) plain data; serde_json
    // cannot fail on it (CONV-13). Non-finite levels would serialize as
    // null, which fail-closed on read (never a real price).
    serde_json::to_string(&levels.iter().copied().collect::<Vec<_>>())
        .expect("book levels serialize")
}

/// Write option events (one venue/symbol/date, sorted by recv) to a Parquet
/// file with footer metadata. Non-option events are ignored.
pub fn write_options(
    path: &Path,
    events: &[EventEnvelope],
    compactor_version: &str,
    source_log_hash: &str,
) -> Result<u64, StorageError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (
        mut sym,
        mut ven,
        mut ex,
        mut rv,
        mut sq,
        mut kind,
        mut underlying,
        mut strike,
        mut expiry,
        mut opt_kind,
        mut price,
        mut qty,
        mut side,
        mut trade_id,
        mut change_id,
        mut is_snapshot,
        mut bids,
        mut asks,
        mut mark_iv,
        mut mark_price,
        mut underlying_price,
        mut open_interest,
        mut delta,
        mut gamma,
        mut theta,
        mut vega,
    ) = (
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
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    for e in events {
        match &e.body {
            MarketEvent::OptionTrade {
                leg,
                price: p,
                qty: q,
                side: sd,
                trade_id: tid,
            } => {
                sym.push(e.symbol.0);
                ven.push(crate::layout::venue_code(e.venue));
                ex.push(e.exch_ts_ns);
                rv.push(e.recv_ts_ns);
                sq.push(e.stream_seq);
                kind.push(KIND_TRADE);
                underlying.push(leg.underlying.clone());
                strike.push(leg.strike);
                expiry.push(leg.expiry_ts_ns);
                opt_kind.push(opt_kind_code(leg.kind));
                price.push(Some(*p));
                qty.push(Some(*q));
                side.push(Some(match sd {
                    Side::Buy => 0u8,
                    Side::Sell => 1u8,
                }));
                trade_id.push(Some(*tid));
                change_id.push(None);
                is_snapshot.push(None);
                bids.push(None);
                asks.push(None);
                mark_iv.push(None);
                mark_price.push(None);
                underlying_price.push(None);
                open_interest.push(None);
                delta.push(None);
                gamma.push(None);
                theta.push(None);
                vega.push(None);
            }
            MarketEvent::OptionBook {
                leg,
                bids: b,
                asks: a,
                change_id: cid,
                is_snapshot: snap,
            } => {
                sym.push(e.symbol.0);
                ven.push(crate::layout::venue_code(e.venue));
                ex.push(e.exch_ts_ns);
                rv.push(e.recv_ts_ns);
                sq.push(e.stream_seq);
                kind.push(KIND_BOOK);
                underlying.push(leg.underlying.clone());
                strike.push(leg.strike);
                expiry.push(leg.expiry_ts_ns);
                opt_kind.push(opt_kind_code(leg.kind));
                price.push(None);
                qty.push(None);
                side.push(None);
                trade_id.push(None);
                change_id.push(Some(*cid));
                is_snapshot.push(Some(*snap));
                bids.push(Some(levels_json(b)));
                asks.push(Some(levels_json(a)));
                mark_iv.push(None);
                mark_price.push(None);
                underlying_price.push(None);
                open_interest.push(None);
                delta.push(None);
                gamma.push(None);
                theta.push(None);
                vega.push(None);
            }
            MarketEvent::OptionTicker {
                leg,
                mark_iv: mi,
                mark_price: mp,
                underlying_price: up,
                open_interest: oi,
                greeks,
            } => {
                sym.push(e.symbol.0);
                ven.push(crate::layout::venue_code(e.venue));
                ex.push(e.exch_ts_ns);
                rv.push(e.recv_ts_ns);
                sq.push(e.stream_seq);
                kind.push(KIND_TICKER);
                underlying.push(leg.underlying.clone());
                strike.push(leg.strike);
                expiry.push(leg.expiry_ts_ns);
                opt_kind.push(opt_kind_code(leg.kind));
                price.push(None);
                qty.push(None);
                side.push(None);
                trade_id.push(None);
                change_id.push(None);
                is_snapshot.push(None);
                bids.push(None);
                asks.push(None);
                mark_iv.push(Some(*mi));
                mark_price.push(Some(*mp));
                underlying_price.push(Some(*up));
                open_interest.push(Some(*oi));
                delta.push(greeks.map(|g| g.delta));
                gamma.push(greeks.map(|g| g.gamma));
                theta.push(greeks.map(|g| g.theta));
                vega.push(greeks.map(|g| g.vega));
            }
            _ => continue,
        }
    }
    let n = sym.len() as u64;

    let batch = RecordBatch::try_new(
        options_schema(),
        vec![
            Arc::new(UInt32Array::from(sym)),
            Arc::new(arrow::array::UInt16Array::from(ven)),
            Arc::new(Int64Array::from(ex)),
            Arc::new(Int64Array::from(rv)),
            Arc::new(UInt64Array::from(sq)),
            Arc::new(UInt8Array::from(kind)),
            Arc::new(StringArray::from(underlying)),
            Arc::new(Float64Array::from(strike)),
            Arc::new(Int64Array::from(expiry)),
            Arc::new(UInt8Array::from(opt_kind)),
            Arc::new(Float64Array::from(price)),
            Arc::new(Float64Array::from(qty)),
            Arc::new(UInt8Array::from(side)),
            Arc::new(UInt64Array::from(trade_id)),
            Arc::new(UInt64Array::from(change_id)),
            Arc::new(BooleanArray::from(is_snapshot)),
            Arc::new(StringArray::from(bids)),
            Arc::new(StringArray::from(asks)),
            Arc::new(Float64Array::from(mark_iv)),
            Arc::new(Float64Array::from(mark_price)),
            Arc::new(Float64Array::from(underlying_price)),
            Arc::new(Float64Array::from(open_interest)),
            Arc::new(Float64Array::from(delta)),
            Arc::new(Float64Array::from(gamma)),
            Arc::new(Float64Array::from(theta)),
            Arc::new(Float64Array::from(vega)),
        ],
    )
    .map_err(|e| StorageError::Arrow(e.to_string()))?;

    let file = File::create(path)?;
    let props = crate::parquet_trades::writer_properties(compactor_version, source_log_hash);
    let mut writer = ArrowWriter::try_new(file, options_schema(), Some(props))
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .write(&batch)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .close()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    Ok(n)
}

/// Read options Parquet back into `EventEnvelope`s (OPT-5 roundtrip).
pub fn read_options(path: &Path) -> Result<Vec<EventEnvelope>, StorageError> {
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
        let kind = col_u8(&batch, 5)?;
        let underlying = col_str(&batch, 6)?;
        let strike = col_f64(&batch, 7)?;
        let expiry = col_i64(&batch, 8)?;
        let opt_kind = col_u8(&batch, 9)?;
        let price = col_f64(&batch, 10)?;
        let qty = col_f64(&batch, 11)?;
        let side = col_u8(&batch, 12)?;
        let trade_id = col_u64(&batch, 13)?;
        let change_id = col_u64(&batch, 14)?;
        let is_snapshot = col_bool(&batch, 15)?;
        let bids = col_str(&batch, 16)?;
        let asks = col_str(&batch, 17)?;
        let mark_iv = col_f64(&batch, 18)?;
        let mark_price = col_f64(&batch, 19)?;
        let underlying_price = col_f64(&batch, 20)?;
        let open_interest = col_f64(&batch, 21)?;
        let delta = col_f64(&batch, 22)?;
        let gamma = col_f64(&batch, 23)?;
        let theta = col_f64(&batch, 24)?;
        let vega = col_f64(&batch, 25)?;

        for i in 0..batch.num_rows() {
            let venue = crate::layout::venue_from_code(ven.value(i))
                .ok_or_else(|| StorageError::Arrow("unknown venue code".into()))?;
            let leg = mp_core::OptionLeg {
                underlying: underlying.value(i).to_owned(),
                strike: strike.value(i),
                expiry_ts_ns: expiry.value(i),
                kind: opt_kind_from_code(opt_kind.value(i))
                    .ok_or_else(|| StorageError::Arrow("bad option kind code".into()))?,
            };
            let body = match kind.value(i) {
                KIND_TRADE => MarketEvent::OptionTrade {
                    leg,
                    price: price.value(i),
                    qty: qty.value(i),
                    side: if side.value(i) == 0 {
                        Side::Buy
                    } else {
                        Side::Sell
                    },
                    trade_id: trade_id.value(i),
                },
                KIND_BOOK => {
                    // Levels decode: non-finite entries are impossible (JSON
                    // serializes them as null, never real prices — fail-closed).
                    // A Book row always writes bids/asks as Some; null here
                    // means a corrupted file, not absent data (CONV-8).
                    let decode = |s: Option<&str>| -> Result<mp_core::Levels, StorageError> {
                        let json = s.ok_or_else(|| {
                            StorageError::Arrow(
                                "OptionBook row with null bids/asks (corrupt)".into(),
                            )
                        })?;
                        let levels: Vec<(f64, f64)> = serde_json::from_str(json).map_err(|e| {
                            StorageError::Arrow(format!("bad book levels json: {e}"))
                        })?;
                        Ok(levels
                            .into_iter()
                            .filter(|(p, q)| p.is_finite() && q.is_finite())
                            .collect())
                    };
                    let bids = if bids.is_null(i) {
                        None
                    } else {
                        Some(bids.value(i))
                    };
                    let asks = if asks.is_null(i) {
                        None
                    } else {
                        Some(asks.value(i))
                    };
                    MarketEvent::OptionBook {
                        leg,
                        bids: decode(bids)?,
                        asks: decode(asks)?,
                        // A Book row always writes change_id/is_snapshot as
                        // Some; null means corruption, fail closed (CONV-8).
                        change_id: if change_id.is_null(i) {
                            return Err(StorageError::Arrow(
                                "OptionBook row with null change_id (corrupt)".into(),
                            ));
                        } else {
                            change_id.value(i)
                        },
                        is_snapshot: if is_snapshot.is_null(i) {
                            return Err(StorageError::Arrow(
                                "OptionBook row with null is_snapshot (corrupt)".into(),
                            ));
                        } else {
                            is_snapshot.value(i)
                        },
                    }
                }
                KIND_TICKER => {
                    let greeks = if delta.is_null(i) {
                        None
                    } else {
                        Some(mp_core::OptionGreeks {
                            delta: delta.value(i),
                            gamma: gamma.value(i),
                            theta: theta.value(i),
                            vega: vega.value(i),
                        })
                    };
                    MarketEvent::OptionTicker {
                        leg,
                        mark_iv: mark_iv.value(i),
                        mark_price: mark_price.value(i),
                        underlying_price: underlying_price.value(i),
                        open_interest: open_interest.value(i),
                        greeks,
                    }
                }
                other => return Err(StorageError::Arrow(format!("bad option kind {other}"))),
            };
            out.push(EventEnvelope::new(
                venue,
                SymbolId(sym.value(i)),
                ex.value(i),
                rv.value(i),
                sq.value(i),
                body,
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
col!(col_u8, UInt8Array);
col!(col_i64, Int64Array);
col!(col_f64, Float64Array);
col!(col_str, StringArray);
col!(col_bool, BooleanArray);
