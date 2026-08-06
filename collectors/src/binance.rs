//! Binance USDⓈ-M Futures public-stream normalizer (COL-5..8). Streams:
//! `aggTrade`, `depthUpdate` (U/u/pu continuity), `markPriceUpdate` (carries
//! mark + index + funding together), `forceOrder` (liquidations — throttled by
//! the venue to ~1/s, so it is a SAMPLE, COL-8). Book needs a REST snapshot to
//! seed; here we sync from the first contiguous run and gap-detect via `pu`.
//!
//! Shapes from documented Binance Futures; fixtures are synthetic-representative.
//! Geo note: Binance blocks US IPs — run the collector from an allowed region.

use crate::book_sync::{BinanceBookSync, DeltaAction, SnapKind};
use crate::json::*;
use crate::normalize::{NormError, Normalizer};
use mp_core::{EventEnvelope, MarketEvent, Side, StatusKind, SymbolId, SymbolTable, Venue};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct BinanceNormalizer {
    symbols: SymbolTable,
    /// Per-symbol depth continuity state — Binance's documented U/u/pu
    /// algorithm (spec 020), not the generic BookSync next+1 rule.
    books: BTreeMap<SymbolId, BinanceBookSync>,
    /// `true` only after the book was seeded from a *REST* snapshot
    /// (`inject_rest_depth_seed`); synthetic-seed from the first delta was
    /// removed — depth deltas before the REST seed are dropped, never
    /// extrapolated into a fake snapshot. Trades/mark/funding still flow.
    seeded: BTreeMap<SymbolId, bool>,
    next_seq: u64,
    /// COL-27: when true, WS `aggTrade` frames are dropped here (trades come
    /// exclusively from the REST poller). Frame counts in the daily log then
    /// reflect only depth/book streams, and a stale WS trade stream can never
    /// churn the collector — the REST poller is the single source of truth.
    suppress_ws_trades: bool,
    /// COL-28: when true, WS `markPriceUpdate` frames are dropped here (mark
    /// price + funding come exclusively from the REST premiumIndex poller).
    /// Same rationale as `suppress_ws_trades`: fstream silently drops the
    /// markPrice stream from this egress (spec 024 incident 2026-08-04), and
    /// when a proxy later restores it, suppression keeps the recording
    /// single-source instead of double-recording mark/funding events.
    suppress_ws_mark_price: bool,
}

impl BinanceNormalizer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }
    /// Mutable access to the symbol table — used by `inject_rest_depth_seed`
    /// to intern the symbol before the WS stream starts (COL-23).
    pub fn symbols_mut(&mut self) -> &mut SymbolTable {
        &mut self.symbols
    }
    /// Directly seed the book state for `id` with `last_update_id` from a
    /// REST snapshot, marking the symbol as seeded so `depthUpdate` continuity
    /// is validated against it (COL-23). Returns the `SnapKind` so the caller
    /// can set the correct `SnapshotReason` on the injected event.
    pub fn seed_book(&mut self, id: SymbolId, last_update_id: u64) -> crate::book_sync::SnapKind {
        let was_seeded = *self.seeded.get(&id).unwrap_or(&false);
        // A desynced book being re-seeded is a Resync even if `seeded` was
        // cleared by reset_books() — the book existed before the gap.
        let had_book = self.books.get(&id).is_some_and(|st| st.needs_reseed());
        self.seeded.insert(id, true);
        self.books.entry(id).or_default().seed(last_update_id);
        if was_seeded || had_book {
            SnapKind::Resync
        } else {
            SnapKind::Init
        }
    }
    /// True if any symbol's book detected a `pu` continuity gap and needs a
    /// fresh REST snapshot (COL-24). The owning binary polls this once per
    /// loop iteration and calls [`reseed_if_needed`] when set — no sleeping
    /// inline, no HTTP inside `normalize()`.
    pub fn needs_reseed(&self) -> bool {
        self.books.values().any(BinanceBookSync::needs_reseed)
    }
    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }
    fn sym(&mut self, s: &str) -> SymbolId {
        self.symbols.intern_default(Venue::BinanceFutures, s)
    }
    /// COL-27: switch trade ingestion between the WS stream and the REST
    /// poller. Off by default (WS is the normal path).
    pub fn set_suppress_ws_trades(&mut self, on: bool) {
        self.suppress_ws_trades = on;
    }
    /// COL-28: switch mark/funding ingestion between the WS `markPriceUpdate`
    /// stream and the REST premiumIndex poller. Off by default (WS is the
    /// normal path).
    pub fn set_suppress_ws_mark_price(&mut self, on: bool) {
        self.suppress_ws_mark_price = on;
    }
}

/// `SystemTime::now` as ns — the one sanctioned wall-clock read in this
/// crate: stamping `recv_ts_ns` on REST-injected events at the binary edge
/// (PD-3: never on decision paths). Used by [`reseed_if_needed`]; binaries
/// may call it for their own edge timestamps instead of open-coding it.
#[cfg(feature = "live-http")]
pub fn wall_now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

