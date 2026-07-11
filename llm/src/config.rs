//! Provider construction and API-key resolution. Keys come **only** from the
//! environment (PD-2: no secrets in the repo); this module reads them at the
//! binary edge, off any decision path. See `providers.example.toml` for the
//! model/env-var map an operator fills in.

use crate::error::LlmError;
use crate::provider::{LlmProvider, Provider};

/// Box the concrete provider for a [`Provider`] value (dispatch).
pub fn provider_for(p: Provider) -> Box<dyn LlmProvider> {
    use crate::*;
    match p {
        Provider::Anthropic => Box::new(anthropic::AnthropicProvider::new()),
        Provider::OpenAi => Box::new(openai_family::OpenAiProvider::new()),
        Provider::Gemini => Box::new(gemini::GeminiProvider::new()),
        Provider::Mistral => Box::new(openai_family::MistralProvider::new()),
        Provider::Cohere => Box::new(cohere::CohereProvider::new()),
        Provider::DeepSeek => Box::new(openai_family::DeepSeekProvider::new()),
        Provider::Groq => Box::new(openai_family::GroqProvider::new()),
        Provider::XAi => Box::new(openai_family::XAiProvider::new()),
        Provider::Ollama => Box::new(openai_family::OllamaProvider::new()),
    }
}

/// Resolve the provider's API key from its env var. Keyless providers
/// (Ollama, empty env name) return an empty string. The key value is never
/// logged or embedded in error text (PD-2) — only the missing var *name* is.
pub fn key_from_env(p: &dyn LlmProvider) -> Result<String, LlmError> {
    let var = p.api_key_env();
    if var.is_empty() {
        return Ok(String::new());
    }
    std::env::var(var).map_err(|_| LlmError::MissingKey(var.to_string()))
}
