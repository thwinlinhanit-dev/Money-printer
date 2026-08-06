//! Hyperliquid on-chain per-user position census normalizer (spec 028,
//! WHL-1..9). Polls the public REST `POST https://api.hyperliquid.xyz/info`
//! (no auth) for the top-N leaderboard addresses + a configured watchlist,
//! then fetches each address's `clearinghouseState` and emits
//! [`MarketEvent::WhalePosition`] events — real liquidation prices from real
//! positions, the ground truth that calibrates spec 029 estimated liq bands.
//!
//! Positions enter as DATA ONLY (WHL-5): they become strategy-consumable
//! features only after event-study grading (RES-4). Copy-trading is rejected.
//! Addresses are recorded as opaque 0x identifiers with no external labels
//! (WHL-3).
//!
//! ## Payload wrapping
//! The `clearinghouseState` response does not echo the requested address, so
//! the poller wraps it with the address before normalization:
//! `{"address": "0x…", "assetPositions": […], "time": …}`. This keeps the
//! [`Normalizer`] trait frame-based and fully deterministic (the address is
//! request state, not venue data). The leaderboard response (no position
//! data) is parsed by [`leaderboard_addresses`].

use crate::json::{f64_field, i64_field, ms_to_ns, str_field};
use crate::normalize::{NormError, Normalizer};
use mp_core::{EventEnvelope, MarketEvent, SymbolId, SymbolTable, Venue};
use serde_json::Value;

/// Number of top addresses fetched from the leaderboard each top-N poll.
pub const DEFAULT_TOP_N: usize = 50;

#[derive(Default)]
pub struct HyperliquidPositionsNormalizer {
    symbols: SymbolTable,
    next_seq: u64,
}

impl HyperliquidPositionsNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn sym(&mut self, s: &str) -> SymbolId {
        self.symbols.intern_default(Venue::Hyperliquid, s)
    }

    /// Seed the symbol table with a coin before the first event references it
    /// (used by the poller for the watchlist/top-N coin filter).
    pub fn symbols_mut(&mut self) -> &mut SymbolTable {
        &mut self.symbols
    }
}

/// Build the `Status::GapDetected` event a missed poll window must emit
/// (WHL-7). The binary records this whenever a top-N or watchlist poll fails
/// so gaps are surfaced as data, never silently swallowed.
pub fn gap_detected(symbol: SymbolId, recv_ts_ns: i64, detail: String) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        symbol,
        recv_ts_ns,
        recv_ts_ns,
        0,
        MarketEvent::Status {
            kind: mp_core::StatusKind::GapDetected,
            detail,
        },
    )
}

