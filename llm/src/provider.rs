//! Provider-agnostic chat types and the `LlmProvider` trait (spec 010).
//!
//! A provider is a pure translator: it turns a [`ChatRequest`] into an
//! [`HttpRequest`] (URL + headers + JSON body) and parses a raw response body
//! back into a [`Completion`]. No network I/O lives here — that is behind the
//! `live-http` feature (see `http.rs`) — so request-shaping and response
//! parsing are fully unit-testable offline against fixtures (RES-6 spirit:
//! reproducible, auditable).

use crate::error::LlmError;

/// Every provider this crate can speak to. Public market intelligence /
/// research prose only — **never** on a decision path (spec 010 grounding
/// contract; CONV-2 spirit). LLM output is read by humans, not parsed into
/// orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Gemini,
    Mistral,
    Cohere,
    DeepSeek,
    Groq,
    XAi,
    Ollama,
}

impl Provider {
    /// Lowercase stable slug used in config files and archive records.
    pub fn slug(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
            Provider::Gemini => "gemini",
            Provider::Mistral => "mistral",
            Provider::Cohere => "cohere",
            Provider::DeepSeek => "deepseek",
            Provider::Groq => "groq",
            Provider::XAi => "xai",
            Provider::Ollama => "ollama",
        }
    }

    /// Parse a slug back to a provider (config loading).
    pub fn from_slug(s: &str) -> Option<Provider> {
        Some(match s {
            "anthropic" => Provider::Anthropic,
            "openai" => Provider::OpenAi,
            "gemini" => Provider::Gemini,
            "mistral" => Provider::Mistral,
            "cohere" => Provider::Cohere,
            "deepseek" => Provider::DeepSeek,
            "groq" => Provider::Groq,
            "xai" => Provider::XAi,
            "ollama" => Provider::Ollama,
            _ => return None,
        })
    }
}

/// Conversation role. `System` is hoisted to the provider's system slot when
/// the wire protocol has one (Anthropic `system`, Gemini `systemInstruction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    /// OpenAI-family wire name.
    pub fn openai_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One turn in the conversation.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(c: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: c.into(),
        }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: c.into(),
        }
    }
    pub fn assistant(c: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: c.into(),
        }
    }
}

/// A provider-agnostic chat request. Providers translate this to their wire
/// shape; unset `model` falls back to the provider's `default_model()`.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub temperature: f64,
}

impl ChatRequest {
    /// A deterministic request: temperature 0, sized output. Research briefs
    /// want reproducibility (RES-6), so this is the sane default.
    pub fn new(messages: Vec<Message>, max_tokens: u32) -> Self {
        ChatRequest {
            model: None,
            messages,
            max_tokens,
            temperature: 0.0,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// The single system message hoisted out, if present (first wins).
    pub(crate) fn system_text(&self) -> Option<&str> {
        self.messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.content.as_str())
    }

    /// Non-system turns, in order.
    pub(crate) fn turns(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter().filter(|m| m.role != Role::System)
    }
}

/// Token accounting parsed from the response (best-effort; 0 when the provider
/// omits it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// A parsed completion — the text plus enough metadata to archive it (RES-6).
#[derive(Debug, Clone)]
pub struct Completion {
    pub model: String,
    pub text: String,
    pub usage: Usage,
    pub stop_reason: Option<String>,
}

/// A ready-to-send HTTP request. Always `POST` with a JSON body in v1.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The translator contract every provider implements.
pub trait LlmProvider {
    fn provider(&self) -> Provider;

    /// Model used when the request leaves `model` unset.
    fn default_model(&self) -> &str;

    /// Env var holding the API key. Empty string ⇒ no key needed (Ollama,
    /// local). Keys are **never** hard-coded (PD-2): resolved from env at the
    /// binary edge, off the decision path.
    fn api_key_env(&self) -> &str;

    /// Build the wire request. `api_key` may be empty for keyless providers.
    fn build_request(&self, api_key: &str, req: &ChatRequest) -> Result<HttpRequest, LlmError>;

    /// Parse a raw response body into a [`Completion`].
    fn parse_response(&self, body: &[u8]) -> Result<Completion, LlmError>;

    /// The effective model for a request (its override or our default).
    fn model_for<'a>(&'a self, req: &'a ChatRequest) -> &'a str {
        req.model.as_deref().unwrap_or_else(|| self.default_model())
    }
}
