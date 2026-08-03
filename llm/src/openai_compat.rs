//! Shared request-build / response-parse for the OpenAI Chat Completions wire
//! shape. OpenAI, DeepSeek, Groq, xAI, Mistral, and Ollama all speak it, so
//! they share this code and differ only in URL / default model / key env.

use crate::error::LlmError;
use crate::provider::{ChatRequest, Completion, HttpRequest, LlmProvider, Usage};
use serde_json::{json, Value};

/// Read a non-negative integer at a JSON pointer. `None` (missing field) is
/// not an error (providers sometimes omit usage on error envelopes the caller
/// parses past); a *present* field that cannot convert to `u32` is — fail
/// closed with a descriptive variant instead of truncating (L4).
pub(crate) fn u32_at(v: &Value, ptr: &'static str) -> Result<u32, LlmError> {
    match v.pointer(ptr) {
        None => Ok(0),
        Some(n) => {
            let raw = n.as_u64().ok_or_else(|| LlmError::UsageOverflow {
                ptr,
                value: u64::MAX,
            })?;
            raw.try_into()
                .map_err(|_| LlmError::UsageOverflow { ptr, value: raw })
        }
    }
}

/// Build a `POST {base}` Chat Completions request with `Bearer` auth. When
/// `api_key` is empty (Ollama-local) the `Authorization` header is omitted.
pub fn build(
    p: &dyn LlmProvider,
    url: &str,
    api_key: &str,
    req: &ChatRequest,
) -> Result<HttpRequest, LlmError> {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| json!({"role": m.role.openai_str(), "content": m.content}))
        .collect();

    let body = json!({
        "model": p.model_for(req),
        "messages": messages,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    });

    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
    if !api_key.is_empty() {
        headers.push(("authorization".to_string(), format!("Bearer {api_key}")));
    }

    Ok(HttpRequest {
        url: url.to_string(),
        headers,
        body: serde_json::to_vec(&body).map_err(|e| LlmError::Parse(e.to_string()))?,
    })
}

/// Parse a Chat Completions response: `choices[0].message.content`, plus
/// `usage.{prompt,completion}_tokens` and `finish_reason`.
pub fn parse(body: &[u8]) -> Result<Completion, LlmError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| LlmError::Parse(e.to_string()))?;
    if let Some(err) = v.get("error") {
        return Err(LlmError::Provider(err.to_string()));
    }
    let choice = v
        .pointer("/choices/0")
        .ok_or_else(|| LlmError::Parse("no choices[0]".into()))?;
    let text = choice
        .pointer("/message/content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| LlmError::Parse("no message.content".into()))?
        .to_string();
    let stop_reason = choice
        .get("finish_reason")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    let model = v
        .get("model")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let usage = Usage {
        input_tokens: u32_at(&v, "/usage/prompt_tokens")?,
        output_tokens: u32_at(&v, "/usage/completion_tokens")?,
    };
    Ok(Completion {
        model,
        text,
        usage,
        stop_reason,
    })
}