/// Parse a `type=leaderboard` response into the ranked addresses (WHL-1).
/// Returns the addresses in the venue's rank order — the top of the list is
/// the top-N set. Malformed entries are skipped; a response that yields
/// nothing is a parse error (never a silently empty top-N poll).
pub fn leaderboard_addresses(payload: &[u8]) -> Result<Vec<String>, NormError> {
    let v: Value = serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;
    let arr = v
        .as_array()
        .ok_or_else(|| NormError::Parse("leaderboard response is not an array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        // The address is an opaque on-chain identifier; name/pnl are never
        // imported as labels (WHL-3).
        if let Some(addr) = str_field(entry, "address") {
            out.push(addr.to_owned());
        }
    }
    if out.is_empty() {
        return Err(NormError::Parse(
            "leaderboard response yielded no addresses".into(),
        ));
    }
    Ok(out)
}

impl Normalizer for HyperliquidPositionsNormalizer {
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
        let address = str_field(&v, "address")
            .ok_or_else(|| NormError::Parse("wrapped position response lacks address".into()))?
            .to_owned();
        if !address.starts_with("0x") {
            return Err(NormError::Parse("address is not an opaque 0x id".into()));
        }
        let positions = v
            .get("assetPositions")
            .and_then(|p| p.as_array())
            .ok_or_else(|| NormError::Parse("wrapped response lacks assetPositions".into()))?;

        // Exchange-reported time of the account state, if present.
        let exch = i64_field(&v, "time").map(ms_to_ns).unwrap_or(0);

        // Deterministic order: the venue's response order is authoritative
        // (positions are per (address, coin) — no map iteration here).
        for entry in positions {
            let Some(pos) = entry.get("position") else {
                continue;
            };
            let Some(coin) = str_field(pos, "coin") else {
                continue;
            };
            // Required fields; a malformed position is skipped, never
            // invented (zero-size/zero-price positions would look real).
            let Some(size) = f64_field(pos, "szi") else {
                continue;
            };
            let Some(entry_px) = f64_field(pos, "entryPx") else {
                continue;
            };
            let Some(leverage) = pos.get("leverage").and_then(|l| f64_field(l, "value")) else {
                continue;
            };
            // liquidationPx may be null (e.g. small positions) → NaN sentinel.
            let liq_price = f64_field(pos, "liquidationPx").unwrap_or(f64::NAN);
            // CONV-8: never propagate NaN/inf as if real data.
            if !size.is_finite() || !entry_px.is_finite() || !leverage.is_finite() {
                continue;
            }

            let id = self.sym(coin);
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::Hyperliquid,
                id,
                exch,
                recv_ts_ns,
                seq,
                MarketEvent::WhalePosition {
                    address: address.clone(),
                    size,
                    entry: entry_px,
                    leverage,
                    liq_price,
                },
            ));
        }
        Ok(())
    }

    fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    fn reset_books(&mut self) {
        // Positions are a stateless census — nothing to reset.
    }
}

#[cfg(feature = "live-http")]
pub mod rest {
    //! Blocking REST pollers against the public Hyperliquid info endpoint
    //! (WHL-1: no auth, no key — PD-2). Behind `live-http`.

    use super::*;

    /// Public leaderboard request body (WHL-1: no auth, no key — PD-2).
    pub fn leaderboard_body(time_window: &str) -> serde_json::Value {
        serde_json::json!({ "type": "leaderboard", "timeWindow": time_window })
    }

    /// Public per-user `clearinghouseState` request body (no auth).
    pub fn clearinghouse_body(address: &str) -> serde_json::Value {
        serde_json::json!({ "type": "clearinghouseState", "user": address })
    }

    /// `POST https://api.hyperliquid.xyz/info` with a JSON body.
    pub fn info_blocking(body: &serde_json::Value) -> Result<serde_json::Value, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .post("https://api.hyperliquid.xyz/info")
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .map_err(|e| format!("hyperliquid info request: {e}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| format!("hyperliquid info body: {e}"))?;
        if !status.is_success() {
            return Err(format!("hyperliquid info HTTP {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("hyperliquid info json: {e}"))
    }

    /// Fetch the top-N leaderboard addresses for `time_window` (e.g. "7d").
    pub fn fetch_leaderboard_blocking(time_window: &str) -> Result<Vec<String>, String> {
        let body = leaderboard_body(time_window);
        let v = info_blocking(&body)?;
        let raw = serde_json::to_vec(&v).map_err(|e| format!("reserialize: {e}"))?;
        leaderboard_addresses(&raw).map_err(|e| format!("leaderboard parse: {e}"))
    }

    /// Fetch one address's `clearinghouseState` and wrap it with the address
    /// for the normalizer (see module docs).
    pub fn fetch_clearinghouse_state_blocking(address: &str) -> Result<serde_json::Value, String> {
        let body = clearinghouse_body(address);
        let v = info_blocking(&body)?;
        let mut wrapped = serde_json::json!({ "address": address });
        if let Some(obj) = wrapped.as_object_mut() {
            if let Some(resp) = v.as_object() {
                for (k, val) in resp {
                    obj.insert(k.clone(), val.clone());
                }
            }
        }
        Ok(wrapped)
    }
}
