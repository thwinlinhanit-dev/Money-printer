//! Kraken Futures public-stream normalizer. Feeds: `trade`, `book_snapshot` /
//! `book` (per-level deltas with monotonic `seq`), `ticker` (mark/funding/OI).
//!
//! Shapes from documented Kraken Futures; fixtures are synthetic-representative.

use crate::book_sync::{BookSync, DeltaAction, SnapKind};
use crate::json::*;
use crate::normalize::{NormError, Normalizer};
use mp_core::{
    EventEnvelope, Level, Levels, MarketEvent, Side, SnapshotReason, StatusKind, SymbolId,
    SymbolTable, Venue,
};
use serde_json::Value;
use smallvec::SmallVec;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct KrakenNormalizer {
    symbols: SymbolTable,
    books: BTreeMap<SymbolId, BookSync>,
    next_seq: u64,
}

impl KrakenNormalizer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }
    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }
    fn sym(&mut self, s: &str) -> SymbolId {
        self.symbols.intern_default(Venue::KrakenFutures, s)
    }
}

fn kr_side(s: &str) -> Side {
    if s == "sell" {
        Side::Sell
    } else {
        Side::Buy
    }
}

impl Normalizer for KrakenNormalizer {
    fn venue(&self) -> Venue {
        Venue::KrakenFutures
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let d: Value =
            serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;
        let feed = str_field(&d, "feed").unwrap_or("");
        let exch = i64_field(&d, "time").map(ms_to_ns).unwrap_or(0);

        match feed {
            "trade" => {
                let sym = str_field(&d, "product_id")
                    .ok_or_else(|| NormError::Parse("kr product_id".into()))?;
                let id = self.sym(sym);
                // price/qty required on trade events (Major #7).
                let price =
                    f64_field(&d, "price").ok_or_else(|| NormError::Parse("kr price".into()))?;
                let qty =
                    f64_field(&d, "qty").ok_or_else(|| NormError::Parse("kr qty".into()))?;
                let side = kr_side(str_field(&d, "side").unwrap_or("buy"));
                let trade_id = u64_field(&d, "seq").unwrap_or(0);
                let seq = self.seq();
                out.push(EventEnvelope::new(
                    Venue::KrakenFutures,
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
            "book_snapshot" => {
                let sym = str_field(&d, "product_id")
                    .ok_or_else(|| NormError::Parse("kr product_id".into()))?;
                let id = self.sym(sym);
                let bids = parse_obj_levels(d.get("bids"), "price", "qty")?;
                let asks = parse_obj_levels(d.get("asks"), "price", "qty")?;
                let seq_id = u64_field(&d, "seq").unwrap_or(0);
                let kind = self.books.entry(id).or_default().on_snapshot(seq_id);
                out.push(EventEnvelope::new(
                    Venue::KrakenFutures,
                    id,
                    exch,
                    recv_ts_ns,
                    seq_id,
                    MarketEvent::BookSnapshot {
                        bids,
                        asks,
                        seq: seq_id,
                        depth: 0,
                        reason: if kind == SnapKind::Init {
                            SnapshotReason::Init
                        } else {
                            SnapshotReason::GapResync
                        },
                    },
                ));
            }
            "book" => {
                let sym = str_field(&d, "product_id")
                    .ok_or_else(|| NormError::Parse("kr product_id".into()))?;
                let id = self.sym(sym);
                let seq_id = u64_field(&d, "seq").unwrap_or(0);
                let px = f64_field(&d, "price").unwrap_or(0.0);
                let qty = f64_field(&d, "qty").unwrap_or(0.0);
                let is_bid = str_field(&d, "side").unwrap_or("buy") != "sell";
                let mut bids: Levels = SmallVec::new();
                let mut asks: Levels = SmallVec::new();
                if is_bid {
                    bids.push((px, qty) as Level);
                } else {
                    asks.push((px, qty) as Level);
                }
                match self.books.entry(id).or_default().on_delta(seq_id, seq_id) {
                    DeltaAction::Apply => out.push(EventEnvelope::new(
                        Venue::KrakenFutures,
                        id,
                        exch,
                        recv_ts_ns,
                        seq_id,
                        MarketEvent::BookDelta {
                            bids,
                            asks,
                            first_seq: seq_id,
                            last_seq: seq_id,
                        },
                    )),
                    DeltaAction::Gap => {
                        let seq = self.seq();
                        out.push(EventEnvelope::new(
                            Venue::KrakenFutures,
                            id,
                            exch,
                            recv_ts_ns,
                            seq,
                            MarketEvent::Status {
                                kind: StatusKind::GapDetected,
                                detail: format!("kraken book gap at seq={seq_id}"),
                            },
                        ));
                    }
                    DeltaAction::Drop => {}
                }
            }
            "ticker" => {
                let sym = str_field(&d, "product_id")
                    .ok_or_else(|| NormError::Parse("kr product_id".into()))?;
                let id = self.sym(sym);
                if let Some(mark) = f64_field(&d, "markPrice") {
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::KrakenFutures,
                        id,
                        exch,
                        recv_ts_ns,
                        seq,
                        MarketEvent::MarkPrice {
                            mark,
                            index: f64::NAN,
                        },
                    ));
                }
                if let Some(rate) = f64_field(&d, "funding_rate") {
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::KrakenFutures,
                        id,
                        exch,
                        recv_ts_ns,
                        seq,
                        MarketEvent::Funding {
                            rate,
                            interval_s: 0,
                            next_funding_ts_ns: 0,
                        },
                    ));
                }
                if let Some(oi) = f64_field(&d, "openInterest") {
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::KrakenFutures,
                        id,
                        exch,
                        recv_ts_ns,
                        seq,
                        MarketEvent::OpenInterest {
                            oi_contracts: oi,
                            oi_notional: f64::NAN,
                        },
                    ));
                }
            }
            _ => {}
        }
        Ok(())
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
