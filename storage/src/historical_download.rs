//! Live Binance-archive historical download (spec 027 HBS-1/HBS-8). Gated
//! behind the `live-http` feature so the offline bootstrap core builds with no
//! network stack. Owner-approved 2026-08-05 (CLAUDE.md safety table: new
//! external egress host) — data.binance.vision is Binance's public, no-auth,
//! no-key archive.
//!
//! Transport contract with the core: this module's job is to turn
//! `(symbol, date)` into raw aggTrades CSV *text* (the
//! [`HistoricalSource`][crate::historical::HistoricalSource] contract). The
//! parse → canonical Trade → `cold/historical/` write pipeline stays in
//! `historical.rs` and stays network-free (HBS-6, CONV-3).
//!
//! Determinism (CONV-10/11): the rate limiter and the retry-backoff schedule
//! are pure functions of injected time / a counter — the only wall-clock read
//! sits at the thin HTTP boundary (`now_ns`), and verification is offline
//! against a local mock HTTP server (HBS-8, CONV-23).

use crate::historical::HistoricalConfig;
use crate::StorageError;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Token-slot rate limiter (HBS-8, default 1 req/s). Pure: `wait_ns(now_ns)`
/// decides the required wait from an injected clock, so the policy is
/// unit-testable without sleeping (CONV-11). A `rate_per_sec ≤ 0` (or
/// non-finite) disables throttling.
#[derive(Debug, Clone, Default)]
pub struct Throttle {
    min_interval_ns: u64,
    next_slot_ns: u64,
}

impl Throttle {
    pub fn new(rate_per_sec: f64) -> Self {
        let min_interval_ns = if rate_per_sec.is_finite() && rate_per_sec > 0.0 {
            (1_000_000_000.0_f64 / rate_per_sec) as u64
        } else {
            0
        };
        Self {
            min_interval_ns,
            next_slot_ns: 0,
        }
    }

    /// For a request beginning at `now_ns`: how many ns must elapse before we
    /// are allowed to send, and the slot advances by `min_interval_ns`. Returns
    /// 0 when the request is already on/after its slot (or throttling is off).
    pub fn wait_ns(&mut self, now_ns: u64) -> u64 {
        if self.min_interval_ns == 0 {
            return 0; // unlimited — don't even advance the slot bookkeeping
        }
        let slot = self.next_slot_ns.max(now_ns);
        self.next_slot_ns = slot.saturating_add(self.min_interval_ns);
        slot.saturating_sub(now_ns)
    }
}

/// Deterministic full-jitter exponential backoff for transient HTTP failures
/// (HBS-8, CONV-11). Ceiling for retry `attempt` is `min(base·2^attempt,
/// cap)`; the actual delay is uniform in `[0, ceiling]` seeded by an FNV-1a of
/// (constant, attempt) — no entropy, reproducible schedules (the same policy
/// with the same failure sequence yields the same sleep sequence).
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    max_retries: u32,
    base_ms: u64,
    cap_ms: u64,
}

impl RetryPolicy {
    /// `cap_ms` is clamped up to `base_ms` (a sub-base ceiling makes
    /// `base·2^0` impossible to honor).
    pub fn new(max_retries: u32, base_ms: u64, cap_ms: u64) -> Self {
        let base_ms = base_ms.max(1);
        let cap_ms = cap_ms.max(1).max(base_ms);
        Self {
            max_retries,
            base_ms,
            cap_ms,
        }
    }

    /// Delay before the (`attempt`+1)-th HTTP request; `attempt` starts at 0.
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        let ceiling = self
            .base_ms
            .checked_shl(attempt.min(30))
            .unwrap_or(u64::MAX)
            .min(self.cap_ms);
        let mut h = mp_core::fnv1a_absorb(mp_core::FNV1A_OFFSET, &0x4852_5338_u64.to_le_bytes());
        h = mp_core::fnv1a_absorb(h, &attempt.to_le_bytes());
        let span = (ceiling as u128) + 1;
        (h as u128 % span) as u64
    }

    /// Number of additional requests allowed after the first attempt.
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }
}

/// Wall-clock ns — the ONLY clock read on the live path. Everything before it
/// (URL, throttle schedule, backoff) is pure.
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Live source: downloads one day's aggTrades ZIP from data.binance.vision,
/// honors the per-request rate limiter (HBS-8), and retries transient HTTP
/// faults (5xx/transport) with the policy's backoff. Implements
/// [`HistoricalSource`][crate::historical::HistoricalSource] exactly like
/// `FileHistoricalSource`/`MockHistoricalSource`, so the whole downstream
/// pipeline is shared and byte-identical regardless of transport.
#[derive(Debug)]
pub struct BinanceVisionSource {
    client: reqwest::blocking::Client,
    base_url: String,
    data_class: String,
    retry: RetryPolicy,
    throttle: std::sync::Mutex<Throttle>,
}

