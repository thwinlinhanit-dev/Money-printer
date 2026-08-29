//! Coinalyze cross-exchange validation-series normalizer (spec 047, COZ-1..10).
//! Pulls aggregated derivatives data (cross-exchange OI, funding, long/short
//! ratio, liquidations) from `api.coinalyze.net/v1` and emits
//! [`MarketEvent::MacroPoint`] events enveloped with [`Venue::Coinalyze`] —
//! **validation/context grade, never alpha** (COZ-9; predicted-funding is
//! public telegraphy) and NOT a long intraday history source
//! (COZ-10; retention is intraday-limited).
//!
//! ## Payload wrapping
//! The poller fetches per-symbol series and flattens them into one deterministic
//! wrapper the normalizer consumes: `{"series": [{"series_id":"AGG_OI_BTC",
//! "value": 2.4e10, "date": "2026-08-26"}, …]}`. Series ids follow the spec 047
//! convention: `AGG_OI_{SYM}`, `AGG_FUNDING_{SYM}`, `AGG_LS_{SYM}`,
//! `AGG_LIQ_{SYM}` for SYM in `{BTC, ETH}`.
//!
//! Key from env `COINALYZE_API_KEY` only (COZ-1, PD-2). External host approved
//! (spec 047 Decisions 2026-08-26).

use crate::json::{f64_field, str_field};
use crate::normalize::{NormError, Normalizer};
use mp_core::{EventEnvelope, MarketEvent, SymbolId, SymbolTable, Venue};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct CoinalyzeNormalizer {
    symbols: SymbolTable,
    next_seq: u64,
}

impl CoinalyzeNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn sym(&mut self, s: &str) -> SymbolId {
        self.symbols.intern_default(Venue::Coinalyze, s)
    }
}

impl Normalizer for CoinalyzeNormalizer {
    fn venue(&self) -> Venue {
        Venue::Coinalyze
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
        // One snapshot observation per aggregate series. Canonical BTreeMap
        // ordering makes provider array ordering irrelevant (COZ-4).
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
            let Some(date) = crate::defillama::parse_date(date_s) else {
                continue;
            };
            let id = self.sym(series_id);
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::Coinalyze,
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
        // Hourly/daily series — nothing to reset.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(payload: &serde_json::Value) -> Vec<EventEnvelope> {
        let mut n = CoinalyzeNormalizer::new();
        let mut out = Vec::new();
        let payload = serde_json::to_vec(payload).expect("test fixture serializes");
        n.normalize(0, &payload, &mut out)
            .expect("fixture normalizes");
        out
    }

