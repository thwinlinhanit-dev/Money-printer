//! Error type for the LLM crate. `thiserror` per CONV (library errors).

/// Errors from building requests, parsing responses, key resolution, and
/// (behind `live-http`) transport.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The response body was not valid JSON or lacked an expected field.
    #[error("llm parse error: {0}")]
    Parse(String),

    /// The provider returned an error envelope (surfaced verbatim for the log).
    #[error("llm provider error: {0}")]
    Provider(String),

    /// Required API key env var is unset (name in the message). Never contains
    /// the key value itself (PD-2).
    #[error("missing api key: set env var {0}")]
    MissingKey(String),

    /// Transport failure (only reachable with the `live-http` feature).
    #[error("llm transport error: {0}")]
    Transport(String),
}
