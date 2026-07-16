//! Bybit v5 public-stream normalizer (COL-5..8).
//!
//! Handles `publicTrade`, `orderbook`, `tickers` (funding/mark/OI), and
//! `liquidation` topics. Book sync uses Bybit's snapshot/delta `u` update-id:
//! a `type:"snapshot"` resets the book; a `delta` whose `u` is not
//! `prev_u + 1` is a gap → emit `Status::GapDetected`, drop deltas until the
//! next snapshot (COL-7).
//!
//! Message shapes are implemented from documented Bybit v5 (knowledge cutoff);
//! fixtures under `testdata/` are synthetic-representative. Replace them with
//! real captured frames when the live transport lands (COL-13; see Decisions).

use crate::book_sync::{BookSync, DeltaAction, SnapKind};
use crate::json::{f64_field, hash_str, i64_field, ms_to_ns, parse_pair_levels, u64_field};
use crate::normalize::{NormError, Normalizer};
use mp_core::{
    EventEnvelope, MarketEvent, Side, SnapshotReason, StatusKind, SymbolId, SymbolTable, Venue,
};
use serde_json::Value;
use std::collections::BTreeMap;

/// Stateful Bybit normalizer.
#[derive(Debug, Default)]
pub struct BybitNormalizer {
    symbols: SymbolTable,
    /// Per-symbol book sequence state — uses the shared `BookSync` (COL-7).
    books: BTreeMap<SymbolId, BookSync>,
    next_seq: u64,
}

impl BybitNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The symbol table built up during normalization (persist via EVT-8).
    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn sym(&mut self, venue_symbol: &str) -> SymbolId {
        self.symbols.intern_default(Venue::Bybit, venue_symbol)
    }
}

fn side_from(s: &str) -> Option<Side> {
    match s {
        "Buy" => Some(Side::Buy),
        "Sell" => Some(Side::Sell),
        _ => None,
    }
}

