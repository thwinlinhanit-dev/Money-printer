//! IBIT ↔ Deribit cross-market daily table (spec 040, IBI-10).
//!
//! One row per UTC day: each venue's closing IV/net-delta aggregates, the IV
//! divergence, and the flow lead-lag correlations at lags 0..2. Written by
//! the offline researcher (mp-materialize path) after replaying both venues'
//! logs through the [`crate::ibit_cross`-style] feature; stored as a flat
//! Parquet table at `cold/ibit_cross/date=<date>.parquet`.
//!
//! Nullable correlation columns mean "not yet computable" (below the overlap
//! minimum) — never zero, which would be a real value.

use crate::StorageError;
use arrow::array::{Array, Float64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use mp_core::EventEnvelope;
use mp_features::ibit_cross::{IbitCrossDaily, IbitCrossParams};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// One day of cross-market aggregates (spec 040 IBI-10 column contract).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LeadLagRow {
    /// UTC date, `YYYY-MM-DD`.
    pub date: String,
    pub ibit_iv_atm: f64,
    pub deribit_iv_atm: f64,
    pub ibit_net_delta: f64,
    pub deribit_net_delta: f64,
    pub iv_divergence: f64,
    pub flow_corr_lag0: Option<f64>,
    pub flow_corr_lag1: Option<f64>,
    pub flow_corr_lag2: Option<f64>,
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("date", DataType::Utf8, false),
        Field::new("ibit_iv_atm", DataType::Float64, true),
        Field::new("deribit_iv_atm", DataType::Float64, true),
        Field::new("ibit_net_delta", DataType::Float64, true),
        Field::new("deribit_net_delta", DataType::Float64, true),
        Field::new("iv_divergence", DataType::Float64, true),
        Field::new("flow_corr_lag0", DataType::Float64, true),
        Field::new("flow_corr_lag1", DataType::Float64, true),
        Field::new("flow_corr_lag2", DataType::Float64, true),
    ]))
}

fn opt(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite())
}

/// Write rows to one Parquet file (sorted-by-date is the CALLER's contract —
/// append-only research table, W-6).
pub fn write_leadlag(path: &Path, rows: &[LeadLagRow]) -> Result<u64, StorageError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let n = rows.len();
    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.date.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|r| opt(Some(r.ibit_iv_atm)))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|r| opt(Some(r.deribit_iv_atm)))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|r| opt(Some(r.ibit_net_delta)))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|r| opt(Some(r.deribit_net_delta)))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|r| opt(Some(r.iv_divergence)))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.flow_corr_lag0).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.flow_corr_lag1).collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.flow_corr_lag2).collect::<Vec<_>>(),
            )),
        ],
    )
    .map_err(|e| StorageError::Arrow(e.to_string()))?;
    let file = File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, batch.schema(), None)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .write(&batch)
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    writer
        .close()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    Ok(n as u64)
}

