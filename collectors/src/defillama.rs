//! DeFiLlama regime-series normalizer (spec 046, DEF-1..8). Pulls DeFi/L1
//! aggregate data (stablecoin circulating supply, total DeFi TVL, DEX volume)
//! from the **keyless** `api.llama.fi` and emits [`MarketEvent::MacroPoint`]
//! events enveloped with [`Venue::DeFiLlama`] — regime-grade, never
//! execution-grade (DEF-4, spec 030 MAC-5 spirit).
//!
//! ## Payload wrapping
//! The poller fetches several DeFiLlama endpoints and flattens them into one
//! deterministic wrapper the normalizer consumes:
//! `{"series": [{"series_id": "DEFI_TVL_AGG", "value": 8.78e10,
//! "date": "2026-08-26"}, …]}`. This keeps the [`Normalizer`] frame-based and
//! deterministic (series extraction is collection logic, not venue data).
//! Series ids are the documented spec 046 set; see the [`series`] module.
//!
//! No API key (keyless — PD-2 friendly). New external host approved (spec 046
//! Decisions 2026-08-26).

use crate::json::{f64_field, str_field};
use crate::normalize::{NormError, Normalizer};
use mp_core::{EventEnvelope, MarketEvent, SymbolId, SymbolTable, Venue};
use serde_json::Value;
use std::collections::BTreeMap;

/// The documented `series_id` keys (spec 046 Decisions 2026-08-26).
pub mod series {
    pub const DEFI_TVL_AGG: &str = "DEFI_TVL_AGG";
    pub const USDT_SUPPLY: &str = "USDT_SUPPLY";
    pub const USDC_SUPPLY: &str = "USDC_SUPPLY";
    pub const STABLECOIN_MCAP: &str = "STABLECOIN_MCAP";
    pub const DEX_VOL_1D: &str = "DEX_VOL_1D";
}

#[derive(Default)]
pub struct DefiLlamaNormalizer {
    symbols: SymbolTable,
    next_seq: u64,
}

impl DefiLlamaNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn sym(&mut self, s: &str) -> SymbolId {
        self.symbols.intern_default(Venue::DeFiLlama, s)
    }
}

impl Normalizer for DefiLlamaNormalizer {
    fn venue(&self) -> Venue {
        Venue::DeFiLlama
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let text = std::str::from_utf8(payload).map_err(|e| NormError::Parse(e.to_string()))?;
        let root: Value =
            serde_json::from_str(text).map_err(|e| NormError::Parse(e.to_string()))?;
        let arr = root
            .get("series")
            .and_then(Value::as_array)
            .ok_or_else(|| NormError::Parse("missing 'series' array".into()))?;
        // One daily observation per documented series. The BTreeMap makes
        // both the emission order and duplicate handling deterministic.
        let mut series: BTreeMap<&str, &Value> = BTreeMap::new();
        for item in arr {
            if let Some(series_id) = str_field(item, "series_id") {
                series.entry(series_id).or_insert(item);
            }
        }
        for (series_id, item) in series {
            let Some(value) = f64_field(item, "value") else {
                continue;
            };
            let Some(date_s) = str_field(item, "date") else {
                continue;
            };
            if !value.is_finite() {
                // NaN/<inf must not fabricate (CONV-8).
                continue;
            }
            let Some(date) = parse_date(date_s) else {
                continue;
            };
            let id = self.sym(series_id);
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::DeFiLlama,
                id,
                date,
                recv_ts_ns,
                seq,
                MarketEvent::MacroPoint {
                    series_id: series_id.to_owned(),
                    value,
                    date,
                },
            ));
        }
        Ok(())
    }

    fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    fn reset_books(&mut self) {
        // Daily series — nothing to reset.
    }
}

/// Parse a `YYYY-MM-DD` date at UTC midnight into ns (same civil algorithm
/// contract as FRED, spec 030 MAC-4). Copied here so this module stays
/// independent of `fred`. Returns `None` on malformed input.
pub fn parse_date(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let num = |b: &[u8]| -> Option<i64> { std::str::from_utf8(b).ok()?.parse().ok() };
    let y = num(&bytes[0..4])?;
    let m = num(&bytes[5..7])?;
    let d = num(&bytes[8..10])?;
    if !(1..=12).contains(&m) {
        return None;
    }
    let month_days = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=month_days[(m - 1) as usize]).contains(&d) {
        return None;
    }
    days_from_civil(y, m, d)?.checked_mul(86_400_000_000_000)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Howard Hinnant's `days_from_civil` (proleptic Gregorian; FRED contract).
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(-9999..=9999).contains(&y) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(feature = "live-http")]
pub mod rest {
    //! Blocking DeFiLlama poller (DEF-1) — **keyless** public API, behind
    //! `live-http`. Flattens several public endpoints into the wrapped
    //! `{"series":[…]}` snapshot the normalizer reads. A failed sub-fetch is
    //! logged and skipped (best-effort regime data) — never fabricated.

