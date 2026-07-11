//! OKX v5 public-stream normalizer (COL-5..8). Channels: `trades`, `books`
//! (snapshot/update with `seqId`/`prevSeqId` continuity), `funding-rate`,
//! `open-interest`, `mark-price`, `liquidation-orders`. Book checksum
//! verification is deferred (continuity via seqId; see spec 002 Decisions).
//!
//! Shapes from documented OKX v5; fixtures are synthetic-representative.

use crate::json::*;
use crate::normalize::{NormError, Normalizer};
use mp_core::{
    EventEnvelope, MarketEvent, Side, SnapshotReason, StatusKind, SymbolId, SymbolTable, Venue,
};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default, Clone, Copy)]
struct OkxBook {
    last_seq: i64,
    init: bool,
    desynced: bool,
}

#[derive(Default)]
pub struct OkxNormalizer {
    symbols: SymbolTable,
    books: BTreeMap<SymbolId, OkxBook>,
    next_seq: u64,
}

impl OkxNormalizer {
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
        self.symbols.intern_default(Venue::Okx, s)
    }
}

fn okx_side(s: &str) -> Option<Side> {
    match s {
        "buy" => Some(Side::Buy),
        "sell" => Some(Side::Sell),
        _ => None,
    }
}

impl Normalizer for OkxNormalizer {
    fn venue(&self) -> Venue {
        Venue::Okx
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let v: Value =
            serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;
        let Some(channel) = v.pointer("/arg/channel").and_then(|c| c.as_str()) else {
            return Ok(()); // event/ack messages
        };
        let action = str_field(&v, "action").unwrap_or("update");
        let Some(data) = v.get("data").and_then(|d| d.as_array()) else {
            return Ok(());
        };

        match channel {
            "trades" => {
                for t in data {
                    let sym = str_field(t, "instId")
                        .ok_or_else(|| NormError::Parse("okx trade instId".into()))?;
                    let id = self.sym(sym);
                    let price =
                        f64_field(t, "px").ok_or_else(|| NormError::Parse("okx px".into()))?;
                    let qty =
                        f64_field(t, "sz").ok_or_else(|| NormError::Parse("okx sz".into()))?;
                    let side = str_field(t, "side")
                        .and_then(okx_side)
                        .ok_or_else(|| NormError::Parse("okx side".into()))?;
                    let exch = i64_field(t, "ts").map(ms_to_ns).unwrap_or(0);
                    let trade_id = str_field(t, "tradeId").map(hash_str).unwrap_or(0);
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Okx,
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
            }
            "books" | "books5" | "books-l2-tbt" => {
                for d in data {
                    let sym = str_field(d, "instId")
                        .or_else(|| v.pointer("/arg/instId").and_then(|x| x.as_str()))
                        .ok_or_else(|| NormError::Parse("okx book instId".into()))?;
                    let id = self.sym(sym);
                    let exch = i64_field(d, "ts").map(ms_to_ns).unwrap_or(0);
                    let bids = parse_pair_levels(d.get("bids"))?;
                    let asks = parse_pair_levels(d.get("asks"))?;
                    let seq_id = i64_field(d, "seqId").unwrap_or(0);
                    let prev = i64_field(d, "prevSeqId").unwrap_or(-1);
                    let is_snapshot = action == "snapshot" || channel == "books5";

                    // Decide the action against book state, then drop the borrow
                    // before calling self.seq() for any status event.
                    enum Act {
                        Snapshot(SnapshotReason),
                        Delta,
                        Gap(i64),
                        Wait,
                    }
                    let act = {
                        let st = self.books.entry(id).or_default();
                        if is_snapshot {
                            let was = st.init || st.desynced;
                            *st = OkxBook {
                                last_seq: seq_id,
                                init: true,
                                desynced: false,
                            };
                            Act::Snapshot(if was {
                                SnapshotReason::GapResync
                            } else {
                                SnapshotReason::Init
                            })
                        } else if !st.init || st.desynced {
                            Act::Wait
                        } else if prev != st.last_seq {
                            let last = st.last_seq;
                            st.desynced = true;
                            Act::Gap(last)
                        } else {
                            st.last_seq = seq_id;
                            Act::Delta
                        }
                    };
                    let seq_u = seq_id.max(0) as u64;
                    match act {
                        Act::Snapshot(reason) => out.push(EventEnvelope::new(
                            Venue::Okx,
                            id,
                            exch,
                            recv_ts_ns,
                            seq_u,
                            MarketEvent::BookSnapshot {
                                bids,
                                asks,
                                seq: seq_u,
                                depth: 0,
                                reason,
                            },
                        )),
                        Act::Delta => out.push(EventEnvelope::new(
                            Venue::Okx,
                            id,
                            exch,
                            recv_ts_ns,
                            seq_u,
                            MarketEvent::BookDelta {
                                bids,
                                asks,
                                first_seq: seq_u,
                                last_seq: seq_u,
                            },
                        )),
                        Act::Gap(last) => {
                            let seq = self.seq();
                            out.push(EventEnvelope::new(
                                Venue::Okx,
                                id,
                                exch,
                                recv_ts_ns,
                                seq,
                                MarketEvent::Status {
                                    kind: StatusKind::GapDetected,
                                    detail: format!("okx book gap: prev={prev} last={last}"),
                                },
                            ));
                        }
                        Act::Wait => {}
                    }
                }
            }
            "funding-rate" => {
                for d in data {
                    let sym = str_field(d, "instId")
                        .ok_or_else(|| NormError::Parse("okx funding instId".into()))?;
                    let id = self.sym(sym);
                    let rate = f64_field(d, "fundingRate").unwrap_or(0.0);
                    let next = i64_field(d, "nextFundingTime").map(ms_to_ns).unwrap_or(0);
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Okx,
                        id,
                        0,
                        recv_ts_ns,
                        seq,
                        MarketEvent::Funding {
                            rate,
                            interval_s: 0,
                            next_funding_ts_ns: next,
                        },
                    ));
                }
            }
            "open-interest" => {
                for d in data {
                    let sym = str_field(d, "instId")
                        .ok_or_else(|| NormError::Parse("okx oi instId".into()))?;
                    let id = self.sym(sym);
                    let oi = f64_field(d, "oi").unwrap_or(0.0);
                    let oi_ccy = f64_field(d, "oiCcy").unwrap_or(f64::NAN);
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Okx,
                        id,
                        0,
                        recv_ts_ns,
                        seq,
                        MarketEvent::OpenInterest {
                            oi_contracts: oi,
                            oi_notional: oi_ccy,
                        },
                    ));
                }
            }
            "mark-price" => {
                for d in data {
                    let sym = str_field(d, "instId")
                        .ok_or_else(|| NormError::Parse("okx mark instId".into()))?;
                    let id = self.sym(sym);
                    let mark = f64_field(d, "markPx").unwrap_or(0.0);
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Okx,
                        id,
                        0,
                        recv_ts_ns,
                        seq,
                        MarketEvent::MarkPrice {
                            mark,
                            index: f64::NAN,
                        },
                    ));
                }
            }
            "liquidation-orders" => {
                for d in data {
                    let sym = str_field(d, "instId").unwrap_or("");
                    let id = self.sym(sym);
                    if let Some(details) = d.get("details").and_then(|x| x.as_array()) {
                        for det in details {
                            let price = f64_field(det, "bkPx").unwrap_or(0.0);
                            let qty = f64_field(det, "sz").unwrap_or(0.0);
                            let side = str_field(det, "side")
                                .and_then(okx_side)
                                .unwrap_or(Side::Sell);
                            let seq = self.seq();
                            out.push(EventEnvelope::new(
                                Venue::Okx,
                                id,
                                0,
                                recv_ts_ns,
                                seq,
                                MarketEvent::Liquidation { price, qty, side },
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn reset_books(&mut self) {
        for st in self.books.values_mut() {
            st.desynced = true;
        }
    }
}