/// Read a lead-lag table back.
pub fn read_leadlag(path: &Path) -> Result<Vec<LeadLagRow>, StorageError> {
    let file = File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| StorageError::Parquet(e.to_string()))?
        .build()
        .map_err(|e| StorageError::Parquet(e.to_string()))?;
    let mut out = Vec::new();
    for batch in reader {
        let b = batch.map_err(|e| StorageError::Parquet(e.to_string()))?;
        let col = |name: &str| {
            b.column_by_name(name)
                .expect("schema column")
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
        };
        let dates = b
            .column_by_name("date")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let c0 = col("flow_corr_lag0");
        let c1 = col("flow_corr_lag1");
        let c2 = col("flow_corr_lag2");
        for i in 0..b.num_rows() {
            out.push(LeadLagRow {
                date: dates.value(i).to_owned(),
                ibit_iv_atm: col("ibit_iv_atm").value(i),
                deribit_iv_atm: col("deribit_iv_atm").value(i),
                ibit_net_delta: col("ibit_net_delta").value(i),
                deribit_net_delta: col("deribit_net_delta").value(i),
                iv_divergence: col("iv_divergence").value(i),
                flow_corr_lag0: c0.is_valid(i).then(|| c0.value(i)),
                flow_corr_lag1: c1.is_valid(i).then(|| c1.value(i)),
                flow_corr_lag2: c2.is_valid(i).then(|| c2.value(i)),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibi_10_leadlag_table_roundtrip_with_null_correlations() {
        let dir = std::env::temp_dir().join(format!("ibi10-{}", std::process::id()));
        let path = dir.join("cold/ibit_cross/date=2026-08-22.parquet");
        let rows = vec![
            LeadLagRow {
                date: "2026-08-21".into(),
                ibit_iv_atm: 0.62,
                deribit_iv_atm: 0.55,
                ibit_net_delta: 50_000.0,
                deribit_net_delta: 40.0,
                iv_divergence: 0.07,
                flow_corr_lag0: None, // below overlap minimum → null, not 0
                flow_corr_lag1: None,
                flow_corr_lag2: None,
            },
            LeadLagRow {
                date: "2026-08-22".into(),
                ibit_iv_atm: 0.60,
                deribit_iv_atm: 0.58,
                ibit_net_delta: 51_200.0,
                deribit_net_delta: 41.5,
                iv_divergence: 0.02,
                flow_corr_lag0: Some(0.42),
                flow_corr_lag1: Some(0.17),
                flow_corr_lag2: Some(-0.05),
            },
        ];
        let written = write_leadlag(&path, &rows).unwrap();
        assert_eq!(written, 2);
        let back = read_leadlag(&path).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].date, "2026-08-21");
        assert!((back[0].iv_divergence - 0.07).abs() < 1e-12);
        // Nulls survive as nulls — a missing correlation is not a zero.
        assert_eq!(back[0].flow_corr_lag0, None);
        assert_eq!(back[1].flow_corr_lag2, Some(-0.05));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---- IBI-10: offline row builder -------------------------------------------

/// UTC day index → `YYYY-MM-DD` (Hinnant civil_from_days; the inverse of the
/// collector's days_from_civil in `mp_collectors::ibit`).
fn date_string_from_day_index(day: i64) -> String {
    let z = day + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Replay events (canonical merged order — caller's contract, MAT-5) through
/// the cross-market daily aggregator and collect one [`LeadLagRow`] per
/// completed PAIRED day. Days with unusable IV closes (no finite OI-weighted
/// print) are skipped; correlation columns snapshot the history at each day's
/// completion (null while below `min_overlap_days`).
///
/// Errors from the event stream fail closed (StorageError::Parquet carries
/// the message — variant reuse for a generic stream failure).
pub fn build_rows<E: std::fmt::Display>(
    mut events: impl Iterator<Item = Result<EventEnvelope, E>>,
    params: &IbitCrossParams,
) -> Result<Vec<LeadLagRow>, StorageError> {
    let mut daily = IbitCrossDaily::new(params.clone());
    let mut rows = Vec::new();
    for ev in events.by_ref() {
        let ev = ev.map_err(|e| StorageError::Arrow(e.to_string()))?;
        if !daily.feed(&ev) {
            continue;
        }
        let Some(close) = daily.next_completed() else {
            continue;
        };
        // Skip days whose closing IVs are unusable (NaN) — a divergence or
        // delta built on them would be fiction.
        if !close.ibit_iv.is_finite() || !close.deriv_iv.is_finite() {
            continue;
        }
        rows.push(LeadLagRow {
            date: date_string_from_day_index(close.date),
            ibit_iv_atm: close.ibit_iv,
            deribit_iv_atm: close.deriv_iv,
            ibit_net_delta: close.ibit_net_delta,
            deribit_net_delta: close.deriv_net_delta,
            iv_divergence: close.divergence(),
            flow_corr_lag0: daily.corr(0),
            flow_corr_lag1: daily.corr(1),
            flow_corr_lag2: daily.corr(2),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod build_tests {
    use super::*;

    fn ticker(
        venue: mp_core::Venue,
        underlying: &str,
        day: i64,
        iv: f64,
        oi: f64,
        delta: f64,
    ) -> EventEnvelope {
        let sym = mp_core::SymbolId(match venue {
            mp_core::Venue::Cboe => 21,
            _ => 22,
        });
        let leg = mp_core::OptionLeg {
            underlying: underlying.to_owned(),
            strike: 50.0,
            expiry_ts_ns: 0,
            kind: mp_core::OptionKind::Call,
        };
        EventEnvelope::new(
            venue,
            sym,
            day * 86_400_000_000_000 + 3_600_000_000_000,
            day * 86_400_000_000_000 + 3_600_000_000_000,
            1,
            mp_core::MarketEvent::OptionTicker {
                leg,
                mark_iv: iv,
                mark_price: 5.0,
                underlying_price: 100.0,
                open_interest: oi,
                greeks: Some(mp_core::OptionGreeks {
                    delta,
                    gamma: 0.0,
                    theta: 0.0,
                    vega: 0.0,
                }),
            },
        )
    }

    fn ok(ev: EventEnvelope) -> Result<EventEnvelope, String> {
        Ok(ev)
    }

    #[test]
    fn ibi_10_build_rows_values_null_gating_and_nan_skip() {
        let params = IbitCrossParams {
            deriv_underlying: "BTC".into(),
            ibit_underlying: "IBIT".into(),
            min_overlap_days: 3,
            ibit_multiplier: 100.0,
            hist_days: 64,
        };
        let mut evs: Vec<Result<EventEnvelope, String>> = Vec::new();
        for d in 1..=3i64 {
            evs.push(ok(ticker(
                mp_core::Venue::Cboe,
                "IBIT",
                d,
                0.40,
                1000.0,
                0.5,
            )));
            evs.push(ok(ticker(
                mp_core::Venue::Deribit,
                "BTC",
                d,
                0.55,
                100.0,
                0.4,
            )));
        }
        // Day-4 pair completes day 3; give day 4 an UNUSABLE ibit close so
        // its row (completed later) is skipped entirely.
        // Day-4 pair completes day 3 (its own row completes on a later
        // feed) — rows exist for days 1 and 2 only.
        evs.push(ok(ticker(mp_core::Venue::Cboe, "IBIT", 4, 9.99, 1.0, 0.5)));
        evs.push(ok(ticker(
            mp_core::Venue::Deribit,
            "BTC",
            4,
            9.99,
            100.0,
            0.4,
        )));
        let rows = build_rows(evs.into_iter(), &params).unwrap();
        // Days 1-3 completed (the day-4 pair rolls day 3); day 4 stays open.
        assert_eq!(rows.len(), 3);
        // Day indexes 1..3 → 1970-01-02 / 03 / 04.
        assert_eq!(rows[0].date, "1970-01-02");
        assert_eq!(rows[1].date, "1970-01-03");
        assert_eq!(rows[2].date, "1970-01-04");
        // Day-1 close: iv_ibit 0.40 − iv_deriv 0.55; deltas 50_000 / 40.
        assert!((rows[0].iv_divergence - (0.40 - 0.55)).abs() < 1e-12);
        assert_eq!(rows[0].ibit_net_delta, 50_000.0);
        assert_eq!(rows[0].deribit_net_delta, 40.0);
        // Correlations below min_overlap_days=3 with 2 paired days → null.
        assert_eq!(rows[0].flow_corr_lag0, None);
    }

    #[test]
    fn ibi_10_date_string_from_day_index_known_values() {
        // Day 0 = 1970-01-01.
        assert_eq!(date_string_from_day_index(0), "1970-01-01");
        // 20660 = ? sanity via known anchor: 2026-01-01 is day 20454.
        assert_eq!(date_string_from_day_index(20_454), "2026-01-01");
        assert_eq!(date_string_from_day_index(20_484), "2026-01-31");
    }
}