/// REST depth snapshot for Binance (spec 020). Fetched once on WebSocket
/// connect to seed book state, avoiding synthetic-seeding from the first delta.
#[cfg(feature = "live-http")]
pub async fn fetch_depth_snapshot(
    symbol: &str,
    limit: u16,
    is_futures: bool,
) -> Result<MarketEvent, Box<dyn std::error::Error + Send + Sync>> {
    fetch_depth_snapshot_budgeted(symbol, limit, is_futures, None).await
}

/// Async depth fetch with an optional injected [`RateBudget`] (COL-21).
/// A 429 surfaces as `UnexpectedStatus`-bearing error text so callers can
/// treat it as RetryAfter; the budget keeps the allowance deterministic in
/// tests (caller injects `now_ns`).
#[cfg(feature = "live-http")]
pub async fn fetch_depth_snapshot_budgeted(
    symbol: &str,
    limit: u16,
    is_futures: bool,
    mut budget: Option<&mut crate::rate::RateBudget>,
) -> Result<MarketEvent, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(b) = &mut budget {
        let now_ns = b.now();
        if !b.try_take(now_ns, depth_weight(limit, is_futures)) {
            return Err(
                "rate budget exhausted for Binance depth snapshot; try again after refill".into(),
            );
        }
    }
    let path = if is_futures {
        "/fapi/v1/depth"
    } else {
        "/api/v3/depth"
    };
    let base = if is_futures {
        "https://fapi.binance.com"
    } else {
        "https://api.binance.com"
    };
    let url = format!("{base}{path}?symbol={symbol}&limit={limit}");
    let resp = reqwest::get(&url).await?;
    let status = resp.status();
    if status.as_u16() == 429 {
        // Retry-After, not a generic error (COL-21): the caller backs off.
        let wait = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60);
        tracing::warn!(
            symbol,
            retry_after_s = wait,
            "binance depth 429 — RetryAfter"
        );
        return Err(format!("Binance depth rate-limited (429); Retry-After {wait}s").into());
    }
    let raw: serde_json::Value = resp.error_for_status()?.json().await?;
    depth_snapshot_from_json(&raw, limit)
}

/// Documented request weights (spec 020 §Rate limits) so the budget spends
/// the same units Binance counts, not raw request counts.
#[cfg(feature = "live-http")]
pub fn depth_weight(limit: u16, is_futures: bool) -> f64 {
    if is_futures {
        if limit <= 50 {
            2.0
        } else if limit <= 100 {
            5.0
        } else {
            50.0
        }
    } else if limit <= 100 {
        5.0
    } else if limit <= 500 {
        25.0
    } else {
        50.0
    }
}

#[cfg(feature = "live-http")]
pub fn fetch_depth_snapshot_blocking(
    symbol: &str,
    limit: u16,
    is_futures: bool,
) -> Result<MarketEvent, Box<dyn std::error::Error + Send + Sync>> {
    fetch_depth_snapshot_blocking_budgeted(symbol, limit, is_futures, None)
}

/// Blocking depth fetch with an optional injected [`RateBudget`] (COL-21);
/// deterministic in tests because the budget consumes injected `now_ns`.
#[cfg(feature = "live-http")]
pub fn fetch_depth_snapshot_blocking_budgeted(
    symbol: &str,
    limit: u16,
    is_futures: bool,
    mut budget: Option<&mut crate::rate::RateBudget>,
) -> Result<MarketEvent, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(b) = &mut budget {
        let now_ns = b.now();
        if !b.try_take(now_ns, depth_weight(limit, is_futures)) {
            return Err(
                "rate budget exhausted for Binance depth snapshot; try again after refill".into(),
            );
        }
    }
    let path = if is_futures {
        "/fapi/v1/depth"
    } else {
        "/api/v3/depth"
    };
    let base = if is_futures {
        "https://fapi.binance.com"
    } else {
        "https://api.binance.com"
    };
    let url = format!("{base}{path}?symbol={symbol}&limit={limit}");
    let resp = reqwest::blocking::get(&url)?;
    let status = resp.status();
    if status.as_u16() == 429 {
        let wait = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60);
        tracing::warn!(
            symbol,
            retry_after_s = wait,
            "binance depth 429 — RetryAfter"
        );
        return Err(format!("Binance depth rate-limited (429); Retry-After {wait}s").into());
    }
    let raw: serde_json::Value = resp.error_for_status()?.json()?;
    depth_snapshot_from_json(&raw, limit)
}

#[cfg(feature = "live-http")]
fn depth_snapshot_from_json(
    raw: &serde_json::Value,
    limit: u16,
) -> Result<MarketEvent, Box<dyn std::error::Error + Send + Sync>> {
    let bids = parse_pair_levels(raw.get("bids"))?;
    let asks = parse_pair_levels(raw.get("asks"))?;
    let last_update_id = raw
        .get("lastUpdateId")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Ok(MarketEvent::BookSnapshot {
        bids,
        asks,
        seq: last_update_id,
        depth: limit,
        reason: mp_core::SnapshotReason::Init,
    })
}

