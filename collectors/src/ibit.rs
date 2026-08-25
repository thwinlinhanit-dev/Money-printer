//! CBOE IBIT options-chain recorder (spec 040, IBI-1..8).
//!
//! Polls CBOE's free delayed quotes endpoint (HTTP REST, 15-min lag) and
//! normalizes the options-chain snapshot into spec 001
//! [`MarketEvent::OptionTicker`] / [`MarketEvent::OptionTrade`] events with
//! `venue = Cboe` and `underlying = "IBIT"` (IBI-1). Contract multiplier is
//! 100 shares (IBI-2) — recorded in every interned [`SymbolMeta`].
//!
//! ## Known approximation (spec 040 Decisions, review entry)
//! Delayed EOD chain data has NO aggressor side. Session volume is emitted as
//! one aggregate [`MarketEvent::OptionTrade`] per contract with the
//! direction-by-kind convention (call volume ⇒ `Side::Buy`, put volume ⇒
//! `Side::Sell`). Flow features on IBIT are therefore direction-by-kind in
//! v1; the OPRA real-time tape (v2) replaces this with true aggressor side.
//!
//! Parse-canary (IBI-1): a poll whose `quotes` array is non-empty but yields
//! ZERO parsed contracts is schema drift, not silence — callers MUST emit a
//! Status event ([`crate::staleness`]/COL-3 semantics).

use crate::json::{f64_field, str_field};
use crate::normalize::{NormError, Normalizer};
use mp_core::{
    EventEnvelope, MarketEvent, OptionGreeks, OptionKind, OptionLeg, Side, SymbolId, SymbolTable,
    Venue,
};
use serde_json::Value;

/// Default CBOE delayed-quotes endpoint (free, no auth, ~15-min lag).
pub const DEFAULT_CBOE_URL: &str =
    "https://cdn.cboe.com/api/global/delayed_quotes/options/IBIT.json";

/// US equity standard contract size (IBI-2).
pub const IBIT_CONTRACT_MULTIPLIER: f64 = 100.0;

/// Parse an OCC option symbol: `{ROOT}{YYMMDD}{C|P}{STRIKE x1000}` —
/// e.g. `IBIT260918C00098000` → IBIT, 2026-09-18, Call, $98.00. Roots may
/// contain letters only; the date is exactly 6 digits, the strike exactly 8.
pub fn parse_occ_symbol(sym: &str) -> Option<OptionLeg> {
    let bytes = sym.as_bytes();
    if bytes.len() < 15 {
        return None;
    }
    // Root = leading alpha run.
    let mut root_end = 0usize;
    while root_end < bytes.len() && bytes[root_end].is_ascii_uppercase() {
        root_end += 1;
    }
    if root_end == 0 || root_end + 15 != bytes.len() {
        return None;
    }
    let rest = &sym[root_end..];
    let yy: u32 = rest.get(0..2)?.parse().ok()?;
    let mm: u32 = rest.get(2..4)?.parse().ok()?;
    let dd: u32 = rest.get(4..6)?.parse().ok()?;
    if !(1..=12).contains(&mm) || !(1..=31).contains(&dd) {
        return None;
    }
    let kind = match rest.as_bytes()[6] {
        b'C' => OptionKind::Call,
        b'P' => OptionKind::Put,
        _ => return None,
    };
    let strike_scaled: u64 = rest.get(7..15)?.parse().ok()?;
    if strike_scaled == 0 {
        return None;
    }
    // OCC 2-digit year: 0..49 → 2000s, 50..99 → 1900s (options are all 20xx).
    let year = 2000 + yy as i64;
    Some(OptionLeg {
        underlying: sym[..root_end].to_owned(),
        strike: strike_scaled as f64 / 1000.0,
        expiry_ts_ns: utc_midnight_ns(year, mm as i64, dd as i64)?,
        kind,
    })
}

/// Days-from-civil → ns at UTC midnight (pure arithmetic, no chrono).
fn utc_midnight_ns(year: i64, month: i64, day: i64) -> Option<i64> {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    // Hinnant days_from_civil: shift so day 0 = 1970-01-01.
    let days = era * 146_097 + doe - 719_468;
    days.checked_mul(86_400_000_000_000)
}

/// CBOE delayed-chain normalizer (spec 040). Stateful only in the symbol
/// table + sequence counter; the chain is a snapshot, so there is no book
/// continuity to maintain (reset_books is a no-op).
#[derive(Debug, Default)]
pub struct CboeChainNormalizer {
    symbols: SymbolTable,
    next_seq: u64,
}

