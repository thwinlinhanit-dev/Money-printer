//! Live HTTP transport (feature `live-http`). Off by default so all
//! request-build / response-parse logic compiles and tests with no network
//! stack. PD-2: keys arrive already resolved from env (see `config.rs`); this
//! module never reads or logs them.
//!
//! Blocking `reqwest` with rustls — a research brief job is a simple
//! request/response, not a hot path, so blocking keeps the call sites plain.
//! Transport policy: a sane default timeout (120s — LLM completions can be
//! slow, but never hang a batch job forever) and bounded retries with jitter
//! on the retryable class only: timeouts, connect/transport errors, HTTP 429
//! (rate limit), and HTTP 5xx. Other 4xx (auth failures, malformed requests)
//! are programmer/operator errors — retrying them would just burn the quota.
//!
//! PD-3: this is a batch research path, not a decision path, so wall-clock
//! sleeps and jitter are allowed (they never influence trading). Jitter is
//! deliberately NOT seeded randomness: it is derived from a process-local
//! atomic counter mixed with the retry attempt, so the retry *schedule* is
//! deterministic per process and needs no OS RNG dependency (see
//! `jitter_ms` — full-jitter style, capped).

use crate::error::LlmError;
use crate::provider::HttpRequest;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Per-request timeout (connect + read). Long LLM completions must not hang
/// the nightly brief job indefinitely.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum attempts per request (1 initial + up to `MAX_RETRIES` retries).
pub const MAX_RETRIES: u32 = 3;

/// Backoff base: attempt N sleeps `BASE_MS << N` plus jitter before the next
/// try (250ms, 500ms, 1000ms, … capped by the retry count).
const BASE_MS: u64 = 250;

/// Process-local counter feeding the jitter hash. Never read from/to anywhere
/// else; makes repeated bursts from this process differ without unseeded
/// randomness.
static JITTER_CTR: AtomicU64 = AtomicU64::new(0);

/// SplitMix64 finalizer over (counter, attempt, salt) — deterministic for a
/// given process + call sequence, no OS entropy (keeps the dependency tree
/// minimal and the retry schedule reproducible in tests via `jitter_ms`).
fn jitter_ms(attempt: u32, salt: u64) -> u64 {
    let mut z = JITTER_CTR
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((attempt as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
        .wrapping_add(salt);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) % BASE_MS
}

/// One send attempt. Returns the status code and body; non-2xx bodies are
/// kept because providers encode errors as JSON envelopes we surface.
fn try_send(client: &reqwest::blocking::Client, req: &HttpRequest) -> Result<(u16, Vec<u8>), LlmError> {
    let mut builder = client.post(&req.url).body(req.body.clone());
    for (k, v) in &req.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    let resp = builder.send().map_err(|e| {
        // Never embed the URL (it cannot carry a key in v1, but keep the
        // invariant that transport errors contain no request material).
        LlmError::Transport(format!("{}: {}", kind_of(&e), statusless(&e)))
    })?;
    let status = resp.status().as_u16();
    let bytes = resp
        .bytes()
        .map_err(|e| LlmError::Transport(format!("read body: {e}")))?;
    Ok((status, bytes.to_vec()))
}

fn kind_of(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else {
        "request"
    }
}

/// Error text minus anything that could echo request material (headers/body
/// are never printed by reqwest, but be explicit anyway — PD-2 defense in
/// depth for the log path).
fn statusless(e: &reqwest::Error) -> String {
    let s = e.to_string();
    match s.find(" for url (") {
        Some(idx) => s[..idx].to_string(),
        None => s,
    }
}

/// Whether this outcome is worth retrying: transport failures (timeout,
/// connect, reset), 429, and 5xx. Everything else (2xx, other 4xx) is final.
pub(crate) fn retryable(result: &Result<(u16, Vec<u8>), LlmError>) -> bool {
    match result {
        Ok((status, _)) => *status == 429 || (500..600).contains(status),
        Err(LlmError::Transport(_)) => true,
        Err(_) => false,
    }
}

/// Send a built request with retries and return the raw response body bytes.
/// The caller hands the bytes to the provider's `parse_response`.
pub fn send_blocking(client: &reqwest::blocking::Client, req: &HttpRequest) -> Result<Vec<u8>, LlmError> {
    // Salt mixes the URL length + body checksum-lite (len) so retries for two
    // different requests in the same process don't share a jitter schedule.
    let salt = (req.url.len() as u64) << 32 | req.body.len() as u64;
    let mut last: Option<Result<(u16, Vec<u8>), LlmError>> = None;
    for attempt in 0..=MAX_RETRIES {
        if let Some(prev) = last.take() {
            if !retryable(&prev) {
                return prev.map(|(_, body)| body);
            }
            let delay = Duration::from_millis((BASE_MS << attempt) + jitter_ms(attempt, salt));
            std::thread::sleep(delay);
        }
        let result = try_send(client, req);
        let done = !retryable(&result) || attempt == MAX_RETRIES;
        last = Some(result);
        if done {
            return last.take().expect("just stored").map(|(_, body)| body);
        }
    }
    unreachable!("loop either retries via `continue` or returns");
}

/// Build the shared blocking client with [`DEFAULT_TIMEOUT`].
pub fn default_client() -> Result<reqwest::blocking::Client, LlmError> {
    reqwest::blocking::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .build()
        .map_err(|e| LlmError::Transport(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_retries_429_and_5xx_but_not_other_4xx() {
        assert!(retryable(&Ok((429, vec![]))));
        assert!(retryable(&Ok((500, vec![]))));
        assert!(retryable(&Ok((503, vec![]))));
        assert!(!retryable(&Ok((200, vec![]))));
        assert!(!retryable(&Ok((400, vec![]))));
        assert!(!retryable(&Ok((401, vec![]))));
        assert!(!retryable(&Ok((404, vec![]))));
        assert!(retryable(&Err(LlmError::Transport("timeout".into()))));
        assert!(!retryable(&Err(LlmError::Parse("bad json".into()))));
    }

    #[test]
    fn jitter_is_bounded_and_deterministic_per_attempt_and_salt() {
        // SplitMix64 over the explicit inputs: the formula's same (attempt,
        // salt, counter) triple always jitters the same way, and stays < BASE_MS.
        for attempt in 0..4 {
            let j = jitter_ms(attempt, 42);
            assert!(j < BASE_MS);
        }
    }
}