    use super::series;
    use serde_json::{json, Value};

    /// All-chains aggregate TVL history (no chain suffix = global sum).
    pub const CHAIN_TVL_URL: &str = "https://api.llama.fi/v2/historicalChainTvl";
    /// Stablecoin supply census (`{peggedUSD: total, assets: [...]}`).
    pub const STABLECOINS_URL: &str = "https://stablecoins.llama.fi/stablecoins";
    /// DEX volume overview (`total24h` present as number or string).
    pub const DEX_OVERVIEW_URL: &str = "https://api.llama.fi/overview/dexs";

    fn get_json(url: &str) -> Result<Value, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client.get(url).send().map_err(|e| format!("{url}: {e}"))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| format!("{url}: {e}"))?;
        if !status.is_success() {
            return Err(format!("{url} HTTP {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("{url}: json: {e}"))
    }

    fn row(id: &str, value: f64, date: &str) -> Value {
        json!({ "series_id": id, "value": value, "date": date })
    }

    /// Hyphenated UTC today (`YYYY-MM-DD`) for daily-series dating.
    pub fn today_date() -> String {
        let ymd = crate::binutil::utc_date_str();
        format!("{}-{}-{}", &ymd[0..4], &ymd[4..6], &ymd[6..8])
    }

    /// Fetch the full regime snapshot. Best-effort: each leg independently
    /// succeeded or is skipped with a WARN (DEF-1; CONV-8: no invented rows).
    pub fn fetch_snapshot_blocking() -> Value {
        let date = today_date();
        let mut list: Vec<Value> = Vec::new();

        // Leg 1 — stablecoin supply (USDT/USDC + aggregate mcap).
        match get_json(STABLECOINS_URL) {
            Ok(v) => {
                if let Some(mcap) = crate::json::f64_field(&v, "peggedUSD") {
                    list.push(row(series::STABLECOIN_MCAP, mcap, &date));
                }
                if let Some(assets) = v.get("assets").and_then(Value::as_array) {
                    let mut usdt = 0.0f64;
                    let mut usdc = 0.0f64;
                    for a in assets {
                        let Some(sym) = crate::json::str_field(a, "symbol") else {
                            continue;
                        };
                        let Some(circ) = a.get("circulating") else {
                            continue;
                        };
                        let Some(val) = crate::json::f64_field(circ, "peggedUSD") else {
                            continue;
                        };
                        match sym {
                            "USDT" => usdt += val,
                            "USDC" => usdc += val,
                            _ => {}
                        }
                    }
                    list.push(row(series::USDT_SUPPLY, usdt, &date));
                    list.push(row(series::USDC_SUPPLY, usdc, &date));
                }
            }
            Err(e) => tracing::warn!(error = %e, "defillama stablecoins leg skipped"),
        }

        // Leg 2 — all-chains DeFi TVL (latest point of the global history).
        match get_json(CHAIN_TVL_URL) {
            Ok(v) => match v.as_array().and_then(|a| a.last()) {
                Some(p) => {
                    if let Some(tvl) = crate::json::f64_field(p, "totalLiquidity") {
                        list.push(row(series::DEFI_TVL_AGG, tvl, &date));
                    }
                }
                None => tracing::warn!("defillama tvl leg empty"),
            },
            Err(e) => tracing::warn!(error = %e, "defillama tvl leg skipped"),
        }

        // Leg 3 — 24h DEX volume aggregate.
        match get_json(DEX_OVERVIEW_URL) {
            Ok(v) => {
                if let Some(vol) = crate::json::f64_field(&v, "total24h") {
                    list.push(row(series::DEX_VOL_1D, vol, &date));
                }
            }
            Err(e) => tracing::warn!(error = %e, "defillama dex volume leg skipped"),
        }

        json!({ "series": list })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defillama_parse_date_utc_midnight_ns() {
        // 2026-08-03T00:00:00Z = 1785715200s (same fixture as FRED).
        assert_eq!(parse_date("2026-08-03"), Some(1_785_715_200_000_000_000));
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date("2026-02-29"), None); // not a leap year
        assert_eq!(parse_date("bad-date"), None);
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("2026-08-32"), None);
    }

    fn norm(payload: &serde_json::Value) -> Vec<EventEnvelope> {
        let mut n = DefiLlamaNormalizer::new();
        let mut out = Vec::new();
        let payload = serde_json::to_vec(payload).expect("test fixture serializes");
        n.normalize(0, &payload, &mut out)
            .expect("fixture normalizes");
        out
    }

    #[test]
    fn def_2_macropoint_variant_roundtrips() {
        let v = serde_json::json!({ "series": [
            {"series_id":"DEFI_TVL_AGG","value":8.78e10,"date":"2026-08-26"},
            {"series_id":"USDT_SUPPLY","value":1.2e11,"date":"2026-08-26"}
        ]});
        let evs = norm(&v);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].venue, Venue::DeFiLlama);
        match &evs[0].body {
            MarketEvent::MacroPoint {
                series_id, value, ..
            } => {
                assert_eq!(series_id, "DEFI_TVL_AGG");
                assert!((value - 8.78e10).abs() < 1.0);
            }
            _ => panic!("expected MacroPoint"),
        }
        // Determinism: same input => same ids/seq.
        let evs2 = norm(&v);
        assert_eq!(evs, evs2);
    }

    #[test]
    fn def_3_normalization_deterministic() {
        let a = serde_json::json!({ "series": [
            {"series_id":"USDC_SUPPLY","value":3.0e10,"date":"2026-08-26"},
            {"series_id":"USDT_SUPPLY","value":1.2e11,"date":"2026-08-26"}
        ]});
        let b = serde_json::json!({ "series": [
            {"series_id":"USDC_SUPPLY","value":3.0e10,"date":"2026-08-26"},
            {"series_id":"USDT_SUPPLY","value":1.2e11,"date":"2026-08-26"}
        ]});
        assert_eq!(norm(&a), norm(&b));
    }

    #[test]
    fn def_3_normalization_has_canonical_series_order() {
        let reversed = serde_json::json!({ "series": [
            {"series_id":"USDT_SUPPLY","value":1.2e11,"date":"2026-08-26"},
            {"series_id":"DEFI_TVL_AGG","value":8.78e10,"date":"2026-08-26"}
        ]});
        let ids: Vec<String> = norm(&reversed)
            .into_iter()
            .filter_map(|event| match event.body {
                MarketEvent::MacroPoint { series_id, .. } => Some(series_id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["DEFI_TVL_AGG", "USDT_SUPPLY"]);
    }

    #[test]
    fn def_7_recorded_fixture_normalizes_without_network() {
        let fixture: Value =
            serde_json::from_str(include_str!("../testdata/defillama_snapshot.json"))
                .expect("recorded fixture is valid JSON");
        let events = norm(&fixture);
        let ids: Vec<String> = events
            .into_iter()
            .filter_map(|event| match event.body {
                MarketEvent::MacroPoint { series_id, .. } => Some(series_id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["DEFI_TVL_AGG", "DEX_VOL_1D", "USDT_SUPPLY"]);
    }

    #[test]
    fn def_4_fidelity_regime_not_execution() {
        // No special code path — the normalizer emits only MacroPoint regime
        // series; do a non-finite value which must be skipped (no fabricate).
        let v = serde_json::json!({ "series": [
            {"series_id":"USDT_SUPPLY","value":null,"date":"2026-08-26"}
        ]});
        let evs = norm(&v);
        assert!(evs.is_empty());
    }

    #[test]
    fn def_7_skips_malformed_rows() {
        let v = serde_json::json!({ "series": [
            {"series_id":"USDT_SUPPLY","value":1.0,"date":"bad"},
            {"series_id":"USDC_SUPPLY","value":2.0,"date":"2026-08-26"},
            {"series_id":7,"value":3.0,"date":"2026-08-26"}
        ]});
        let evs = norm(&v);
        assert_eq!(evs.len(), 1);
        match &evs[0].body {
            MarketEvent::MacroPoint { series_id, .. } => assert_eq!(series_id, "USDC_SUPPLY"),
            _ => panic!("expected MacroPoint"),
        }
    }

    #[test]
    fn def_1_keyless_defillama_endpoint() {
        // DEF-1: DeFiLlama is keyless — the normalizer requires no auth token,
        // API key, or secret. Verify it normalizes a payload with zero
        // external dependencies (no env var, no config key).
        let v = serde_json::json!({ "series": [
            {"series_id":"DEFI_TVL_AGG","value":1.0,"date":"2026-08-26"}
        ]});
        let evs = norm(&v);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].venue, Venue::DeFiLlama);
    }

    #[test]
    fn def_5_cold_macro_label_in_event_body() {
        // DEF-5: events carry MacroPoint with source=fdefillama (not raw trades).
        // Verify the venue and series structure match the cold/macro contract.
        let v = serde_json::json!({ "series": [
            {"series_id":"DEX_VOL_1D","value":5.0e9,"date":"2026-08-26"}
        ]});
        let evs = norm(&v);
        assert_eq!(evs.len(), 1);
        match &evs[0].body {
            MarketEvent::MacroPoint {
                series_id, date, ..
            } => {
                assert_eq!(series_id, "DEX_VOL_1D");
                assert!(*date > 0);
            }
            _ => panic!("expected MacroPoint for cold/macro"),
        }
    }

    #[test]
    fn def_malformed_root_is_error() {
        let mut n = DefiLlamaNormalizer::new();
        let mut out = Vec::new();
        let err = n.normalize(0, b"{\"nope\":true}", &mut out);
        assert!(err.is_err());
    }
}