impl BinanceVisionSource {
    pub fn new(cfg: &HistoricalConfig) -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "mp-bootstrap/",
                env!("CARGO_PKG_VERSION"),
                " (spec 027 historical bootstrap; offline research job)"
            ))
            .build()
            // SAFETY (CONV-13): reqwest's default TLS stack + no custom zones
            // constructs infallibly; the only failure modes require explicit
            // overrides we do not use.
            .expect("reqwest blocking client builds");
        Self {
            client,
            base_url: cfg.download_base_url.clone(),
            data_class: cfg.download_data_class.clone(),
            retry: RetryPolicy::new(
                cfg.download_max_retries,
                cfg.download_backoff_base_ms,
                cfg.download_backoff_cap_ms,
            ),
            throttle: std::sync::Mutex::new(Throttle::new(cfg.download_rate_per_sec)),
        }
    }

    /// Deterministic archive URL (HBS-1 layout):
    /// `{base}/data/{class}/daily/aggTrades/{sym}/{sym}-aggTrades-{date}.zip`.
    pub fn daily_url(&self, symbol: &str, date: &str) -> String {
        format!(
            "{}/data/{}/daily/aggTrades/{}/{}-aggTrades-{}.zip",
            self.base_url, self.data_class, symbol, symbol, date
        )
    }

    fn fetch_with_retry(&self, url: &str) -> Result<Vec<u8>, StorageError> {
        let mut last_err = StorageError::Refused(format!("no attempt made (url {url})"));
        for attempt in 0..=self.retry.max_retries() {
            if attempt > 0 {
                let ms = self.retry.delay_ms(attempt - 1);
                tracing::warn!(
                    attempt,
                    backoff_ms = ms,
                    kind = "hbs.http",
                    "transient failure — retrying after backoff (HBS-8)"
                );
                std::thread::sleep(Duration::from_millis(ms));
            }
            let wait_ns = self
                .throttle
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .wait_ns(now_ns());
            if wait_ns > 0 {
                std::thread::sleep(Duration::from_nanos(wait_ns));
            }
            let resp = match self.client.get(url).send() {
                Ok(r) => r,
                Err(e) => {
                    last_err = StorageError::Refused(format!("request failed: {e} (url {url})"));
                    continue;
                }
            };
            let status = resp.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                // The archive genuinely has no such (symbol, date). Retrying
                // cannot help — fail immediately, never fabricate (HBS-8).
                return Err(StorageError::Refused(format!(
                    "archive has no record for {url} (HTTP 404)"
                )));
            }
            if status.is_success() {
                return resp
                    .bytes()
                    .map(|b| b.to_vec())
                    .map_err(|e| StorageError::Refused(format!("read body: {e} (url {url})")));
            }
            last_err = StorageError::Refused(format!("HTTP {status} for {url}"));
        }
        Err(last_err)
    }
}

impl crate::historical::HistoricalSource for BinanceVisionSource {
    fn fetch_day(&self, symbol: &str, date: &str) -> Result<String, StorageError> {
        let url = self.daily_url(symbol, date);
        let bytes = self.fetch_with_retry(&url)?;
        unzip_single_csv(&bytes, symbol, date)
    }
}

/// Extract the single CSV from a data.binance.vision daily ZIP (HBS-1). The
/// archive's aggtrades daily zips contain exactly ONE `.csv`; any other count
/// is upstream drift and MUST fail loudly (CONV-8 fail-closed, CONV-15 — we
/// never guess which entry is "the data").
pub fn unzip_single_csv(
    zip_bytes: &[u8],
    symbol: &str,
    date: &str,
) -> Result<String, StorageError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| StorageError::Refused(format!("cannot open zip for {symbol} {date}: {e}")))?;
    let entries: Vec<String> = archive.file_names().map(str::to_owned).collect();
    let csvs: Vec<&String> = entries
        .iter()
        .filter(|n| n.to_ascii_lowercase().ends_with(".csv"))
        .collect();
    match csvs.len() {
        0 => Err(StorageError::Refused(format!(
            "zip for {symbol} {date} contains no CSV (entries: {entries:?})"
        ))),
        1 => {
            let name = csvs[0];
            let mut file = archive.by_name(name).map_err(|e| {
                StorageError::Refused(format!("read csv {name} for {symbol} {date}: {e}"))
            })?;
            let mut text = String::new();
            file.read_to_string(&mut text).map_err(|e| {
                StorageError::Refused(format!("read csv {name} for {symbol} {date}: {e}"))
            })?;
            Ok(text)
        }
        n => Err(StorageError::Refused(format!(
            "zip for {symbol} {date} has {n} CSVs (expected exactly 1): {csvs:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_rate_limits_deterministically() {
        // 1 req/s; slot schedule is pure over injected now.
        let mut t = Throttle::new(1.0);
        assert_eq!(t.wait_ns(0), 0, "first request is on-slot");
        assert_eq!(
            t.wait_ns(500_000_000),
            500_000_000,
            "second at t=0.5s waits the remainder of its 1s slot"
        );
        assert_eq!(
            t.wait_ns(1_200_000_000),
            800_000_000,
            "third at t=1.2s backs to its 2s slot"
        );
        // Rate limiter off ⇔ no wait, no advance.
        let mut off = Throttle::new(0.0);
        assert_eq!(off.wait_ns(0), 0);
        assert_eq!(off.wait_ns(u64::MAX), 0);
        let mut nan = Throttle::new(f64::NAN);
        assert_eq!(
            nan.wait_ns(123),
            0,
            "NaN rate must fail to unlimited, not panic"
        );
    }

    #[test]
    fn retry_backoff_is_deterministic_exponential_capped() {
        let p = RetryPolicy::new(4, 1000, 5000);
        // Same attempt ⇒ same delay (deterministic seed, CONV-11).
        let a0 = p.delay_ms(0);
        let a0b = p.delay_ms(0);
        assert_eq!(a0, a0b);
        assert!(a0 <= 1000, "attempt 0 within [0, base]");
        let a1 = p.delay_ms(1);
        assert!(a1 <= 2000, "attempt 1 within [0, base·2]");
        let a2 = p.delay_ms(2);
        assert!(a2 <= 4000);
        // Ceiling: base·2^n clamped at cap regardless of attempt.
        for n in [3u32, 4, 30] {
            assert!(p.delay_ms(n) <= 5000, "capped at cap");
        }
    }
}
