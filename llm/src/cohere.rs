//! Cohere Chat API v2 provider (`POST /v2/chat`).
//!
//! v2 accepts an OpenAI-style `messages` array but returns the assistant text
//! as a list of content blocks under `message.content`, and token usage under
//! `usage.tokens`.

use crate::error::LlmError;
use crate::provider::{ChatRequest, Completion, HttpRequest, LlmProvider, Provider, Usage};
use serde_json::{json, Value};

#[derive(Debug, Default, Clone, Copy)]
pub struct CohereProvider;

impl CohereProvider {
    pub fn new() -> Self {
        CohereProvider
    }
    pub const URL: &'static str = "https://api.cohere.com/v2/chat";
}

impl LlmProvider for CohereProvider {
    fn provider(&self) -> Provider {
        Provider::Cohere
    }
    fn default_model(&self) -> &str {
        "command-r-plus"
    }
    fn api_key_env(&self) -> &str {
        "COHERE_API_KEY"
    }

    fn build_request(&self, api_key: &str, req: &ChatRequest) -> Result<HttpRequest, LlmError> {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| json!({"role": m.role.openai_str(), "content": m.content}))
            .collect();

        let body = json!({
            "model": self.model_for(req),
            "messages": messages,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
        });

        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("authorization".to_string(), format!("Bearer {api_key}")),
        ];

        Ok(HttpRequest {
            url: Self::URL.to_string(),
            headers,
            body: serde_json::to_vec(&body).map_err(|e| LlmError::Parse(e.to_string()))?,
        })
    }

    fn parse_response(&self, body: &[u8]) -> Result<Completion, LlmError> {
        let v: Value = serde_json::from_slice(body).map_err(|e| LlmError::Parse(e.to_string()))?;
        if let Some(msg) = v.get("message").filter(|_| v.get("id").is_none()) {
            // Cohere error envelopes carry `message` at top level without an id.
            if let Some(s) = msg.as_str() {
                return Err(LlmError::Provider(s.to_string()));
            }
        }
        let text = v
            .pointer("/message/content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .ok_or_else(|| LlmError::Parse("no message.content".into()))?;
        let stop_reason = v
            .get("finish_reason")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        let usage = Usage {
            input_tokens: crate::openai_compat::u32_at(&v, "/usage/tokens/input_tokens"),
            output_tokens: crate::openai_compat::u32_at(&v, "/usage/tokens/output_tokens"),
        };
        Ok(Completion {
            model: self.default_model().to_string(),
            text,
            usage,
            stop_reason,
        })
    }
}