impl CboeChainNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Intern an IBIT option symbol with `contract_multiplier = 100`
    /// (IBI-2 — every aggregate computation for IBIT uses this).
    fn sym(&mut self, venue_symbol: &str) -> SymbolId {
        let mult = IBIT_CONTRACT_MULTIPLIER;
        self.symbols.intern(Venue::Cboe, venue_symbol, move |id| {
            let mut m = mp_core::SymbolMeta::new(
                id,
                Venue::Cboe,
                venue_symbol,
                "IBIT",
                "USD",
                mp_core::InstrumentKind::Option,
                0.01,
                0.01,
                1.0,
            );
            m.contract_multiplier = mult;
            m
        })
    }
}

impl Normalizer for CboeChainNormalizer {
    fn venue(&self) -> Venue {
        Venue::Cboe
    }

    fn reset_books(&mut self) {}

    fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    /// Normalize one chain-snapshot payload. Unknown/blank payloads are
    /// ignored (`Ok`); a structurally-broken payload is a parse error (COL-6)
    /// which the caller counts for the parse-canary (IBI-1).
    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let v: Value = serde_json::from_slice(payload)
            .map_err(|e| NormError::Parse(format!("cboe json: {e}")))?;
        // Tolerant quote-array discovery: CBOE has shipped both
        // `data.options[]` and `data.quotes[]`; some shapes inline the array.
        let quotes = v
            .pointer("/data/options")
            .and_then(|q| q.as_array())
            .or_else(|| v.pointer("/data/quotes").and_then(|q| q.as_array()))
            .or_else(|| v.get("data").and_then(|d| d.as_array()));
        let Some(quotes) = quotes else {
            return Ok(()); // not a chain snapshot — ignore (heartbeat-only poll)
        };
        // Underlying spot: `data.current_price` (fallbacks: close / last).
        let spot = v
            .pointer("/data/current_price")
            .and_then(Value::as_f64)
            .or_else(|| f64_field(&v, "close"))
            .or_else(|| f64_field(&v, "last"))
            .unwrap_or(f64::NAN);
        let mut seq = self.seq();
        for q in quotes {
            if !q.is_object() {
                continue;
            }
            let Some(sym) = str_field(q, "option")
                .or_else(|| str_field(q, "symbol"))
                .map(str::to_owned)
            else {
                continue; // non-option row (e.g. the underlying itself)
            };
            let Some(leg) = parse_occ_symbol(&sym) else {
                continue;
            };
            let symbol = self.sym(&sym);
            seq += 1;
            let greeks = {
                let d = f64_field(q, "delta");
                let g = f64_field(q, "gamma");
                let t = f64_field(q, "theta");
                let ve = f64_field(q, "vega");
                match (d, g, t, ve) {
                    (Some(d), Some(g), Some(t), Some(ve))
                        if d.is_finite() && g.is_finite() && t.is_finite() && ve.is_finite() =>
                    {
                        Some(OptionGreeks {
                            delta: d,
                            gamma: g,
                            theta: t,
                            vega: ve,
                        })
                    }
                    _ => None,
                }
            };
            out.push(EventEnvelope::new(
                Venue::Cboe,
                symbol,
                recv_ts_ns,
                recv_ts_ns,
                seq,
                MarketEvent::OptionTicker {
                    leg: leg.clone(),
                    mark_iv: f64_field(q, "iv").unwrap_or(f64::NAN),
                    mark_price: f64_field(q, "last_trade_price")
                        .or_else(|| f64_field(q, "theo"))
                        .unwrap_or(f64::NAN),
                    underlying_price: spot,
                    open_interest: f64_field(q, "open_interest").unwrap_or(f64::NAN),
                    greeks,
                },
            ));
            // Session volume → one aggregate OptionTrade per contract.
            // Direction-by-kind convention (module docs): call ⇒ Buy,
            // put ⇒ Sell. Zero volume emits nothing.
            if let Some(volume) = f64_field(q, "volume") {
                if volume.is_finite() && volume > 0.0 {
                    let price = f64_field(q, "last_trade_price").unwrap_or(f64::NAN);
                    if price.is_finite() && price > 0.0 {
                        seq += 1;
                        let side = match leg.kind {
                            OptionKind::Call => Side::Buy,
                            OptionKind::Put => Side::Sell,
                        };
                        out.push(EventEnvelope::new(
                            Venue::Cboe,
                            symbol,
                            recv_ts_ns,
                            recv_ts_ns,
                            seq,
                            MarketEvent::OptionTrade {
                                leg,
                                price,
                                qty: volume,
                                side,
                                trade_id: crate::json::hash_str(&sym),
                            },
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// IBIT poller configuration (IBI-7). TOML with `deny_unknown_fields` —
/// a typo is a startup error, never a silently-ignored option.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IbitConfig {
    /// Event-log + raw-capture root (default `data`, matching the other
    /// collectors).
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// CBOE delayed-quotes endpoint for the chain snapshot (IBI-1).
    #[serde(default = "default_url")]
    pub url: String,
    /// Poll cadence in seconds (default 900s = 15 min, the tape's own lag).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_s: u64,
    /// Underlying recorded on every leg (default "IBIT"; OCC root must match).
    #[serde(default = "default_underlying")]
    pub underlying: String,
    /// US equity contract multiplier (IBI-2; default 100).
    #[serde(default = "default_multiplier")]
    pub contract_multiplier: f64,
    /// Block-trade threshold in USD notional (spec 040 table; default $500k).
    #[serde(default = "default_block_threshold")]
    pub block_threshold_usd: f64,
    /// US market hours in ET, `HH:MM` (documented for cross-market alignment;
    /// IBI-6). The poller does not gate on them — the tape itself is the
    /// gate — but the values MUST be explicit in config.
    #[serde(default = "default_market_hours")]
    pub market_hours: MarketHours,
    /// Verbatim raw-frame capture to `{data_dir}/raw/cboe/{date}/` (COL-9).
    #[serde(default = "default_true")]
    pub raw_capture: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketHours {
    /// Session open, ET `HH:MM` (default "09:30").
    #[serde(default = "default_open_et")]
    pub open_et: String,
    /// Session close, ET `HH:MM` (default "16:00").
    #[serde(default = "default_close_et")]
    pub close_et: String,
}

fn default_data_dir() -> String {
    "data".to_owned()
}
fn default_url() -> String {
    DEFAULT_CBOE_URL.to_owned()
}
fn default_poll_interval() -> u64 {
    900
}
fn default_underlying() -> String {
    "IBIT".to_owned()
}
fn default_multiplier() -> f64 {
    IBIT_CONTRACT_MULTIPLIER
}
fn default_block_threshold() -> f64 {
    500_000.0
}
fn default_true() -> bool {
    true
}
fn default_open_et() -> String {
    "09:30".to_owned()
}
fn default_close_et() -> String {
    "16:00".to_owned()
}
fn default_market_hours() -> MarketHours {
    MarketHours::default()
}
impl Default for MarketHours {
    fn default() -> Self {
        MarketHours {
            open_et: default_open_et(),
            close_et: default_close_et(),
        }
    }
}

impl Default for IbitConfig {
    fn default() -> Self {
        IbitConfig {
            data_dir: default_data_dir(),
            url: default_url(),
            poll_interval_s: default_poll_interval(),
            underlying: default_underlying(),
            contract_multiplier: default_multiplier(),
            block_threshold_usd: default_block_threshold(),
            market_hours: MarketHours::default(),
            raw_capture: true,
        }
    }
}

/// Parse + validate the TOML config (CONV-16/IBI-7).
pub fn parse_config(text: &str) -> Result<IbitConfig, String> {
    let cfg: IbitConfig = toml::from_str(text).map_err(|e| format!("ibit config: {e}"))?;
    if !cfg.underlying.chars().all(|c| c.is_ascii_uppercase()) || cfg.underlying.is_empty() {
        return Err(format!(
            "ibit config: underlying must be an upper-case OCC root, got {:?}",
            cfg.underlying
        ));
    }
    if !cfg.contract_multiplier.is_finite() || cfg.contract_multiplier <= 0.0 {
        return Err("ibit config: contract_multiplier must be finite > 0".into());
    }
    if cfg.poll_interval_s == 0 {
        return Err("ibit config: poll_interval_s must be > 0".into());
    }
    Ok(cfg)
}

/// Parse-canary verdict for one poll (IBI-1, review decision): a response
/// whose `quotes` array is NON-EMPTY but yields ZERO parsed contracts is
/// schema drift — the caller MUST emit a Status event, never a clean empty
/// recording. `quotes_present=false` (heartbeat-only / non-chain payload) is
/// NOT a canary failure.
pub fn parse_canary(quotes_present: bool, parsed_contracts: usize) -> bool {
    quotes_present && parsed_contracts == 0
}

/// Whether a raw chain payload carries a `quotes` array at all (cheap probe
/// used by the canary before normalization).
pub fn quotes_present(payload: &[u8]) -> bool {
    serde_json::from_slice::<Value>(payload)
        .ok()
        .map(|v| {
            v.pointer("/data/options")
                .and_then(|q| q.as_array())
                .is_some()
                || v.pointer("/data/quotes")
                    .and_then(|q| q.as_array())
                    .is_some()
                || v.get("data").and_then(|d| d.as_array()).is_some()
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{parse_canary, parse_config, parse_occ_symbol};
    use crate::normalize::Normalizer;
    use mp_core::OptionKind;

    const FIXTURE: &str = r#"{
        "data": {
            "current_price": 51.2,
            "options": [
                {
                    "option": "IBIT260918C00045000",
                    "bid": 6.9, "ask": 7.1,
                    "iv": 0.62, "open_interest": 1234, "volume": 210,
                    "last_trade_price": 7.05,
                    "delta": 0.55, "gamma": 0.03, "theta": -0.02, "vega": 4.1
                },
                {
                    "option": "IBIT260918P00045000",
                    "bid": 1.1, "ask": 1.3,
                    "iv": 0.71, "open_interest": 890, "volume": 0,
                    "delta": -0.35
                }
            ]
        }
    }"#;

    #[test]
    fn ibi_1_occ_symbol_parses_leg() {
        let leg = parse_occ_symbol("IBIT260918C00045000").unwrap();
        assert_eq!(leg.underlying, "IBIT");
        assert_eq!(leg.strike, 45.0); // 45000/1000
        assert_eq!(leg.kind, OptionKind::Call);
        // 2026-09-18T00:00:00Z (20,714 days after the epoch).
        assert_eq!(leg.expiry_ts_ns, 1_789_689_600_000_000_000);
    }

    #[test]
    fn ibi_1_chain_snapshot_normalizes_to_events() {
        let mut n = super::CboeChainNormalizer::new();
        let mut out = Vec::new();
        n.normalize(1_000, FIXTURE.as_bytes(), &mut out).unwrap();
        // 2 contracts → 2 tickers; only the call has volume → 1 trade.
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|e| e.venue == mp_core::Venue::Cboe));
        let tick = &out[0].body;
        let mp_core::MarketEvent::OptionTicker {
            leg,
            mark_iv,
            open_interest,
            underlying_price,
            ..
        } = tick
        else {
            panic!("expected ticker");
        };
        assert_eq!(leg.underlying, "IBIT");
        assert!((leg.strike - 45.0).abs() < 1e-12);
        assert!((mark_iv - 0.62).abs() < 1e-12);
        assert!((open_interest - 1234.0).abs() < 1e-12);
        assert!((underlying_price - 51.2).abs() < 1e-12);
        let tr = &out[1].body; // [call-ticker, call-trade, put-ticker]
        let mp_core::MarketEvent::OptionTrade {
            price, qty, side, ..
        } = tr
        else {
            panic!("expected trade");
        };
        assert!((price - 7.05).abs() < 1e-12);
        assert!((qty - 210.0).abs() < 1e-12);
        assert_eq!(*side, mp_core::Side::Buy); // direction-by-kind convention
                                               // The zero-volume put produced NO trade event.
        assert!(matches!(
            &out[2].body,
            mp_core::MarketEvent::OptionTicker { .. }
        ));
    }

    #[test]
    fn ibi_2_symbol_meta_multiplier_is_100() {
        let mut n = super::CboeChainNormalizer::new();
        let mut out = Vec::new();
        n.normalize(1_000, FIXTURE.as_bytes(), &mut out).unwrap();
        let sym = out[0].symbol;
        let meta = n.symbols().get(sym).unwrap();
        assert_eq!(meta.contract_multiplier, 100.0);
        assert_eq!(meta.venue, mp_core::Venue::Cboe);
    }

    #[test]
    fn ibi_1_parse_canary_flags_zero_parsed_from_nonempty_quotes() {
        assert!(parse_canary(true, 0));
        assert!(!parse_canary(true, 5));
        assert!(!parse_canary(false, 0)); // heartbeat-only poll is fine
        assert!(!super::quotes_present(b"{}"));
    }

    #[test]
    fn ibi_7_check_config_rejects_unknown_fields() {
        let res =
            parse_config("data_dir = \"data\"\nunderlying = \"IBIT\"\npoll_intervall_s = 60\n");
        assert!(res.is_err()); // typo'd key → reject (deny_unknown_fields)
    }

    #[test]
    fn ibi_7_config_defaults_and_validation() {
        let cfg = parse_config("underlying = \"IBIT\"").unwrap();
        assert_eq!(cfg.contract_multiplier, 100.0);
        assert_eq!(cfg.block_threshold_usd, 500_000.0);
        assert_eq!(cfg.market_hours.open_et, "09:30");
        assert_eq!(cfg.market_hours.close_et, "16:00");
        assert!(parse_config("underlying = \"ibit\"").is_err());
        assert!(parse_config("poll_interval_s = 0").is_err());
    }

    #[test]
    fn ibi_8_malformed_payloads_are_counted_not_fatal() {
        let mut n = super::CboeChainNormalizer::new();
        let mut out = Vec::new();
        // Broken JSON is a counted parse error (COL-6), not a panic.
        assert!(n.normalize(1, b"{not json", &mut out).is_err());
        // A non-chain JSON object is ignored (heartbeat-only poll).
        assert!(n.normalize(1, b"{\"other\":1}", &mut out).is_ok());
        assert!(out.is_empty());
    }
}
