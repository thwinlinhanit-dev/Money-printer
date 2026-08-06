//! Deribit public options market-data normalizer (spec 031, OPT-1..10).
//! Subscribes to public WS channels (`book.{instr}.{group}.{depth}`, `trades.
//! {instr}.{interval}`, `ticker.{instr}.{interval}`) via the generic WS
//! transport — no auth for market data (OPT-1) — and emits option events with
//! parsed instrument metadata ([`OptionLeg`]: underlying, strike, expiry,
//! call/put, OPT-2). Recording only (OPT-6): no vol-surface/GEX/greeks
//! analytics here.
//!
//! Book sync follows Deribit's documented algorithm (COL-7/OPT-4): each book
//! message is a `snapshot` (full book) or a `change` (incremental deltas)
//! carrying `change_id`/`prev_change_id`. A change whose `prev_change_id`
//! does not match the last observed id means messages were missed → emit
//! `Status::GapDetected` and drop changes until the next snapshot.

use crate::json::{f64_field, i64_field, str_field, u64_field};
use crate::normalize::{NormError, Normalizer};
use mp_core::{
    EventEnvelope, Level, Levels, MarketEvent, OptionGreeks, OptionKind, OptionLeg, Side,
    StatusKind, SymbolId, SymbolTable, Venue,
};
use serde_json::Value;
use smallvec::SmallVec;
use std::collections::BTreeMap;

/// Per-instrument book continuity state (CONV-10: BTreeMap — deterministic).
#[derive(Debug, Clone, Default)]
struct BookState {
    last_change_id: u64,
    /// Set when a change gap is detected; changes drop until a snapshot.
    stale: bool,
    /// Whether a snapshot has ever been seen (first change without a
    /// snapshot has no baseline to validate against).
    initialized: bool,
}

#[derive(Default)]
pub struct DeribitNormalizer {
    symbols: SymbolTable,
    books: BTreeMap<String, BookState>,
    next_seq: u64,
}

impl DeribitNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn sym(&mut self, s: &str) -> SymbolId {
        self.symbols.intern_default(Venue::Deribit, s)
    }
}

/// Parse a Deribit option instrument name into leg metadata (OPT-2).
///
/// Format: `{UNDERLYING}-{DDMMMYY}-{STRIKE}-{C|P}` e.g. `BTC-28JUN26-100000-C`.
/// Strike scaling is per underlying: BTC/SOL strikes are in USD cents
/// (100000 → $1000.00), ETH strikes in milli-USD (100000 → $100.00) —
/// documented in spec 031 Decisions (verify against venue docs if new
/// underlyings are added).
pub fn parse_instrument_name(name: &str) -> Option<OptionLeg> {
    let mut parts = name.split('-');
    let underlying_full = parts.next()?;
    let expiry = parts.next()?;
    let strike = parts.next()?;
    let kind = parts.next()?;
    if parts.next().is_some() {
        return None; // more than 4 segments — not an option instrument
    }
    let underlying = underlying_full.split('_').next()?.to_owned();
    let kind = match kind {
        "C" => OptionKind::Call,
        "P" => OptionKind::Put,
        _ => return None,
    };
    let strike_int: u64 = strike.parse().ok()?;
    let divisor: f64 = if underlying == "ETH" { 1000.0 } else { 100.0 };
    let strike = strike_int as f64 / divisor;
    let expiry_ts_ns = parse_expiry(expiry)?;
    Some(OptionLeg {
        underlying,
        strike,
        expiry_ts_ns,
        kind,
    })
}

/// Parse Deribit expiry `DDMMMYY` (e.g. `28JUN26`) into ns at UTC midnight
/// of that date (settlement time 08:00 UTC is not captured — see Decisions).
fn parse_expiry(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 7 {
        return None;
    }
    let day: i64 = std::str::from_utf8(&b[0..2]).ok()?.parse().ok()?;
    let month = match &b[2..5] {
        b"JAN" => 1,
        b"FEB" => 2,
        b"MAR" => 3,
        b"APR" => 4,
        b"MAY" => 5,
        b"JUN" => 6,
        b"JUL" => 7,
        b"AUG" => 8,
        b"SEP" => 9,
        b"OCT" => 10,
        b"NOV" => 11,
        b"DEC" => 12,
        _ => return None,
    };
    let yy: i64 = std::str::from_utf8(&b[5..7]).ok()?.parse().ok()?;
    if !(1..=31).contains(&day) {
        return None;
    }
    // 2-digit year: 2000 + yy (Deribit options are dated 2020+; documented).
    let y = 2000 + yy;
    crate::fred::parse_fred_date(&format!("{y:04}-{month:02}-{day:02}"))
}

