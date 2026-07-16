//! Hyperliquid public-stream normalizer. Channels: `trades`, `l2Book`
//! (snapshot-only — every message is a full book, so we emit a periodic
//! `BookSnapshot`), `activeAssetCtx` (mark/funding/OI). Hyperliquid positions
//! are public on-chain (a unique data edge, tracked in docs/BACKLOG.md).
//!
//! Shapes from documented Hyperliquid; fixtures are synthetic-representative.

use crate::json::*;
use crate::normalize::{NormError, Normalizer};
use mp_core::{
    EventEnvelope, Levels, MarketEvent, Side, SnapshotReason, SymbolId, SymbolTable, Venue,
};
use serde_json::Value;

#[derive(Default)]
pub struct HyperliquidNormalizer {
    symbols: SymbolTable,
    next_seq: u64,
}

impl HyperliquidNormalizer {
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
        self.symbols.intern_default(Venue::Hyperliquid, s)
    }
}

fn hl_levels(v: Option<&Value>) -> Result<Levels, NormError> {
    // Skip invalid levels rather than inventing price 0 (honesty: zeros look real).
    parse_obj_levels(v, "px", "sz")
}

impl Normalizer for HyperliquidNormalizer {
    fn venue(&self) -> Venue {
        Venue::Hyperliquid
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

        match channel {
            "trades" => {
                let Some(arr) = v.get("data").and_then(|d| d.as_array()) else {
                    return Ok(());
                };
                for t in arr {
                    let coin =
                        str_field(t, "coin").ok_or_else(|| NormError::Parse("hl coin".into()))?;
                    let id = self.sym(coin);
                    // price and qty required (Major #7: omit event rather than
                    // invent zero — a zero-price trade is indistinguishable from
                    // real data once stored).
                    let price =
                        f64_field(t, "px").ok_or_else(|| NormError::Parse("hl px".into()))?;
                    let qty =
                        f64_field(t, "sz").ok_or_else(|| NormError::Parse("hl sz".into()))?;
                    // "B" = buy aggressor, "A" = sell aggressor.
                    let side = match str_field(t, "side") {
                        Some("A") => Side::Sell,
                        _ => Side::Buy,
                    };
                    let exch = i64_field(t, "time").map(ms_to_ns).unwrap_or(0);
                    let trade_id = u64_field(t, "tid").unwrap_or(0);
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Hyperliquid,
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
            "l2Book" => {
                let d = v
                    .get("data")
                    .ok_or_else(|| NormError::Parse("hl l2 data".into()))?;
                let coin =
                    str_field(d, "coin").ok_or_else(|| NormError::Parse("hl coin".into()))?;
                let id = self.sym(coin);
                let exch = i64_field(d, "time").map(ms_to_ns).unwrap_or(0);
                let levels = d.get("levels").and_then(|l| l.as_array());
                let bids = hl_levels(levels.and_then(|l| l.first()))?;
                let asks = hl_levels(levels.and_then(|l| l.get(1)))?;
                let seq = self.seq();
                out.push(EventEnvelope::new(
                    Venue::Hyperliquid,
                    id,
                    exch,
                    recv_ts_ns,
                    seq,
                    MarketEvent::BookSnapshot {
                        bids,
                        asks,
                        seq,
                        depth: 0,
                        // Snapshot-only feed ⇒ every book is periodic.
                        reason: SnapshotReason::Periodic,
                    },
                ));
            }
            "activeAssetCtx" => {
                let d = v
                    .get("data")
                    .ok_or_else(|| NormError::Parse("hl ctx data".into()))?;
                let coin =
                    str_field(d, "coin").ok_or_else(|| NormError::Parse("hl coin".into()))?;
                let id = self.sym(coin);
                let ctx = d.get("ctx").unwrap_or(d);
                if let Some(mark) = f64_field(ctx, "markPx") {
                    let index = f64_field(ctx, "oraclePx").unwrap_or(f64::NAN);
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Hyperliquid,
                        id,
                        0,
                        recv_ts_ns,
                        seq,
                        MarketEvent::MarkPrice { mark, index },
                    ));
                }
                if let Some(rate) = f64_field(ctx, "funding") {
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Hyperliquid,
                        id,
                        0,
                        recv_ts_ns,
                        seq,
                        MarketEvent::Funding {
                            rate,
                            interval_s: 3_600, // Hyperliquid funds hourly
                            next_funding_ts_ns: 0,
                        },
                    ));
                }
                if let Some(oi) = f64_field(ctx, "openInterest") {
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Hyperliquid,
                        id,
                        0,
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
        // Snapshot-only book — nothing to reset.
    }
}