#[cfg(feature = "live-http")]
pub fn fetch_open_interest_blocking(
    symbol: &str,
) -> Result<MarketEvent, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("https://fapi.binance.com/fapi/v1/openInterest?symbol={symbol}");
    let resp = reqwest::blocking::get(&url)?;
    let status = resp.status();
    if status.as_u16() == 429 {
        // 429 ⇒ RetryAfter semantics, never a generic error (COL-21).
        let wait = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60);
        tracing::warn!(
            symbol,
            retry_after_s = wait,
            "binance open-interest 429 — RetryAfter"
        );
        return Err(format!("Binance OI rate-limited (429); Retry-After {wait}s").into());
    }
    let raw: serde_json::Value = resp.error_for_status()?.json()?;
    let open_interest = raw
        .get("openInterest")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or("missing openInterest field in Binance response")?;
    Ok(MarketEvent::OpenInterest {
        oi_contracts: open_interest,
        oi_notional: f64::NAN,
    })
}

/// One Binance futures aggregated trade as returned by `GET /fapi/v1/aggTrades`
/// (COL-25). Ids are globally increasing per symbol, which is what makes a
/// `fromId` watermark a loss-free resume point.
#[derive(Debug, Clone, PartialEq)]
pub struct AggTrade {
    pub id: u64,
    pub price: f64,
    pub qty: f64,
    /// `m` = "is buyer the market maker": true ⇒ aggressor is the seller.
    pub maker: bool,
    pub exch_ts_ns: i64,
}

/// Parse the `GET /fapi/v1/aggTrades` response body (an array). Pure, so the
/// watermark logic and event mapping are unit-testable without HTTP.
pub fn parse_agg_trades(raw: &serde_json::Value) -> Result<Vec<AggTrade>, NormError> {
    let arr = raw
        .as_array()
        .ok_or(NormError::Parse("aggTrades: not an array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let id = u64_field(v, "a").ok_or(NormError::Parse("aggTrades a".into()))?;
        let price = f64_field(v, "p").ok_or(NormError::Parse("aggTrades p".into()))?;
        let qty = f64_field(v, "q").ok_or(NormError::Parse("aggTrades q".into()))?;
        let maker = v.get("m").and_then(|x| x.as_bool()).unwrap_or(false);
        let t = i64_field(v, "T").map(ms_to_ns).unwrap_or(0);
        out.push(AggTrade {
            id,
            price,
            qty,
            maker,
            exch_ts_ns: t,
        });
    }
    Ok(out)
}

/// One Binance futures mark-price/funding sample as returned by
/// `GET /fapi/v1/premiumIndex` (COL-28): mark price, index price, last
/// funding rate, next funding time. The WS `markPriceUpdate` stream normally
/// carries this ~1/s; when fstream silently drops that stream (spec 024
/// incident 2026-08-04), the collector polls this endpoint instead.
#[derive(Debug, Clone, PartialEq)]
pub struct PremiumIndex {
    pub mark: f64,
    pub index: f64,
    pub last_funding_rate: f64,
    pub next_funding_ts_ns: i64,
}

/// Parse the `GET /fapi/v1/premiumIndex` response body (a single object).
/// Pure, so the event mapping is unit-testable without HTTP. Mirrors the
/// `markPriceUpdate` WS shape: mark (`markPrice`), index (`indexPrice`), last
/// funding rate (`lastFundingRate`), next funding time (`nextFundingTime`, ms).
pub fn parse_premium_index(raw: &serde_json::Value) -> Result<PremiumIndex, NormError> {
    let mark =
        f64_field(raw, "markPrice").ok_or(NormError::Parse("premiumIndex markPrice".into()))?;
    let index = f64_field(raw, "indexPrice").unwrap_or(f64::NAN);
    let last_funding_rate = f64_field(raw, "lastFundingRate").unwrap_or(0.0);
    let next_funding_ts_ns = i64_field(raw, "nextFundingTime").map(ms_to_ns).unwrap_or(0);
    Ok(PremiumIndex {
        mark,
        index,
        last_funding_rate,
        next_funding_ts_ns,
    })
}

/// Fetch the latest futures mark price + funding rate via REST (COL-28).
/// Shares the 429 ⇒ RetryAfter convention (COL-21).
#[cfg(feature = "live-http")]
pub fn fetch_premium_index_blocking(
    symbol: &str,
) -> Result<PremiumIndex, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("https://fapi.binance.com/fapi/v1/premiumIndex?symbol={symbol}");
    let resp = reqwest::blocking::get(&url)?;
    let status = resp.status();
    if status.as_u16() == 429 {
        let wait = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60);
        tracing::warn!(
            symbol,
            retry_after_s = wait,
            "binance premiumIndex 429 — RetryAfter"
        );
        return Err(format!("Binance premiumIndex rate-limited (429); Retry-After {wait}s").into());
    }
    let raw: serde_json::Value = resp.error_for_status()?.json()?;
    parse_premium_index(&raw).map_err(|e| e.to_string().into())
}

