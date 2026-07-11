//! Live HTTP transport (feature `live-http`). Off by default so all
//! request-build / response-parse logic compiles and tests with no network
//! stack. PD-2: keys arrive already resolved from env (see `config.rs`); this
//! module never reads or logs them.
//!
//! Blocking `reqwest` with rustls — a research brief job is a simple
//! request/response, not a hot path, so blocking keeps the call sites plain.

use crate::error::LlmError;
use crate::provider::HttpRequest;

/// Send a built request and return the raw response body bytes. The caller
/// hands the bytes to the provider's `parse_response`. Non-2xx responses still
/// return their body (providers encode errors as JSON envelopes we surface).
pub fn send_blocking(req: &HttpRequest) -> Result<Vec<u8>, LlmError> {
    let client = reqwest::blocking::Client::new();
    let mut builder = client.post(&req.url).body(req.body.clone());
    for (k, v) in &req.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    let resp = builder
        .send()
        .map_err(|e| LlmError::Transport(e.to_string()))?;
    let bytes = resp
        .bytes()
        .map_err(|e| LlmError::Transport(e.to_string()))?;
    Ok(bytes.to_vec())
}
