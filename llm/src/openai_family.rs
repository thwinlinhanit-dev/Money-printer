//! The six providers that share the OpenAI Chat Completions wire shape. They
//! differ only in endpoint URL, default model, and API-key env var.

use crate::error::LlmError;
use crate::openai_compat;
use crate::provider::{ChatRequest, Completion, HttpRequest, LlmProvider, Provider};

macro_rules! openai_like {
    ($name:ident, $prov:expr, $url:expr, $model:expr, $env:expr) => {
        #[doc = concat!("OpenAI-compatible provider: ", $url)]
        #[derive(Debug, Default, Clone, Copy)]
        pub struct $name;
        impl $name {
            pub fn new() -> Self {
                $name
            }
            /// Endpoint URL (public knowledge, not a secret).
            pub const URL: &'static str = $url;
        }
        impl LlmProvider for $name {
            fn provider(&self) -> Provider {
                $prov
            }
            fn default_model(&self) -> &str {
                $model
            }
            fn api_key_env(&self) -> &str {
                $env
            }
            fn build_request(
                &self,
                api_key: &str,
                req: &ChatRequest,
            ) -> Result<HttpRequest, LlmError> {
                openai_compat::build(self, $url, api_key, req)
            }
            fn parse_response(&self, body: &[u8]) -> Result<Completion, LlmError> {
                openai_compat::parse(body)
            }
        }
    };
}

openai_like!(
    OpenAiProvider,
    Provider::OpenAi,
    "https://api.openai.com/v1/chat/completions",
    "gpt-4o",
    "OPENAI_API_KEY"
);
openai_like!(
    DeepSeekProvider,
    Provider::DeepSeek,
    "https://api.deepseek.com/v1/chat/completions",
    "deepseek-chat",
    "DEEPSEEK_API_KEY"
);
openai_like!(
    GroqProvider,
    Provider::Groq,
    "https://api.groq.com/openai/v1/chat/completions",
    "llama-3.3-70b-versatile",
    "GROQ_API_KEY"
);
openai_like!(
    XAiProvider,
    Provider::XAi,
    "https://api.x.ai/v1/chat/completions",
    "grok-2-latest",
    "XAI_API_KEY"
);
openai_like!(
    MistralProvider,
    Provider::Mistral,
    "https://api.mistral.ai/v1/chat/completions",
    "mistral-large-latest",
    "MISTRAL_API_KEY"
);
// Ollama runs locally and speaks the OpenAI shape at /v1. Empty key env ⇒ no
// auth header (keyless local transport). Default host is the documented one;
// override the URL via config for a remote host.
openai_like!(
    OllamaProvider,
    Provider::Ollama,
    "http://localhost:11434/v1/chat/completions",
    "llama3.1",
    ""
);
