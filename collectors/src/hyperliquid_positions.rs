//! Hyperliquid on-chain per-user position census normalizer (spec 028,
//! WHL-1..9). Polls the public REST API (no auth) for the top-N leaderboard
//! addresses + a configured watchlist, then fetches each address's
//! `clearinghouseState` (`POST https://api.hyperliquid.xyz/info`) and emits
//! [`MarketEvent::WhalePosition`] events — real liquidation prices from real
//! positions, the ground truth that calibrates spec 029 estimated liq bands.
//!
//! Leaderboard source (2026-08-08): the original `POST /info
//! {"type":"leaderboard"}` was removed upstream (HTTP 422); the protocol now
//! serves it as a public GET from the stats-data bucket
//! (`https://stats-data.hyperliquid.xyz/Mainnet/leaderboard`, refreshed
//! ~hourly) — see [`rest::fetch_leaderboard_blocking`].
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

/// Build the `Status::Census` event a COMPLETED poll emits (WHL-7): "at this
/// receive time the census ran and found `positions` open positions across
/// the polled addresses." Zero-position polls are real observations — the
/// raw log must keep recording them so (a) the audit sees a live census
/// rather than a silent gap, and (b) the watchdog's log-stall check never
/// mistakes a quiet (flat) market for a dead collector.
pub fn census_detected(symbol: SymbolId, recv_ts_ns: i64, positions: usize) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        symbol,
        recv_ts_ns,
        recv_ts_ns,
        0,
        MarketEvent::Status {
            kind: mp_core::StatusKind::Census,
            detail: format!("census ran, {positions} position(s)"),
        },
    )
}

/// Parse the current stats-data leaderboard response (the replacement for
/// the removed `POST /info type=leaderboard`, WHL-1) into addresses ranked by
/// the requested window's PnL — the top of the list is the top-N set.
/// `time_window` maps to the response's window keys: "1d"→"day", "7d"→"week",
/// "30d"→"month", anything else → "allTime". Malformed entries are skipped;
/// a response that yields nothing is a parse error (never a silently empty
/// top-N poll). Addresses are opaque 0x ids; names/pnl are never imported as
/// labels (WHL-3).
pub fn leaderboard_addresses(payload: &[u8], time_window: &str) -> Result<Vec<String>, NormError> {
    let v: Value = serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;
    let rows = v
        .get("leaderboardRows")
        .and_then(|r| r.as_array())
        .ok_or_else(|| NormError::Parse("leaderboard response lacks leaderboardRows".into()))?;
    let window_key = match time_window {
        "1d" => "day",
        "7d" => "week",
        "30d" => "month",
        _ => "allTime",
    };
    let mut ranked: Vec<(f64, String)> = Vec::with_capacity(rows.len());
    for entry in rows {
        let Some(addr) = str_field(entry, "ethAddress") else {
            continue;
        };
        if addr.is_empty() || !addr.starts_with("0x") {
            continue;
        }
        // Window PnL as the ranking key; a record missing its window still
        // ranks (0.0) — the census is never silently shrunk by a partial row.
        let pnl = entry
            .get("windowPerformances")
            .and_then(|w| w.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|kv| kv.get(0).and_then(Value::as_str) == Some(window_key))
            })
            .and_then(|kv| kv.get(1))
            .and_then(|perf| f64_field(perf, "pnl"))
            .unwrap_or(0.0);
        ranked.push((pnl, addr.to_owned()));
    }
    // Highest PnL first — the venue's rank order (WHL-1).
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    if ranked.is_empty() {
        return Err(NormError::Parse(
            "leaderboard response yielded no addresses".into(),
        ));
    }
    Ok(ranked.into_iter().map(|(_, a)| a).collect())
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

    /// Public leaderboard endpoint (WHL-1: no auth, no key — PD-2). The
    /// original `POST /info {"type":"leaderboard"}` was removed upstream
    /// (HTTP 422 since 2026-08); the leaderboard now lives in the stats-data
    /// bucket, refreshed ~hourly. `MP_HYPERLIQUID_LEADERBOARD_URL` overrides
    /// (testnet or a pinned snapshot for tests).
    pub const LEADERBOARD_URL: &str = "https://stats-data.hyperliquid.xyz/Mainnet/leaderboard";

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

    /// Fetch the top-N leaderboard addresses for `time_window` (e.g. "7d"),
    /// ranked by that window's PnL. GET from the stats-data bucket (the
    /// full leaderboard is ~34 MB, refreshed upstream ~hourly — the whale
    /// caches the list and re-polls the addresses' positions on its own
    /// faster cadence). Fail-closed: any non-2xx or parse failure is an
    /// error (WHL-7 records a GapDetected, never a silent skip).
    pub fn fetch_leaderboard_blocking(time_window: &str) -> Result<Vec<String>, String> {
        let url = std::env::var("MP_HYPERLIQUID_LEADERBOARD_URL")
            .unwrap_or_else(|_| LEADERBOARD_URL.to_string());
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120)) // ~34 MB payload
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("hyperliquid leaderboard request: {e}"))?;
        let status = resp.status();
        let body = resp
            .bytes()
            .map_err(|e| format!("hyperliquid leaderboard body: {e}"))?;
        if !status.is_success() {
            return Err(format!("hyperliquid leaderboard HTTP {status}"));
        }
        leaderboard_addresses(&body, time_window).map_err(|e| format!("leaderboard parse: {e}"))
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