    #[test]
    fn coz_3_event_variant_roundtrips() {
        let v = serde_json::json!({ "series": [
            {"series_id":"AGG_OI_BTC","value":2.4e10,"date":"2026-08-26"},
            {"series_id":"AGG_FUNDING_ETH","value":0.000008,"date":"2026-08-26"}
        ]});
        let evs = norm(&v);
        assert_eq!(evs.len(), 2);
        // BTreeMap sorts alphabetically: AGG_FUNDING_ETH < AGG_OI_BTC.
        assert_eq!(evs[0].venue, Venue::Coinalyze);
        assert_eq!(evs[1].venue, Venue::Coinalyze);
        let ids: Vec<&str> = evs
            .iter()
            .filter_map(|ev| match &ev.body {
                MarketEvent::MacroPoint { series_id, .. } => Some(series_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["AGG_FUNDING_ETH", "AGG_OI_BTC"]);
        // Verify values round-trip.
        match &evs[1].body {
            MarketEvent::MacroPoint { value, .. } => {
                assert!((value - 2.4e10).abs() < 1.0);
            }
            _ => panic!("expected MacroPoint"),
        }
        // Deterministic: same input, same bytes-level equality.
        assert_eq!(evs, norm(&v));
    }

    #[test]
    fn coz_4_normalization_deterministic() {
        let a = serde_json::json!({ "series": [ {"series_id":"AGG_LS_BTC","value":0.8,"date":"2026-08-26"} ] });
        let b = serde_json::json!({ "series": [ {"series_id":"AGG_LS_BTC","value":0.8,"date":"2026-08-26"} ] });
        assert_eq!(norm(&a), norm(&b));
    }

    #[test]
    fn coz_4_normalization_has_canonical_series_order() {
        let reversed = serde_json::json!({ "series": [
            {"series_id":"AGG_OI_BTC","value":2.4e10,"date":"2026-08-26"},
            {"series_id":"AGG_FUNDING_ETH","value":0.000008,"date":"2026-08-26"}
        ]});
        let ids: Vec<String> = norm(&reversed)
            .into_iter()
            .filter_map(|event| match event.body {
                MarketEvent::MacroPoint { series_id, .. } => Some(series_id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["AGG_FUNDING_ETH", "AGG_OI_BTC"]);
    }

    #[test]
    fn coz_8_recorded_fixture_normalizes_without_key_or_network() {
        let fixture: Value =
            serde_json::from_str(include_str!("../testdata/coinalyze_snapshot.json"))
                .expect("recorded fixture is valid JSON");
        let events = norm(&fixture);
        let ids: Vec<String> = events
            .into_iter()
            .filter_map(|event| match event.body {
                MarketEvent::MacroPoint { series_id, .. } => Some(series_id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["AGG_FUNDING_ETH", "AGG_LIQ_BTC", "AGG_OI_BTC"]);
    }

    #[test]
    fn coz_9_skips_nonfinite_keeps_valid() {
        // Non-finite values must be dropped (no fabricate); valid rows kept.
        let v = serde_json::json!({ "series": [
            {"series_id":"AGG_OI_BTC","value":null,"date":"2026-08-26"},
            {"series_id":"AGG_LIQ_BTC","value":1.5e6,"date":"2026-08-26"}
        ]});
        let evs = norm(&v);
        assert_eq!(evs.len(), 1);
        match &evs[0].body {
            MarketEvent::MacroPoint { series_id, .. } => assert_eq!(series_id, "AGG_LIQ_BTC"),
            _ => panic!("expected MacroPoint"),
        }
    }

    #[test]
    fn coz_5_empty_history_fabricates_nothing() {
        // COZ-5/COZ-8: an empty history array must produce zero events —
        // the poller must not fabricate a value from nothing.
        let v = serde_json::json!({ "series": [
            {"series_id":"AGG_OI_BTC","value":null,"date":"2026-08-26"},
            {"series_id":"AGG_LIQ_ETH","value":null,"date":"2026-08-26"}
        ]});
        let evs = norm(&v);
        assert!(evs.is_empty(), "null values must not fabricate events");
    }

    #[test]
    fn coz_6_duplicate_series_id_first_wins() {
        // COZ-4 deterministic dedup: duplicate series_id entries → first wins
        // (BTreeMap .or_insert). Same input, same output.
        let v = serde_json::json!({ "series": [
            {"series_id":"AGG_OI_BTC","value":2.4e10,"date":"2026-08-26"},
            {"series_id":"AGG_OI_BTC","value":9.9e10,"date":"2026-08-27"}
        ]});
        let evs = norm(&v);
        assert_eq!(evs.len(), 1);
        match &evs[0].body {
            MarketEvent::MacroPoint { value, .. } => assert!((value - 2.4e10).abs() < 1.0),
            _ => panic!("expected MacroPoint"),
        }
        assert_eq!(evs, norm(&v));
    }

    #[test]
    fn coz_malformed_root_is_error() {
        let mut n = CoinalyzeNormalizer::new();
        let mut out = Vec::new();
        assert!(n.normalize(0, b"{\"nope\":true}", &mut out).is_err());
    }
}

#[cfg(feature = "live-http")]
pub mod rest {
    //! Blocking Coinalyze poller (COZ-1..5). Key from env `COINALYZE_API_KEY`
    //! only (PD-2). Flattens per-symbol series into the wrapped snapshot the
    //! normalizer reads. Behind `live-http`.

    use serde_json::{json, Value};
    use std::time::{Duration, Instant};

    pub const BASE_URL: &str = "https://api.coinalyze.net/v1";

    /// Read `COINALYZE_API_KEY` from the environment (absent/empty is an error —
    /// the collector must not silently run keyless).
    pub fn api_key_from_env() -> Result<String, String> {
        match std::env::var("COINALYZE_API_KEY") {
            Ok(k) if !k.is_empty() => Ok(k),
            _ => Err("COINALYZE_API_KEY env var is not set (get a free key from \
                 coinalyze.net, then export it)"
                .into()),
        }
    }

    fn get_json(path: &str, api_key: &str) -> Result<Value, String> {
        let url = format!("{BASE_URL}{path}");
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .get(&url)
            .header("api_key", api_key)
            .send()
            .map_err(|e| format!("{url}: {e}"))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| format!("{url}: {e}"))?;
        if !status.is_success() {
            return Err(format!("{url} HTTP {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("{url}: json: {e}"))
    }

    /// Unix seconds → hyphenated UTC date (`YYYY-MM-DD`) for a datapoint.
    pub fn date_of_unix_s(t: i64) -> String {
        let days = t.div_euclid(86_400);
        let mut y = 1970i64;
        let mut rem = days;
        loop {
            let dy = if is_leap(y) { 366 } else { 365 };
            if rem < dy {
                break;
            }
            rem -= dy;
            y += 1;
        }
        let months = [
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
        let mut m = 0i64;
        while m < 12 && rem >= months[m as usize] {
            rem -= months[m as usize];
            m += 1;
        }
        format!("{y:04}-{:02}-{:02}", m + 1, rem + 1)
    }

    fn is_leap(y: i64) -> bool {
        (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
    }

    /// Extract the numeric payload of one Coinalyze datapoint object.
    ///
    /// Datapoints are `{ "t": <unix s>, "<letter>": <number> }` where the
    /// payload letter varies per endpoint (e.g. `"o"` for open interest).
    /// Rather than hard-code letters per endpoint, take the FIRST numeric
    /// field in sorted-key order excluding `"t"` — deterministic, and
    /// verifies trivially at deployment against the live docs (venue schema
    /// drift is pitfall #2). Returns `None` if no numeric payload exists.
    fn datapoint_value(p: &Value, value_field: &str) -> Option<f64> {
        if value_field == "l+s" {
            let long = crate::json::f64_field(p, "l")?;
            let short = crate::json::f64_field(p, "s")?;
            return (long + short).is_finite().then_some(long + short);
        }
        if !value_field.is_empty() {
            return crate::json::f64_field(p, value_field).filter(|v| v.is_finite());
        }
        let obj = p.as_object()?;
        let mut keys: Vec<&str> = obj
            .keys()
            .map(|s| s.as_str())
            .filter(|k| *k != "t")
            .collect();
        keys.sort_unstable();
        for k in keys {
            if let Some(f) = crate::json::f64_field(p, k) {
                return Some(f);
            }
        }
        None
    }

    /// Request pacing independent of endpoint shape. The collector keeps one
    /// instance across every symbol and pull, so a two-symbol poll never
    /// bursts above the configured budget (COZ-5).
    pub struct RequestPacer {
        interval: Duration,
        last_request: Option<Instant>,
    }

    impl RequestPacer {
        pub fn new(interval: Duration) -> Self {
            Self {
                interval,
                last_request: None,
            }
        }

        fn wait(&mut self) {
            if let Some(last) = self.last_request {
                let remaining = self.interval.saturating_sub(last.elapsed());
                if !remaining.is_zero() {
                    std::thread::sleep(remaining);
                }
            }
            self.last_request = Some(Instant::now());
        }
    }

    /// Fetch one history endpoint for a set of symbols and flatten the LAST
    /// datapoint of each symbol's series into wrapper rows.
    ///
    /// `extra_query` is appended raw to the URL (e.g. `&interval=hourly`),
    /// config-driven so cadence knobs stay out of code. `field` selects the
    /// history array inside each per-symbol object (`"open_interest"`,
    /// `"funding_rate"`, …).
    pub fn fetch_last_datapoints_blocking(
        api_key: &str,
        endpoint: &str,
        field: &str,
        value_field: &str,
        series_prefix: &str,
        pairs: &[(&str, &str)],
        extra_query: &str,
        pacer: &mut RequestPacer,
    ) -> Result<Value, String> {
        let mut list: Vec<Value> = Vec::new();
        let to = crate::binutil::now_ns().div_euclid(1_000_000_000);
        let from = to.saturating_sub(172_800);
        for (sym, suffix) in pairs {
            pacer.wait();
            let path = format!("{endpoint}?symbols={sym}{extra_query}&from={from}&to={to}");
            let v = get_json(&path, api_key)?;
            let Some(arr) = v.as_array() else {
                continue;
            };
            for obj in arr {
                // Match the requested symbol (responses echo it).
                if crate::json::str_field(obj, "symbol") != Some(sym) {
                    continue;
                }
                let Some(hist) = obj.get(field).and_then(Value::as_array) else {
                    continue;
                };
                let Some(last) = hist.last() else {
                    continue; // empty history — nothing honest to record
                };
                let Some(value) = datapoint_value(last, value_field) else {
                    continue;
                };
                if !value.is_finite() {
                    continue; // CONV-8
                }
                // Datapoint timestamp → its own date (no wall-clock mixing).
                let date = last
                    .get("t")
                    .and_then(Value::as_i64)
                    .map(date_of_unix_s)
                    .unwrap_or_else(today_date);
                list.push(json!({
                    "series_id": format!("{series_prefix}_{suffix}"),
                    "value": value,
                    "date": date,
                }));
            }
        }
        Ok(json!({ "series": list }))
    }

    /// Hyphenated UTC today — fallback when a datapoint lacks `"t"`.
    fn today_date() -> String {
        let ymd = crate::binutil::utc_date_str();
        format!("{}-{}-{}", &ymd[0..4], &ymd[4..6], &ymd[6..8])
    }

    #[test]
    fn coz_rest_datapoint_extraction_deterministic() {
        use serde_json::json;
        // Payload letter ignored; first sorted numeric key wins; "t" excluded.
        let p = json!({ "t": 1787721600i64, "o": 2.4e10, "c": 2.3e10 });
        assert_eq!(datapoint_value(&p, "c"), Some(2.3e10));
        assert_eq!(
            datapoint_value(&json!({ "t": 1, "l": 2.0, "s": 3.0 }), "l+s"),
            Some(5.0)
        );
        assert_eq!(date_of_unix_s(1787721600), "2026-08-26");
    }

    #[test]
    fn coz_1_key_from_env_not_repo() {
        // PD-2: key read from env only. Absent/empty env must error, never
        // silently run keyless; nothing here reads config/repo.
        std::env::remove_var("COINALYZE_API_KEY");
        assert!(api_key_from_env().is_err());
        // Single-threaded assertion of the env contract (PD-2).
        std::env::set_var("COINALYZE_API_KEY", "test-key");
        assert_eq!(api_key_from_env().unwrap(), "test-key");
        std::env::remove_var("COINALYZE_API_KEY");
    }

    #[test]
    fn coz_5_request_pacer_respects_interval() {
        // COZ-5: the pacer must sleep for the configured interval between
        // requests. A pacer with a 0ms interval never blocks (trivial case);
        // a pacer with a >0 interval blocks on the second call.
        use std::time::Duration;
        let mut pacer_zero = RequestPacer::new(Duration::ZERO);
        let start = Instant::now();
        pacer_zero.wait();
        pacer_zero.wait();
        assert!(start.elapsed() < Duration::from_millis(50));

        // Non-zero interval: second call must block.
        let mut pacer_slow = RequestPacer::new(Duration::from_millis(100));
        pacer_slow.wait();
        let before = Instant::now();
        pacer_slow.wait();
        let elapsed = before.elapsed();
        assert!(
            elapsed >= Duration::from_millis(80),
            "pacer should have blocked ~100ms, elapsed {elapsed:?}"
        );
    }
}
