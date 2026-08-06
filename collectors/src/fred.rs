//! FRED (St. Louis Fed) daily economic series normalizer (spec 030,
//! MAC-1..8). Polls `https://api.stlouisfed.org/fred/series/observations`
//! with `FRED_API_KEY` from the environment (MAC-2, PD-2: the key never
//! lives in the repo/config/`.example`/fixtures) and emits
//! [`MarketEvent::MacroPoint`] events — correlation-grade, not
//! execution-grade (MAC-5).
//!
//! ## Payload wrapping
//! The observations response does not echo the requested `series_id`, so the
//! poller wraps it before normalization: `{"series_id": "DGS10",
//! "observations": […], …}`. This keeps the [`Normalizer`] frame-based and
//! deterministic (series id is request state, not venue data).
//!
//! The HIP-3 half of spec 030 (MAC-1) has no new code: HIP-3 TradFi
//! symbols flow through the existing `collectors::hyperliquid` normalizer
//! with `InstrumentKind::TradFiSynthetic` metadata (see
//! `mp-collector --hip3-symbols`).

use crate::json::str_field;
use crate::normalize::{NormError, Normalizer};
use mp_core::{EventEnvelope, MarketEvent, SymbolId, SymbolTable, Venue};
use serde_json::Value;

/// FRED's sentinel for an observation that was not published (MAC-8: skip,
/// never invent a value).
pub const MISSING: &str = ".";

#[derive(Default)]
pub struct FredNormalizer {
    symbols: SymbolTable,
    next_seq: u64,
}

impl FredNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn sym(&mut self, s: &str) -> SymbolId {
        self.symbols.intern_default(Venue::Fred, s)
    }
}

/// Parse a FRED observation date `YYYY-MM-DD` into ns at UTC midnight
/// (MAC-4: deterministic; CONV-4: UTC ns). Returns `None` on malformed input.
pub fn parse_fred_date(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let num = |b: &[u8]| -> Option<i64> {
        let t = std::str::from_utf8(b).ok()?;
        t.parse().ok()
    };
    let y = num(&bytes[0..4])?;
    let m = num(&bytes[5..7])?;
    let d = num(&bytes[8..10])?;
    if !(1..=12).contains(&m) {
        return None;
    }
    // Day must be valid for the month (leap-aware) — a triple like
    // 2026-02-29 is not a real date and must not silently wrap into March.
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
    // Days since epoch of `y-m-d` at UTC midnight (civil → days algorithm).
    let days = days_from_civil(y, m, d)?;
    days.checked_mul(86_400_000_000_000)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Howard Hinnant's `days_from_civil` — exact for the proleptic Gregorian
/// calendar (FRED dates are Gregorian). Returns `None` for out-of-range
/// years (avoid overflow).
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

impl Normalizer for FredNormalizer {
    fn venue(&self) -> Venue {
        Venue::Fred
    }

    fn normalize(
        &mut self,
        recv_ts_ns: i64,
        payload: &[u8],
        out: &mut Vec<EventEnvelope>,
    ) -> Result<(), NormError> {
        let v: Value =
            serde_json::from_slice(payload).map_err(|e| NormError::Parse(e.to_string()))?;
        let series_id = str_field(&v, "series_id")
            .ok_or_else(|| NormError::Parse("wrapped FRED response lacks series_id".into()))?
            .to_owned();
        let observations = v
            .get("observations")
            .and_then(|o| o.as_array())
            .ok_or_else(|| NormError::Parse("wrapped FRED response lacks observations".into()))?;

        // FRED returns observations ascending by date — deterministic order.
        let id = self.sym(&series_id);
        for obs in observations {
            let Some(date_s) = str_field(obs, "date") else {
                continue;
            };
            let Some(value_s) = str_field(obs, "value") else {
                continue;
            };
            if value_s == MISSING {
                // Unpublished observation: skip honestly (MAC-8).
                continue;
            }
            let Some(value) = value_s.parse::<f64>().ok().filter(|v| v.is_finite()) else {
                continue;
            };
            let Some(date) = parse_fred_date(date_s) else {
                continue;
            };
            let seq = self.seq();
            out.push(EventEnvelope::new(
                Venue::Fred,
                id,
                date,
                recv_ts_ns,
                seq,
                MarketEvent::MacroPoint {
                    series_id: series_id.clone(),
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

#[cfg(feature = "live-http")]
pub mod rest {
    //! Blocking FRED REST poller (MAC-2). Behind `live-http`. The API key
    //! comes from the `FRED_API_KEY` env var only — never from config or the
    //! repo (PD-2, CONV-17; the key is a free public-data key).

    pub const FRED_OBSERVATIONS_URL: &str = "https://api.stlouisfed.org/fred/series/observations";

    /// Read `FRED_API_KEY` from the environment (absent/empty is an error —
    /// the collector must not silently run keyless).
    pub fn api_key_from_env() -> Result<String, String> {
        match std::env::var("FRED_API_KEY") {
            Ok(k) if !k.is_empty() => Ok(k),
            _ => Err("FRED_API_KEY env var is not set (get a free key from \
                 https://fred.stlouisfed.org/docs/api/api_key.html)"
                .into()),
        }
    }

    /// Fetch observations for one series since `start_date` (YYYY-MM-DD).
    /// Returns the venue response (unwrapped — see module docs).
    pub fn fetch_observations_blocking(
        series_id: &str,
        api_key: &str,
        start_date: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut url = format!(
            "{FRED_OBSERVATIONS_URL}?series_id={series_id}&api_key={api_key}&file_type=json"
        );
        if let Some(start) = start_date {
            url.push_str(&format!("&observation_start={start}"));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("fred request: {e}"))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| format!("fred body: {e}"))?;
        if !status.is_success() {
            return Err(format!("fred HTTP {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("fred json: {e}"))
    }
}
#[cfg(test)]
mod tests {
    use super::parse_fred_date;

    #[test]
    fn fred_date_parses_utc_midnight_ns() {
        // 2026-08-03T00:00:00Z = 1785715200s = 1785715200000000000 ns
        // (20668 days after the epoch, verified with days_from_civil).
        assert_eq!(
            parse_fred_date("2026-08-03"),
            Some(1_785_715_200_000_000_000)
        );
        assert_eq!(parse_fred_date("1970-01-01"), Some(0));
        assert_eq!(parse_fred_date("1969-12-31"), Some(-86_400_000_000_000));
        assert_eq!(parse_fred_date("2026-02-29"), None); // not a leap year
        assert_eq!(parse_fred_date("bad-date"), None);
        assert_eq!(parse_fred_date("2026-13-01"), None);
    }
}
