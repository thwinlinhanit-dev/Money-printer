//! Google Gemini provider (Generative Language API `:generateContent`).
//!
//! The key travels in the `x-goog-api-key` header (never in the URL, so it
//! never lands in a log line). Roles map user→`user`, assistant→`model`;
//! the system prompt goes to `systemInstruction`.

use crate::error::LlmError;
use crate::provider::{ChatRequest, Completion, HttpRequest, LlmProvider, Provider, Role, Usage};
use serde_json::{json, Value};

#[derive(Debug, Default, Clone, Copy)]
pub struct GeminiProvider;

impl GeminiProvider {
    pub fn new() -> Self {
        GeminiProvider
    }
    pub const BASE: &'static str = "https://generativelanguage.googleapis.com/v1beta/models";
}

impl LlmProvider for GeminiProvider {
    fn provider(&self) -> Provider {
        Provider::Gemini
    }
    fn default_model(&self) -> &str {
        "gemini-1.5-pro"
    }
    fn api_key_env(&self) -> &str {
        "GEMINI_API_KEY"
    }

    fn build_request(&self, api_key: &str, req: &ChatRequest) -> Result<HttpRequest, LlmError> {
        let contents: Vec<Value> = req
            .turns()
            .map(|m| {
                let role = if m.role == Role::Assistant {
                    "model"
                } else {
                    "user"
                };
                json!({"role": role, "parts": [{"text": m.content}]})
            })
            .collect();

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": req.max_tokens,
                "temperature": req.temperature,
            },
        });
        if let Some(sys) = req.system_text() {
            body["systemInstruction"] = json!({"parts": [{"text": sys}]});
        }

        let url = format!("{}/{}:generateContent", Self::BASE, self.model_for(req));
        let headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-goog-api-key".to_string(), api_key.to_string()),
        ];

        Ok(HttpRequest {
            url,
            headers,
            body: serde_json::to_vec(&body).map_err(|e| LlmError::Parse(e.to_string()))?,
        })
    }

    fn parse_response(&self, body: &[u8]) -> Result<Completion, LlmError> {
        let v: Value = serde_json::from_slice(body).map_err(|e| LlmError::Parse(e.to_string()))?;
        if let Some(err) = v.get("error") {
            return Err(LlmError::Provider(err.to_string()));
        }
        let cand = v
            .pointer("/candidates/0")
            .ok_or_else(|| LlmError::Parse("no candidates[0]".into()))?;
        let text = cand
            .pointer("/content/parts")
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .ok_or_else(|| LlmError::Parse("no content.parts".into()))?;
        let stop_reason = cand
            .get("finishReason")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        let usage = Usage {
            input_tokens: crate::openai_compat::u32_at(&v, "/usageMetadata/promptTokenCount"),
            output_tokens: crate::openai_compat::u32_at(&v, "/usageMetadata/candidatesTokenCount"),
        };
        Ok(Completion {
            model: self.default_model().to_string(),
            text,
            usage,
            stop_reason,
        })
    }
}
