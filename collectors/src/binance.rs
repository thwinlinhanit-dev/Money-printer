//! Binance USDⓈ-M Futures public-stream normalizer (COL-5..8). Streams:
//! `aggTrade`, `depthUpdate` (U/u/pu continuity), `markPriceUpdate` (carries
//! mark + index + funding together), `forceOrder` (liquidations — throttled by
//! the venue to ~1/s, so it is a SAMPLE, COL-8). Book needs a REST snapshot to
//! seed; here we sync from the first contiguous run and gap-detect via `pu`.
//!
//! Shapes from documented Binance Futures; fixtures are synthetic-representative.
//! Geo note: Binance blocks US IPs — run the collector from an allowed region.

use crate::book_sync::{BookSync, DeltaAction, SnapKind};
use crate::json::*;
use crate::normalize::{NormError, Normalizer};
use mp_core::{
    EventEnvelope, MarketEvent, Side, SnapshotReason, StatusKind, SymbolId, SymbolTable, Venue,
};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct BinanceNormalizer {
    symbols: SymbolTable,
    books: BTreeMap<SymbolId, BookSync>,
    seeded: BTreeMap<SymbolId, bool>,
    next_seq: u64,
}

impl BinanceNormalizer {
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
        self.symbols.intern_default(Venue::BinanceFutures, s)
    }
}

impl Normalizer for BinanceNormalizer {
    fn venue(&self) -> Venue {
        Venue::BinanceFutures
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let raw: Value =
            serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;
        // Combined-stream wrapper: {"stream":..,"data":{..}}.
        let d = raw.get("data").unwrap_or(&raw);
        let Some(etype) = str_field(d, "e") else {
            return Ok(());
        };
        let exch = i64_field(d, "E").map(ms_to_ns).unwrap_or(0);

        match etype {
            "aggTrade" => {
                let sym = str_field(d, "s").ok_or_else(|| NormError::Parse("bn s".into()))?;
                let id = self.sym(sym);
                let price = f64_field(d, "p").ok_or_else(|| NormError::Parse("bn p".into()))?;
                let qty = f64_field(d, "q").ok_or_else(|| NormError::Parse("bn q".into()))?;
                // m = "is buyer the market maker": true ⇒ aggressor is the seller.
                let maker = d.get("m").and_then(|x| x.as_bool()).unwrap_or(false);
                let side = if maker { Side::Sell } else { Side::Buy };
                let trade_id = u64_field(d, "a").unwrap_or(0);
                let t = i64_field(d, "T").map(ms_to_ns).unwrap_or(exch);
                let seq = self.seq();
                out.push(EventEnvelope::new(
                    Venue::BinanceFutures,
                    id,
                    t,
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
            "depthUpdate" => {
                let sym = str_field(d, "s").ok_or_else(|| NormError::Parse("bn s".into()))?;
                let id = self.sym(sym);
                let first = u64_field(d, "U").unwrap_or(0);
                let last = u64_field(d, "u").unwrap_or(0);
                let bids = parse_pair_levels(d.get("b"))?;
                let asks = parse_pair_levels(d.get("a"))?;
                let seeded = *self.seeded.get(&id).unwrap_or(&false);
                let st = self.books.entry(id).or_default();
                if !seeded {
                    // First message seeds the book as a synthetic snapshot (real
                    // impl seeds from a REST depth snapshot first — COL-7).
                    let kind = st.on_snapshot(last);
                    self.seeded.insert(id, true);
                    out.push(EventEnvelope::new(
                        Venue::BinanceFutures,
                        id,
                        exch,
                        recv_ts_ns,
                        last,
                        MarketEvent::BookSnapshot {
                            bids,
                            asks,
                            seq: last,
                            depth: 0,
                            reason: if kind == SnapKind::Init {
                                SnapshotReason::Init
                            } else {
                                SnapshotReason::GapResync
                            },
                        },
                    ));
                    return Ok(());
                }
                match st.on_delta(first, last) {
                    DeltaAction::Apply => out.push(EventEnvelope::new(
                        Venue::BinanceFutures,
                        id,
                        exch,
                        recv_ts_ns,
                        last,
                        MarketEvent::BookDelta {
                            bids,
                            asks,
                            first_seq: first,
                            last_seq: last,
                        },
                    )),
                    DeltaAction::Gap => {
                        self.seeded.insert(id, false); // reseed on next message
                        let seq = self.seq();
                        out.push(EventEnvelope::new(
                            Venue::BinanceFutures,
                            id,
                            exch,
                            recv_ts_ns,
                            seq,
                            MarketEvent::Status {
                                kind: StatusKind::GapDetected,
                                detail: format!("binance depth gap at U={first}"),
                            },
                        ));
                    }
                    DeltaAction::Drop => {}
                }
            }
            "markPriceUpdate" => {
                let sym = str_field(d, "s").ok_or_else(|| NormError::Parse("bn s".into()))?;
                let id = self.sym(sym);
                let mark = f64_field(d, "p")
                    .ok_or_else(|| NormError::Parse("bn markPriceUpdate p".into()))?;
                let index = f64_field(d, "i").unwrap_or(f64::NAN);
                let seq = self.seq();
                out.push(EventEnvelope::new(
                    Venue::BinanceFutures,
                    id,
                    exch,
                    recv_ts_ns,
                    seq,
                    MarketEvent::MarkPrice { mark, index },
                ));
                if let Some(rate) = f64_field(d, "r") {
                    let next = i64_field(d, "T").map(ms_to_ns).unwrap_or(0);
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::BinanceFutures,
                        id,
                        exch,
                        recv_ts_ns,
                        seq,
                        MarketEvent::Funding {
                            rate,
                            interval_s: 28_800, // Binance funds every 8h
                            next_funding_ts_ns: next,
                        },
                    ));
                }
            }
            "forceOrder" => {
                let o = d
                    .get("o")
                    .ok_or_else(|| NormError::Parse("bn forceOrder o".into()))?;
                let sym = str_field(o, "s").ok_or_else(|| NormError::Parse("bn fo s".into()))?;
                let id = self.sym(sym);
                let price = f64_field(o, "p")
                    .ok_or_else(|| NormError::Parse("bn forceOrder price".into()))?;
                let qty = f64_field(o, "q")
                    .ok_or_else(|| NormError::Parse("bn forceOrder qty".into()))?;
                let side = match str_field(o, "S") {
                    Some("BUY") => Side::Buy,
                    _ => Side::Sell,
                };
                let seq = self.seq();
                out.push(EventEnvelope::new(
                    Venue::BinanceFutures,
                    id,
                    exch,
                    recv_ts_ns,
                    seq,
                    MarketEvent::Liquidation { price, qty, side },
                ));
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
        for s in self.seeded.values_mut() {
            *s = false;
        }
    }
}