/// Advance the trade watermark over a fresh REST batch and return the new
/// trades plus the count of ids skipped (COL-26). `fromId` is inclusive, so
/// the first returned trade may already be recorded — skipped as a duplicate.
/// A jump in ids is an honest loss report: the caller surfaces it as
/// `Status::GapDetected` so the recording never hides a gap.
pub fn advance_trade_watermark(watermark: &mut u64, trades: Vec<AggTrade>) -> (Vec<AggTrade>, u64) {
    let mut missing = 0u64;
    let mut fresh = Vec::new();
    for t in trades {
        if t.id <= *watermark {
            continue;
        }
        if *watermark > 0 && t.id > *watermark + 1 {
            missing += t.id - *watermark - 1;
        }
        *watermark = t.id;
        fresh.push(t);
    }
    (fresh, missing)
}

/// Fetch the latest futures aggregated trades via REST (COL-25). With
/// `from_id` the response starts at that id (inclusive) — the caller's
/// watermark resumes loss-free. `None` fetches the most recent window, used
/// on the very first poll. Shares the 429 ⇒ RetryAfter convention (COL-21).
#[cfg(feature = "live-http")]
pub fn fetch_agg_trades_blocking(
    symbol: &str,
    from_id: Option<u64>,
) -> Result<Vec<AggTrade>, Box<dyn std::error::Error + Send + Sync>> {
    let mut url = format!("https://fapi.binance.com/fapi/v1/aggTrades?symbol={symbol}&limit=500");
    if let Some(id) = from_id {
        url.push_str(&format!("&fromId={id}"));
    }
    let resp = reqwest::blocking::get(&url)?;
    let status = resp.status();
    if status.as_u16() == 429 {
        let wait = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60);
        tracing::warn!(
            symbol,
            retry_after_s = wait,
            "binance aggTrades 429 — RetryAfter"
        );
        return Err(format!("Binance aggTrades rate-limited (429); Retry-After {wait}s").into());
    }
    let raw: serde_json::Value = resp.error_for_status()?.json()?;
    parse_agg_trades(&raw).map_err(|e| e.to_string().into())
}

/// Map a REST premiumIndex sample into `MarkPrice` + `Funding` envelopes
/// (COL-28). The mapping is byte-identical to the WS `markPriceUpdate` branch
/// — same `MarkPrice { mark, index }` body, same `Funding { rate,
/// interval_s: 28_800, next_funding_ts_ns }` (the venue does not carry the
/// interval on either payload; 8h is the documented default) — so a
/// REST-fed recording and a WS-fed recording of the same mark/funding grade
/// identically. `recv_ts_ns` is the poll time (REST-injected stragglers are
/// handled by `monotonicize`).
pub fn apply_premium_index(
    normalizer: &mut BinanceNormalizer,
    symbol: &str,
    pi: &PremiumIndex,
    recv_ts_ns: i64,
    out: &mut Vec<mp_core::EventEnvelope>,
) {
    let id = normalizer
        .symbols_mut()
        .intern_default(mp_core::Venue::BinanceFutures, symbol);
    let seq = normalizer.seq();
    out.push(mp_core::EventEnvelope::new(
        mp_core::Venue::BinanceFutures,
        id,
        0,
        recv_ts_ns,
        seq,
        MarketEvent::MarkPrice {
            mark: pi.mark,
            index: pi.index,
        },
    ));
    let seq = normalizer.seq();
    out.push(mp_core::EventEnvelope::new(
        mp_core::Venue::BinanceFutures,
        id,
        0,
        recv_ts_ns,
        seq,
        MarketEvent::Funding {
            rate: pi.last_funding_rate,
            interval_s: 28_800,
            next_funding_ts_ns: pi.next_funding_ts_ns,
        },
    ));
}

/// Map REST aggTrades into `Trade` events through the normalizer (COL-25).
/// The mapping is byte-identical to the WS `aggTrade` branch — same side rule
/// (`m`), same id, same exchange timestamp — so a REST-fed recording and a
/// WS-fed recording of the same trades grade identically. `recv_ts_ns` is the
/// poll time (REST-injected stragglers are handled by `monotonicize`).
pub fn apply_agg_trades(
    normalizer: &mut BinanceNormalizer,
    symbol: &str,
    trades: &[AggTrade],
    recv_ts_ns: i64,
    out: &mut Vec<mp_core::EventEnvelope>,
) {
    let id = normalizer
        .symbols_mut()
        .intern_default(mp_core::Venue::BinanceFutures, symbol);
    for t in trades {
        let seq = normalizer.seq();
        out.push(mp_core::EventEnvelope::new(
            mp_core::Venue::BinanceFutures,
            id,
            t.exch_ts_ns,
            recv_ts_ns,
            seq,
            MarketEvent::Trade {
                price: t.price,
                qty: t.qty,
                side: if t.maker { Side::Sell } else { Side::Buy },
                trade_id: t.id,
            },
        ));
    }
}

/// First-delta straddle check (COL-23), wired into
/// [`crate::book_sync::BinanceBookSync::on_delta`]: a delta whose `U` is at or
/// below the snapshot's `lastUpdateId` overlaps the snapshot, so applying it
/// is consistent. Kept public for the unit tests that pin the rule.
#[must_use]
pub fn check_delta_continuity(first_delta_u: u64, snapshot_last_update_id: u64) -> bool {
    first_delta_u <= snapshot_last_update_id
}

