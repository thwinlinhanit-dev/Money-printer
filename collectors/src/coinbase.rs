//! Coinbase Advanced Trade public-stream normalizer (spot only). Channels:
//! `market_trades`, `l2_data`. The new API omits per-symbol book sequence
//! numbers, so gap detection is limited (documented; spec 002 Decisions) — we
//! emit snapshots and deltas and rely on periodic snapshots. ISO-8601 exchange
//! timestamps are not parsed in v1 (exch_ts = 0); `recv_ts_ns` is authoritative.
//!
//! Shapes from documented Coinbase Advanced Trade; fixtures are synthetic.

use crate::json::*;
use crate::normalize::{NormError, Normalizer};
use mp_core::{
    EventEnvelope, Level, Levels, MarketEvent, Side, SnapshotReason, SymbolId, SymbolTable, Venue,
};
use serde_json::Value;
use smallvec::SmallVec;

#[derive(Default)]
pub struct CoinbaseNormalizer {
    symbols: SymbolTable,
    next_seq: u64,
}

impl CoinbaseNormalizer {
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
        self.symbols.intern_default(Venue::Coinbase, s)
    }
}

impl Normalizer for CoinbaseNormalizer {
    fn venue(&self) -> Venue {
        Venue::Coinbase
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let v: Value =
            serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;
        let channel = str_field(&v, "channel").unwrap_or("");
        let Some(events) = v.get("events").and_then(|e| e.as_array()) else {
            return Ok(());
        };

        match channel {
            "market_trades" => {
                for ev in events {
                    let Some(trades) = ev.get("trades").and_then(|t| t.as_array()) else {
                        continue;
                    };
                    for t in trades {
                        let sym = str_field(t, "product_id")
                            .ok_or_else(|| NormError::Parse("cb product_id".into()))?;
                        let id = self.sym(sym);
                        // price/qty are required on trade events (Major #7: zero-price
                        // trades are real-looking noise — drop the event, not invent 0).
                        let price = f64_field(t, "price")
                            .ok_or_else(|| NormError::Parse("cb trade price".into()))?;
                        let qty = f64_field(t, "size")
                            .ok_or_else(|| NormError::Parse("cb trade size".into()))?;
                        let side = match str_field(t, "side") {
                            Some("SELL") => Side::Sell,
                            _ => Side::Buy,
                        };
                        let trade_id = str_field(t, "trade_id").map(hash_str).unwrap_or(0);
                        let seq = self.seq();
                        out.push(EventEnvelope::new(
                            Venue::Coinbase,
                            id,
                            0,
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
            }
            "l2_data" => {
                for ev in events {
                    let sym = str_field(ev, "product_id")
                        .ok_or_else(|| NormError::Parse("cb l2 product_id".into()))?;
                    let id = self.sym(sym);
                    let kind = str_field(ev, "type").unwrap_or("update");
                    let mut bids: Levels = SmallVec::new();
                    let mut asks: Levels = SmallVec::new();
                    if let Some(updates) = ev.get("updates").and_then(|u| u.as_array()) {
                        for u in updates {
                            let px = f64_field(u, "price_level").unwrap_or(0.0);
                            let qty = f64_field(u, "new_quantity").unwrap_or(0.0);
                            match str_field(u, "side") {
                                Some("bid") => bids.push((px, qty) as Level),
                                Some("offer") | Some("ask") => asks.push((px, qty) as Level),
                                _ => {}
                            }
                        }
                    }
                    let seq = self.seq();
                    if kind == "snapshot" {
                        out.push(EventEnvelope::new(
                            Venue::Coinbase,
                            id,
                            0,
                            recv_ts_ns,
                            seq,
                            MarketEvent::BookSnapshot {
                                bids,
                                asks,
                                seq,
                                depth: 0,
                                reason: SnapshotReason::Init,
                            },
                        ));
                    } else {
                        out.push(EventEnvelope::new(
                            Venue::Coinbase,
                            id,
                            0,
                            recv_ts_ns,
                            seq,
                            MarketEvent::BookDelta {
                                bids,
                                asks,
                                first_seq: seq,
                                last_seq: seq,
                            },
                        ));
                    }
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
        // No per-symbol book sequence state to reset (snapshot-driven).
    }
}
