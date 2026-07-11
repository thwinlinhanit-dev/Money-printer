//! Anthropic Messages API provider (`POST /v1/messages`).
//!
//! Wire shape and headers per the Anthropic API: `x-api-key`,
//! `anthropic-version: 2023-06-01`, system prompt hoisted to the top-level
//! `system` field, and `content` returned as a list of typed blocks. Default
//! model is `claude-opus-4-8` (this repo's own model family).

use crate::error::LlmError;
use crate::provider::{ChatRequest, Completion, HttpRequest, LlmProvider, Provider, Usage};
use serde_json::{json, Value};

/// Anthropic Messages API version pinned per the API contract.
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Default, Clone, Copy)]
pub struct AnthropicProvider;

impl AnthropicProvider {
    pub fn new() -> Self {
        AnthropicProvider
    }
    pub const URL: &'static str = "https://api.anthropic.com/v1/messages";
}

impl LlmProvider for AnthropicProvider {
    fn provider(&self) -> Provider {
        Provider::Anthropic
    }
    fn default_model(&self) -> &str {
        "claude-opus-4-8"
    }
    fn api_key_env(&self) -> &str {
        "ANTHROPIC_API_KEY"
    }

    fn build_request(&self, api_key: &str, req: &ChatRequest) -> Result<HttpRequest, LlmError> {
        // Anthropic messages are user/assistant only; system is top-level.
        let messages: Vec<Value> = req
            .turns()
            .map(|m| json!({"role": m.role.openai_str(), "content": m.content}))
            .collect();

        let mut body = json!({
            "model": self.model_for(req),
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
            "messages": messages,
        });
        if let Some(sys) = req.system_text() {
            body["system"] = json!(sys);
        }

        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-api-key".to_string(), api_key.to_string()),
            (
                "anthropic-version".to_string(),
                ANTHROPIC_VERSION.to_string(),
            ),
        ];

        Ok(HttpRequest {
            url: Self::URL.to_string(),
            headers,
            body: serde_json::to_vec(&body).map_err(|e| LlmError::Parse(e.to_string()))?,
        })
    }

    fn parse_response(&self, body: &[u8]) -> Result<Completion, LlmError> {
        let v: Value = serde_json::from_slice(body).map_err(|e| LlmError::Parse(e.to_string()))?;
        if v.get("type").and_then(|t| t.as_str()) == Some("error") {
            let msg = v
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            return Err(LlmError::Provider(msg.to_string()));
        }
        // Concatenate all text blocks (ignore tool_use etc. — not used in v1).
        let text = v
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .ok_or_else(|| LlmError::Parse("no content blocks".into()))?;
        let model = v
            .get("model")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string();
        let stop_reason = v
            .get("stop_reason")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        let usage = Usage {
            input_tokens: crate::openai_compat::u32_at(&v, "/usage/input_tokens"),
            output_tokens: crate::openai_compat::u32_at(&v, "/usage/output_tokens"),
        };
        Ok(Completion {
            model,
            text,
            usage,
            stop_reason,
        })
    }
}