/// Feed a REST depth snapshot directly into a normalizer as its initial book
/// seed (COL-23 / spec 020). Call this ONCE right after WS connect, before
/// the first `depthUpdate` message is processed. Injects a `BookSnapshot`
/// event into `out` and marks the book seeded so the normalizer never falls
/// back to synthetic-snapshot mode from the first delta.
///
/// This decouples the blocking REST call from the `normalize()` hot path:
/// the collector binary calls this on its startup thread, then starts the
/// WS poll loop — no HTTP inside `normalize()`.
#[cfg(feature = "live-http")]
pub fn inject_rest_depth_seed(
    normalizer: &mut BinanceNormalizer,
    symbol: &str,
    recv_ts_ns: i64,
    out: &mut Vec<mp_core::EventEnvelope>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    inject_rest_depth_seed_budgeted(normalizer, symbol, recv_ts_ns, out, None)
}

/// Same as [`inject_rest_depth_seed`] but with an optional [`RateBudget`]
/// (COL-21) so the binary can share one allowance across seed + reseed.
#[cfg(feature = "live-http")]
pub fn inject_rest_depth_seed_budgeted(
    normalizer: &mut BinanceNormalizer,
    symbol: &str,
    recv_ts_ns: i64,
    out: &mut Vec<mp_core::EventEnvelope>,
    budget: Option<&mut crate::rate::RateBudget>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::book_sync::SnapKind;
    use mp_core::SnapshotReason;

    let snap = fetch_depth_snapshot_blocking_budgeted(symbol, 100, true, budget)?;
    let MarketEvent::BookSnapshot {
        bids,
        asks,
        seq,
        depth,
        ..
    } = snap
    else {
        return Err("expected BookSnapshot from REST depth".into());
    };
    let id = normalizer
        .symbols_mut()
        .intern_default(mp_core::Venue::BinanceFutures, symbol);
    let kind = normalizer.seed_book(id, seq);
    out.push(mp_core::EventEnvelope::new(
        mp_core::Venue::BinanceFutures,
        id,
        0,
        recv_ts_ns,
        seq,
        MarketEvent::BookSnapshot {
            bids,
            asks,
            seq,
            depth,
            reason: if kind == SnapKind::Init {
                SnapshotReason::Init
            } else {
                SnapshotReason::GapResync
            },
        },
    ));
    tracing::info!(symbol, seq, "REST depth snapshot injected; book seeded");
    Ok(())
}