/// Normalize Deribit book level entries `[action, price, amount]` (a 2-element
/// `[price, amount]` form is also accepted). `delete` → qty 0.0 (removal).
fn parse_book_levels(v: Option<&Value>) -> Result<Levels, NormError> {
    let mut out: Levels = SmallVec::new();
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return Ok(out);
    };
    for lvl in arr {
        let la = lvl
            .as_array()
            .ok_or_else(|| NormError::Parse("deribit level".into()))?;
        // [action, price, amount] or [price, amount].
        let (price, amount) = match la.len() {
            3 => {
                let action = la[0].as_str().unwrap_or("");
                let p = crate::json::pair_num_like(&la[1]).unwrap_or(f64::NAN);
                let a = crate::json::pair_num_like(&la[2]).unwrap_or(f64::NAN);
                let a = if action == "delete" { 0.0 } else { a };
                (p, a)
            }
            2 => (
                crate::json::pair_num_like(&la[0]).unwrap_or(f64::NAN),
                crate::json::pair_num_like(&la[1]).unwrap_or(f64::NAN),
            ),
            _ => continue,
        };
        if price > 0.0 && amount.is_finite() {
            out.push((price, amount) as Level);
        }
    }
    Ok(out)
}

impl Normalizer for DeribitNormalizer {
    fn venue(&self) -> Venue {
        Venue::Deribit
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let v: Value =
            serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;

        // JSON-RPC error frames are malformed-from-our-perspective frames
        // (COL-6) — checked FIRST because they also carry an `id` (an error
        // response must not be mistaken for a subscription ack).
        if v.get("error").is_some() {
            return Err(NormError::Parse("deribit jsonrpc error frame".into()));
        }
        // Subscription acks / heartbeats: {"id": n, "result": [...]} → ignore.
        if v.get("result").is_some() || v.get("id").is_some() && v.get("method").is_none() {
            return Ok(());
        }
        if str_field(&v, "method") != Some("subscription") {
            return Ok(()); // unknown message shape — ignore
        }

        let params = v
            .get("params")
            .ok_or_else(|| NormError::Parse("subscription lacks params".into()))?;
        let channel = str_field(params, "channel")
            .ok_or_else(|| NormError::Parse("subscription lacks channel".into()))?;
        let data = params
            .get("data")
            .ok_or_else(|| NormError::Parse("subscription lacks data".into()))?;

        // channel = "<type>.<instrument>.<rest…>" — the instrument name
        // contains '-' but never '.', so the second dot-segment is the
        // instrument (e.g. "book.BTC-28JUN26-100000-C.10.100ms").
        let mut parts = channel.split('.');
        let kind = parts.next().unwrap_or("");
        let instrument = parts.next().unwrap_or("");

        let Some(leg) = parse_instrument_name(instrument) else {
            // Non-option instrument on the channel (should not be subscribed)
            // or malformed name — skip honestly (OPT-9: no panic).
            return Ok(());
        };
        let id = self.sym(instrument);
        let exch = i64_field(data, "timestamp")
            .map(|ms| ms.saturating_mul(1_000_000))
            .unwrap_or(0);

        match kind {
            "book" => {
                let is_snapshot = str_field(data, "type") == Some("snapshot");
                let change_id = u64_field(data, "change_id").unwrap_or(0);

                if is_snapshot {
                    // Full book: reset continuity state (COL-7).
                    self.books.insert(
                        instrument.to_owned(),
                        BookState {
                            last_change_id: change_id,
                            stale: false,
                            initialized: true,
                        },
                    );
                    let bids = parse_book_levels(data.get("bids"))?;
                    let asks = parse_book_levels(data.get("asks"))?;
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Deribit,
                        id,
                        exch,
                        recv_ts_ns,
                        seq,
                        MarketEvent::OptionBook {
                            leg,
                            bids,
                            asks,
                            change_id,
                            is_snapshot: true,
                        },
                    ));
                    return Ok(());
                }

                // Change message. Read continuity state without holding the
                // books borrow across `self.seq()`.
                let state = self.books.get(instrument).cloned().unwrap_or_default();
                if !state.initialized {
                    // No baseline yet — cannot validate continuity.
                    return Ok(());
                }
                if state.stale {
                    // Still waiting for the resync snapshot.
                    return Ok(());
                }
                let prev_change_id = u64_field(data, "prev_change_id").unwrap_or(0);
                if prev_change_id != state.last_change_id {
                    // Missed changes (Deribit documented algorithm) — gap.
                    if let Some(st) = self.books.get_mut(instrument) {
                        st.stale = true;
                    }
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Deribit,
                        id,
                        exch,
                        recv_ts_ns,
                        seq,
                        MarketEvent::Status {
                            kind: StatusKind::GapDetected,
                            detail: format!(
                                "deribit book gap on {instrument}: prev_change_id={prev_change_id} expected={}",
                                state.last_change_id
                            ),
                        },
                    ));
                    return Ok(());
                }
                if let Some(st) = self.books.get_mut(instrument) {
                    st.last_change_id = change_id;
                }
                let bids = parse_book_levels(data.get("bids"))?;
                let asks = parse_book_levels(data.get("asks"))?;
                let seq = self.seq();
                out.push(EventEnvelope::new(
                    Venue::Deribit,
                    id,
                    exch,
                    recv_ts_ns,
                    seq,
                    MarketEvent::OptionBook {
                        leg,
                        bids,
                        asks,
                        change_id,
                        is_snapshot: false,
                    },
                ));
            }
            "trades" => {
                let Some(arr) = data.as_array() else {
                    return Ok(());
                };
                for t in arr {
                    let price = f64_field(t, "price").unwrap_or(f64::NAN);
                    let qty = f64_field(t, "amount").unwrap_or(f64::NAN);
                    if !price.is_finite() || !qty.is_finite() || price <= 0.0 {
                        continue;
                    }
                    let side = match str_field(t, "direction") {
                        Some("sell") => Side::Sell,
                        _ => Side::Buy,
                    };
                    // trade_id is a venue string; use it when numeric, else
                    // fall back to the numeric trade_seq (spec 001 Decision:
                    // string/u128 ids are hashed/truncated at the boundary).
                    let trade_id = u64_field(t, "trade_id")
                        .or_else(|| u64_field(t, "trade_seq"))
                        .unwrap_or(0);
                    let seq = self.seq();
                    out.push(EventEnvelope::new(
                        Venue::Deribit,
                        id,
                        exch,
                        recv_ts_ns,
                        seq,
                        MarketEvent::OptionTrade {
                            leg: leg.clone(),
                            price,
                            qty,
                            side,
                            trade_id,
                        },
                    ));
                }
            }
            "ticker" => {
                let mark_iv = f64_field(data, "mark_iv").unwrap_or(f64::NAN);
                let mark_price = f64_field(data, "mark_price").unwrap_or(f64::NAN);
                let underlying_price = f64_field(data, "underlying_price").unwrap_or(f64::NAN);
                let open_interest = f64_field(data, "open_interest").unwrap_or(f64::NAN);
                let greeks = data.get("greeks").map(|g| OptionGreeks {
                    delta: f64_field(g, "delta").unwrap_or(f64::NAN),
                    gamma: f64_field(g, "gamma").unwrap_or(f64::NAN),
                    theta: f64_field(g, "theta").unwrap_or(f64::NAN),
                    vega: f64_field(g, "vega").unwrap_or(f64::NAN),
                });
                let seq = self.seq();
                out.push(EventEnvelope::new(
                    Venue::Deribit,
                    id,
                    exch,
                    recv_ts_ns,
                    seq,
                    MarketEvent::OptionTicker {
                        leg,
                        mark_iv,
                        mark_price,
                        underlying_price,
                        open_interest,
                        greeks,
                    },
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
        // Reconnect: book state is untrusted until a fresh snapshot (COL-7).
        self.books.clear();
    }
}

#[cfg(feature = "live-http")]
pub mod rest {
    //! Blocking Deribit public JSON-RPC helpers (OPT-1: market data needs no
    //! auth). Instrument discovery for the subscription filter (OPT-7).
    //! Behind `live-http`.

    use super::*;

    pub const DERIBIT_HTTP: &str = "https://www.deribit.com/api/v2";

    /// Filter bounds for the option instrument subscription set (spec 031
    /// Decisions: near-expiry subset to bound volume).
    #[derive(Debug, Clone, Copy)]
    pub struct InstrumentFilter {
        /// Maximum instruments to subscribe (cap the channel count).
        pub max_instruments: usize,
        /// Only instruments expiring within this many days of `now`.
        pub expiry_window_days: u64,
    }

    impl Default for InstrumentFilter {
        fn default() -> Self {
            Self {
                max_instruments: 200,
                expiry_window_days: 45,
            }
        }
    }

    /// List active option instrument names for `currency` (`public/get_instruments`),
    /// filtered to a near-expiry subset (OPT-7 default).
    pub fn discover_option_instruments_blocking(
        currency: &str,
        filter: &InstrumentFilter,
        now_ns: i64,
    ) -> Result<Vec<String>, String> {
        let url = format!(
            "{DERIBIT_HTTP}/public/get_instruments?currency={currency}&kind=option&expired=false"
        );
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("deribit get_instruments: {e}"))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| format!("deribit body: {e}"))?;
        if !status.is_success() {
            return Err(format!("deribit HTTP {status}: {text}"));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("deribit json: {e}"))?;
        let result = v
            .get("result")
            .and_then(|r| r.as_array())
            .ok_or_else(|| "deribit get_instruments result missing".to_string())?;

        let cutoff = now_ns
            .saturating_add((filter.expiry_window_days as i64).saturating_mul(86_400_000_000_000));
        let mut names: Vec<String> = Vec::new();
        for inst in result {
            let Some(name) = str_field(inst, "instrument_name") else {
                continue;
            };
            let Some(leg) = parse_instrument_name(name) else {
                continue;
            };
            if leg.expiry_ts_ns > cutoff {
                continue; // outside the near-expiry window
            }
            names.push(name.to_owned());
            if names.len() >= filter.max_instruments {
                break;
            }
        }
        // Deterministic order (CONV-10): the venue list is already ordered;
        // sort for stability across runs regardless.
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_instrument_name;
    use mp_core::OptionKind;

    #[test]
    fn opt_2_instrument_name_parses_btc_call() {
        let leg = parse_instrument_name("BTC-28JUN26-100000-C").unwrap();
        assert_eq!(leg.underlying, "BTC");
        assert_eq!(leg.strike, 1000.0); // cents of USD
        assert_eq!(leg.kind, OptionKind::Call);
        // 2026-06-28T00:00:00Z (20632 days after the epoch).
        assert_eq!(leg.expiry_ts_ns, 1_782_604_800_000_000_000);
    }

    #[test]
    fn opt_2_instrument_name_parses_eth_put() {
        let leg = parse_instrument_name("ETH-30SEP26-5000-P").unwrap();
        assert_eq!(leg.underlying, "ETH");
        assert_eq!(leg.strike, 5.0); // milli-USD
        assert_eq!(leg.kind, OptionKind::Put);
    }

    #[test]
    fn opt_9_malformed_names_return_none() {
        assert_eq!(parse_instrument_name("BTC-PERPETUAL"), None);
        assert_eq!(parse_instrument_name("BTC-28JUN26-100000"), None);
        assert_eq!(parse_instrument_name("BTC-28JUN26-abc-C"), None);
        assert_eq!(parse_instrument_name("BTC-99XXX26-100000-C"), None);
        assert_eq!(parse_instrument_name(""), None);
    }
}