impl BybitNormalizer {
    fn on_trade(
        &mut self,
        recv_ts_ns: i64,
        data: &Value,
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let arr = data
            .as_array()
            .ok_or_else(|| NormError::Parse("trade data not array".into()))?;
        for t in arr {
            let sym = t
                .get("s")
                .and_then(|x| x.as_str())
                .ok_or_else(|| NormError::Parse("trade missing symbol".into()))?;
            let id = self.sym(sym);
            let price = f64_field(t, "p").ok_or_else(|| NormError::Parse("trade price".into()))?;
            let qty = f64_field(t, "v").ok_or_else(|| NormError::Parse("trade qty".into()))?;
            let side = t
                .get("S")
                .and_then(|x| x.as_str())
                .and_then(side_from)
                .ok_or_else(|| NormError::Parse("trade side".into()))?;
            let exch = i64_field(t, "T").map(ms_to_ns).unwrap_or(0);
            let trade_id = t
                .get("i")
                .and_then(|x| x.as_str())
                .map(hash_str)
                .unwrap_or(0);
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::Bybit,
                id,
                exch,
                recv_ts_ns,
                seq,
                MarketEvent::Trade {
                    price,
                    qty,
                    side,
                    trade_id,
                },
            ));
        }
        Ok(())
    }

    fn on_book(
        &mut self,
        recv_ts_ns: i64,
        msg_type: &str,
        exch_ts_ns: i64,
        data: &Value,
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let sym = data
            .get("s")
            .and_then(|x| x.as_str())
            .ok_or_else(|| NormError::Parse("book missing symbol".into()))?;
        let id = self.sym(sym);
        let u = u64_field(data, "u").ok_or_else(|| NormError::Parse("book missing u".into()))?;
        let bids = parse_pair_levels(data.get("b"))?;
        let asks = parse_pair_levels(data.get("a"))?;

        if msg_type == "snapshot" {
            let st = self.books.entry(id).or_default();
            let kind = st.on_snapshot(u);
            let reason = match kind {
                SnapKind::Init => SnapshotReason::Init,
                SnapKind::Resync => SnapshotReason::GapResync,
            };
            let depth = (bids.len() + asks.len()) as u16;
            out.push(EventEnvelope::new(
                Venue::Bybit,
                id,
                exch_ts_ns,
                recv_ts_ns,
                u,
                MarketEvent::BookSnapshot {
                    bids,
                    asks,
                    seq: u,
                    depth,
                    reason,
                },
            ));
            return Ok(());
        }

        // Delta: use shared BookSync with single-update-id range (u, u).
        let action = {
            let st = self.books.entry(id).or_default();
            st.on_delta(u, u)
        };
        match action {
            DeltaAction::Apply => {
                out.push(EventEnvelope::new(
                    Venue::Bybit,
                    id,
                    exch_ts_ns,
                    recv_ts_ns,
                    u,
                    MarketEvent::BookDelta {
                        bids,
                        asks,
                        first_seq: u,
                        last_seq: u,
                    },
                ));
            }
            DeltaAction::Gap => {
                // Sequence gap → signal (COL-7). BookSync already marked desynced.
                let seq = self.seq();
                out.push(EventEnvelope::new(
                    Venue::Bybit,
                    id,
                    exch_ts_ns,
                    recv_ts_ns,
                    seq,
                    MarketEvent::Status {
                        kind: StatusKind::GapDetected,
                        detail: format!(
                            "orderbook gap: expected u={}, got u={}",
                            self.books.get(&id).map(|b| b.expected_next()).unwrap_or(0),
                            u
                        ),
                    },
                ));
            }
            DeltaAction::Drop => {} // stale/uninitialized — drop silently
        }
        Ok(())
    }

    fn on_tickers(
        &mut self,
        recv_ts_ns: i64,
        exch_ts_ns: i64,
        data: &Value,
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let sym = data
            .get("symbol")
            .and_then(|x| x.as_str())
            .ok_or_else(|| NormError::Parse("tickers missing symbol".into()))?;
        let id = self.sym(sym);

        if let Some(rate) = f64_field(data, "fundingRate") {
            let next = i64_field(data, "nextFundingTime")
                .map(ms_to_ns)
                .unwrap_or(0);
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::Bybit,
                id,
                exch_ts_ns,
                recv_ts_ns,
                seq,
                // interval_s unknown from this stream (Bybit omits it here); 0 =
                // unknown. Downstream funding features don't require it.
                MarketEvent::Funding {
                    rate,
                    interval_s: 0,
                    next_funding_ts_ns: next,
                },
            ));
        }
        if let Some(mark) = f64_field(data, "markPrice") {
            let index = f64_field(data, "indexPrice").unwrap_or(f64::NAN);
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::Bybit,
                id,
                exch_ts_ns,
                recv_ts_ns,
                seq,
                MarketEvent::MarkPrice { mark, index },
            ));
        }
        if let Some(oi) = f64_field(data, "openInterest") {
            let oi_notional = f64_field(data, "openInterestValue").unwrap_or(f64::NAN);
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::Bybit,
                id,
                exch_ts_ns,
                recv_ts_ns,
                seq,
                MarketEvent::OpenInterest {
                    oi_contracts: oi,
                    oi_notional,
                },
            ));
        }
        Ok(())
    }

    fn on_liquidation(
        &mut self,
        recv_ts_ns: i64,
        exch_ts_ns: i64,
        data: &Value,
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        // Bybit may deliver a single object or an array; handle both.
        let items: Vec<&Value> = match data {
            Value::Array(a) => a.iter().collect(),
            v => vec![v],
        };
        for it in items {
            let sym = it
                .get("symbol")
                .or_else(|| it.get("s"))
                .and_then(|x| x.as_str())
                .ok_or_else(|| NormError::Parse("liquidation missing symbol".into()))?;
            let id = self.sym(sym);
            let price = f64_field(it, "price")
                .or_else(|| f64_field(it, "p"))
                .ok_or_else(|| NormError::Parse("liquidation price".into()))?;
            let qty = f64_field(it, "size")
                .or_else(|| f64_field(it, "v"))
                .ok_or_else(|| NormError::Parse("liquidation size".into()))?;
            let side = it
                .get("side")
                .or_else(|| it.get("S"))
                .and_then(|x| x.as_str())
                .and_then(side_from)
                .ok_or_else(|| NormError::Parse("liquidation side".into()))?;
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::Bybit,
                id,
                exch_ts_ns,
                recv_ts_ns,
                seq,
                MarketEvent::Liquidation { price, qty, side },
            ));
        }
        Ok(())
    }
}

impl Normalizer for BybitNormalizer {
    fn venue(&self) -> Venue {
        Venue::Bybit
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let v: Value =
            serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;

        // Subscription acks / pongs have no "topic" — ignore quietly.
        let Some(topic) = v.get("topic").and_then(|t| t.as_str()) else {
            return Ok(());
        };
        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("delta");
        let exch_ts_ns = i64_field(&v, "ts").map(ms_to_ns).unwrap_or(0);
        let data = v
            .get("data")
            .ok_or_else(|| NormError::Parse("message missing data".into()))?;

        if topic.starts_with("publicTrade.") {
            self.on_trade(recv_ts_ns, data, out)
        } else if topic.starts_with("orderbook.") {
            self.on_book(recv_ts_ns, msg_type, exch_ts_ns, data, out)
        } else if topic.starts_with("tickers.") {
            self.on_tickers(recv_ts_ns, exch_ts_ns, data, out)
        } else if topic.starts_with("liquidation.") || topic.starts_with("allLiquidation.") {
            self.on_liquidation(recv_ts_ns, exch_ts_ns, data, out)
        } else {
            Ok(()) // unknown topic: ignore
        }
    }

    fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    fn reset_books(&mut self) {
        for st in self.books.values_mut() {
            st.desync();
        }
    }
}