/// Spec-020 reseed path (COL-24): if a `pu` continuity gap desynced the book,
/// fetch a fresh REST snapshot, re-seed, and emit the snapshot event into
/// `out`. Returns `Ok(true)` when a reseed happened. No sleeping inline — the
/// caller invokes this from its normal poll loop iteration.
#[cfg(feature = "live-http")]
pub fn reseed_if_needed(
    normalizer: &mut BinanceNormalizer,
    symbol: &str,
    recv_ts_ns: i64,
    out: &mut Vec<mp_core::EventEnvelope>,
    budget: Option<&mut crate::rate::RateBudget>,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    if !normalizer.needs_reseed() {
        return Ok(false);
    }
    let recv_ts_ns = match recv_ts_ns {
        0 => wall_now_ns(), // caller delegates stamping to the lib edge
        ts => ts,
    };
    inject_rest_depth_seed_budgeted(normalizer, symbol, recv_ts_ns, out, budget)?;
    tracing::warn!(
        symbol,
        "depth book re-seeded from REST after pu gap (COL-24)"
    );
    Ok(true)
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
                if self.suppress_ws_trades {
                    tracing::debug!("aggTrade frame dropped: REST trade source active");
                    return Ok(());
                }
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
                let pu = u64_field(d, "pu").unwrap_or(0);
                let bids = parse_pair_levels(d.get("b"))?;
                let asks = parse_pair_levels(d.get("a"))?;
                let seeded = *self.seeded.get(&id).unwrap_or(&false);
                if !seeded {
                    // No REST snapshot yet (offline/test run, or the inject
                    // failed). Spec 020 leaves no honest synthetic fallback:
                    // drop deltas until the collector binary seeds via
                    // `inject_rest_depth_seed`. Other frame types flow freely.
                    tracing::debug!(symbol = %sym, "depth delta dropped: awaiting REST snapshot seed");
                    return Ok(());
                }
                let st = self.books.entry(id).or_default();
                match st.on_delta(first, last, pu) {
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
                        self.seeded.insert(id, false); // needs_reseed() now reports it
                        let seq = self.seq();
                        out.push(EventEnvelope::new(
                            Venue::BinanceFutures,
                            id,
                            exch,
                            recv_ts_ns,
                            seq,
                            MarketEvent::Status {
                                kind: StatusKind::GapDetected,
                                detail: format!("binance depth gap at U={first} u={last} pu={pu}"),
                            },
                        ));
                    }
                    DeltaAction::Drop => {}
                }
            }
            "markPriceUpdate" => {
                if self.suppress_ws_mark_price {
                    tracing::debug!(
                        "markPriceUpdate frame dropped: REST premiumIndex source active"
                    );
                    return Ok(());
                }
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
                    // Funding interval: Binance's markPriceUpdate payload does
                    // NOT carry an interval field (the `i` field is the index
                    // price, already read above). Interval lives on
                    // `fundingInfo`; absent a per-event value we keep the
                    // documented 8h default (28_800s) — an explicit assumption,
                    // logged at DEBUG so it is auditable, not silent.
                    let interval_s: u32 = 28_800;
                    tracing::debug!(
                        symbol = %sym,
                        interval_s,
                        "funding interval not carried on markPriceUpdate; assuming venue 8h default"
                    );
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::BinanceFutures,
                        id,
                        exch,
                        recv_ts_ns,
                        seq,
                        MarketEvent::Funding {
                            rate,
                            interval_s,
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

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn depth_payload(sym: &str, u: u64, uu: u64, pu: u64) -> String {
        format!(
            r#"{{"e":"depthUpdate","E":2,"s":"{sym}","U":{u},"u":{uu},"pu":{pu},"b":[["50000","1"]],"a":[]}}"#
        )
    }

    /// Seed a normalizer from a synthetic REST shape, then drive depth deltas.
    /// Uses `seed_book` (no HTTP) so the test is deterministic and offline.
    fn seeded_normalizer(snapshot_last: u64) -> BinanceNormalizer {
        let mut n = BinanceNormalizer::new();
        let id = n
            .symbols_mut()
            .intern_default(Venue::BinanceFutures, "BTCUSDT");
        let kind = n.seed_book(id, snapshot_last);
        assert_eq!(kind, crate::book_sync::SnapKind::Init);
        n
    }

    #[test]
    fn col_23_seq_continuity_verified() {
        let mut n = seeded_normalizer(100);
        let mut out = Vec::new();
        // First delta must STRADDLE the snapshot: U <= lastUpdateId <= u.
        n.normalize(
            1,
            depth_payload("BTCUSDT", 100, 105, 99).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(matches!(
            out[0].body,
            MarketEvent::BookDelta {
                first_seq: 100,
                last_seq: 105,
                ..
            }
        ));
        // Subsequent deltas chain via pu == prev_u.
        out.clear();
        n.normalize(
            2,
            depth_payload("BTCUSDT", 106, 110, 105).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(matches!(
            out[0].body,
            MarketEvent::BookDelta {
                first_seq: 106,
                last_seq: 110,
                ..
            }
        ));
        assert!(!n.needs_reseed());
    }

    #[test]
    fn col_24_mismatch_triggers_gap() {
        let mut n = seeded_normalizer(100);
        let mut out = Vec::new();
        // Snapshot says 100; first buffered delta starts at 105 — gap.
        n.normalize(
            1,
            depth_payload("BTCUSDT", 105, 110, 104).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(matches!(
            out[0].body,
            MarketEvent::Status {
                kind: StatusKind::GapDetected,
                ..
            }
        ));
        assert!(
            n.needs_reseed(),
            "gap must set the reseed flag for the driver"
        );
        // While desynced, further deltas drop silently until a re-seed.
        out.clear();
        n.normalize(
            2,
            depth_payload("BTCUSDT", 111, 115, 110).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn col_24_pu_mismatch_mid_stream_triggers_gap() {
        let mut n = seeded_normalizer(100);
        let mut out = Vec::new();
        n.normalize(
            1,
            depth_payload("BTCUSDT", 100, 105, 99).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(matches!(out[0].body, MarketEvent::BookDelta { .. }));
        out.clear();
        // pu (109) != prev_u (105) — venue missed a delta.
        n.normalize(
            2,
            depth_payload("BTCUSDT", 110, 115, 109).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(matches!(
            out[0].body,
            MarketEvent::Status {
                kind: StatusKind::GapDetected,
                ..
            }
        ));
        assert!(n.needs_reseed());
    }

    #[test]
    fn col_24_reseed_clears_flag_and_resyncs() {
        let mut n = seeded_normalizer(100);
        let mut out = Vec::new();
        n.normalize(
            1,
            depth_payload("BTCUSDT", 105, 110, 104).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(n.needs_reseed());
        // Driver observes the flag and re-seeds via the same path as startup.
        let id = n
            .symbols_mut()
            .intern_default(Venue::BinanceFutures, "BTCUSDT");
        let kind = n.seed_book(id, 500);
        assert_eq!(kind, crate::book_sync::SnapKind::Resync);
        assert!(!n.needs_reseed());
        // And new deltas validate against the new snapshot.
        out.clear();
        n.normalize(
            2,
            depth_payload("BTCUSDT", 500, 505, 499).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(matches!(out[0].body, MarketEvent::BookDelta { .. }));
    }

    #[test]
    fn col_23_deltas_before_seed_drop_without_synthetic_snapshot() {
        // The synthetic-seed fallback was removed (spec 020): with no REST
        // snapshot, depth deltas are dropped (never turned into a fake book).
        let mut n = BinanceNormalizer::new();
        let mut out = Vec::new();
        n.normalize(
            1,
            depth_payload("BTCUSDT", 100, 105, 99).as_bytes(),
            &mut out,
        )
        .unwrap();
        assert!(
            out.is_empty(),
            "no snapshot, no depth events before REST seed"
        );
    }

    #[test]
    fn col_25_parse_agg_trades_from_rest_json() {
        // Real /fapi/v1/aggTrades response shape: strings for p/q, numbers
        // for a/T, bool for m.
        let raw: serde_json::Value = serde_json::from_str(
            r#"[{"a":601,"p":"50000","q":"0.1","f":100,"l":101,"T":1673280000000,"m":true},
                {"a":602,"p":"50001.5","q":"2","f":102,"l":103,"T":1673280001000,"m":false}]"#,
        )
        .unwrap();
        let trades = parse_agg_trades(&raw).unwrap();
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].id, 601);
        assert_eq!(trades[0].price, 50_000.0);
        assert_eq!(trades[0].qty, 0.1);
        assert!(trades[0].maker);
        assert_eq!(trades[0].exch_ts_ns, 1_673_280_000_000_000_000);
        assert!(!trades[1].maker);
    }

    #[test]
    fn col_25_parse_agg_trades_rejects_non_array() {
        let raw: serde_json::Value = serde_json::from_str(r#"{"a":1}"#).unwrap();
        assert!(parse_agg_trades(&raw).is_err());
    }

    #[test]
    fn col_26_watermark_dedup_and_gap_counting() {
        // First batch establishes the watermark without a gap.
        let mut wm = 0u64;
        let (fresh, missing) = advance_trade_watermark(
            &mut wm,
            vec![
                AggTrade {
                    id: 10,
                    price: 1.0,
                    qty: 1.0,
                    maker: false,
                    exch_ts_ns: 0,
                },
                AggTrade {
                    id: 11,
                    price: 1.0,
                    qty: 1.0,
                    maker: false,
                    exch_ts_ns: 0,
                },
            ],
        );
        assert_eq!(wm, 11);
        assert_eq!(missing, 0);
        assert_eq!(fresh.len(), 2);

        // fromId is inclusive — the id-11 overlap is a duplicate, skipped.
        let (fresh, missing) = advance_trade_watermark(
            &mut wm,
            vec![
                AggTrade {
                    id: 11,
                    price: 1.0,
                    qty: 1.0,
                    maker: false,
                    exch_ts_ns: 0,
                },
                AggTrade {
                    id: 12,
                    price: 1.0,
                    qty: 1.0,
                    maker: false,
                    exch_ts_ns: 0,
                },
            ],
        );
        assert_eq!(wm, 12);
        assert_eq!(missing, 0);
        assert_eq!(fresh.len(), 1);

        // A jump of 15→20 (watermark 12) means 13..14 AND 16..19 were missed:
        // every non-contiguous advance is counted, per segment — honest gap.
        let (fresh, missing) = advance_trade_watermark(
            &mut wm,
            vec![
                AggTrade {
                    id: 15,
                    price: 1.0,
                    qty: 1.0,
                    maker: false,
                    exch_ts_ns: 0,
                },
                AggTrade {
                    id: 20,
                    price: 1.0,
                    qty: 1.0,
                    maker: false,
                    exch_ts_ns: 0,
                },
            ],
        );
        assert_eq!(wm, 20);
        assert_eq!(missing, 6);
        assert_eq!(fresh.len(), 2);
    }

    #[test]
    fn col_26_rest_trades_match_ws_normalization() {
        // The same trade through the WS path and through apply_agg_trades must
        // produce identical events — a mixed or migrated recording grades the
        // same either way (the audit compares streams, not transport).
        let mut n = BinanceNormalizer::new();
        let mut out = Vec::new();
        n.normalize(
            1,
            br#"{"e":"aggTrade","E":1,"s":"BTCUSDT","a":5,"p":"50000","q":"0.1","T":1,"m":true}"#,
            &mut out,
        )
        .unwrap();
        let MarketEvent::Trade {
            price,
            qty,
            side,
            trade_id,
        } = &out[0].body
        else {
            panic!("expected Trade");
        };
        let ws_trade = (price, qty, side, trade_id);

        let mut n2 = BinanceNormalizer::new();
        let mut out2 = Vec::new();
        apply_agg_trades(
            &mut n2,
            "BTCUSDT",
            &[AggTrade {
                id: 5,
                price: 50_000.0,
                qty: 0.1,
                maker: true,
                exch_ts_ns: 1_000_000,
            }],
            7_000_000,
            &mut out2,
        );
        let MarketEvent::Trade {
            price,
            qty,
            side,
            trade_id,
        } = &out2[0].body
        else {
            panic!("expected Trade");
        };
        assert_eq!(ws_trade, (price, qty, side, trade_id));
        // Envelope: exchange ts from the trade's T, recv stamped at poll time.
        assert_eq!(out2[0].exch_ts_ns, 1_000_000);
        assert_eq!(out2[0].recv_ts_ns, 7_000_000);
    }

    #[test]
    fn col_27_suppress_ws_trades_drops_frames_but_rest_path_flows() {
        let mut n = BinanceNormalizer::new();
        n.set_suppress_ws_trades(true);
        let mut out = Vec::new();
        n.normalize(
            1,
            br#"{"e":"aggTrade","E":1,"s":"BTCUSDT","a":5,"p":"50000","q":"0.1","T":1,"m":true}"#,
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty(), "WS aggTrade suppressed in rest mode");

        // The REST path still emits the same trade.
        let mut out2 = Vec::new();
        apply_agg_trades(
            &mut n,
            "BTCUSDT",
            &[AggTrade {
                id: 5,
                price: 50_000.0,
                qty: 0.1,
                maker: true,
                exch_ts_ns: 1_000_000,
            }],
            7_000_000,
            &mut out2,
        );
        assert_eq!(out2.len(), 1);
        assert!(matches!(out2[0].body, MarketEvent::Trade { .. }));
    }

    #[test]
    fn col_28_parse_premium_index_from_rest_json() {
        // Real /fapi/v1/premiumIndex response shape: strings for prices/rates,
        // ms number for nextFundingTime.
        let raw: serde_json::Value = serde_json::from_str(
            r#"{"symbol":"BTCUSDT","markPrice":"28200.5","indexPrice":"28195.3",
                "estimatedSettlePrice":"28205.5","lastFundingRate":"0.000125",
                "interestRate":"0.0001","nextFundingTime":1621267200000,"time":1621267199000}"#,
        )
        .unwrap();
        let pi = parse_premium_index(&raw).unwrap();
        assert_eq!(pi.mark, 28_200.5);
        assert_eq!(pi.index, 28_195.3);
        assert_eq!(pi.last_funding_rate, 0.000125);
        assert_eq!(pi.next_funding_ts_ns, 1_621_267_200_000_000_000);
    }

    #[test]
    fn col_28_parse_premium_index_missing_mark_is_error() {
        let raw: serde_json::Value = serde_json::from_str(r#"{"symbol":"BTCUSDT"}"#).unwrap();
        assert!(parse_premium_index(&raw).is_err());
    }

    #[test]
    fn col_28_rest_premium_index_matches_ws_normalization() {
        // The same mark/funding through the WS path and through
        // apply_premium_index must produce identical event bodies — a mixed or
        // migrated recording grades the same either way.
        let mut n = BinanceNormalizer::new();
        let mut out = Vec::new();
        n.normalize(
            1,
            br#"{"e":"markPriceUpdate","E":1,"s":"BTCUSDT","p":"28200.5","i":"28195.3","r":"0.000125","T":1621267200000}"#,
            &mut out,
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        let (
            MarketEvent::MarkPrice { mark, index },
            MarketEvent::Funding {
                rate,
                interval_s,
                next_funding_ts_ns,
            },
        ) = (&out[0].body, &out[1].body)
        else {
            panic!("expected MarkPrice then Funding");
        };
        let ws = (*mark, *index, *rate, *interval_s, *next_funding_ts_ns);

        let mut n2 = BinanceNormalizer::new();
        let mut out2 = Vec::new();
        apply_premium_index(
            &mut n2,
            "BTCUSDT",
            &PremiumIndex {
                mark: 28_200.5,
                index: 28_195.3,
                last_funding_rate: 0.000125,
                next_funding_ts_ns: 1_621_267_200_000_000_000,
            },
            7_000_000,
            &mut out2,
        );
        assert_eq!(out2.len(), 2);
        let (
            MarketEvent::MarkPrice { mark, index },
            MarketEvent::Funding {
                rate,
                interval_s,
                next_funding_ts_ns,
            },
        ) = (&out2[0].body, &out2[1].body)
        else {
            panic!("expected MarkPrice then Funding");
        };
        assert_eq!(ws, (*mark, *index, *rate, *interval_s, *next_funding_ts_ns));
        // Envelope: recv stamped at poll time.
        assert_eq!(out2[0].recv_ts_ns, 7_000_000);
    }

    #[test]
    fn col_28_suppress_ws_mark_price_drops_frames_but_rest_path_flows() {
        let mut n = BinanceNormalizer::new();
        n.set_suppress_ws_mark_price(true);
        let mut out = Vec::new();
        n.normalize(
            1,
            br#"{"e":"markPriceUpdate","E":1,"s":"BTCUSDT","p":"28200.5","i":"28195.3","r":"0.000125","T":1621267200000}"#,
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty(), "WS markPriceUpdate suppressed in rest mode");

        // The REST path still emits the same MarkPrice + Funding.
        let mut out2 = Vec::new();
        apply_premium_index(
            &mut n,
            "BTCUSDT",
            &PremiumIndex {
                mark: 28_200.5,
                index: 28_195.3,
                last_funding_rate: 0.000125,
                next_funding_ts_ns: 1_621_267_200_000_000_000,
            },
            7_000_000,
            &mut out2,
        );
        assert_eq!(out2.len(), 2);
        assert!(matches!(out2[0].body, MarketEvent::MarkPrice { .. }));
        assert!(matches!(out2[1].body, MarketEvent::Funding { .. }));
    }
}
