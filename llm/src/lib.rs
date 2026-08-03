//! `mp-llm` — provider abstraction for the research/intelligence LLM agents
//! (spec 010). Turns a provider-agnostic [`ChatRequest`] into a wire request
//! and parses the response, for nine popular providers. Actual network I/O is
//! behind the `live-http` feature; the request-build / response-parse logic is
//! pure and offline-testable.
//!
//! **Grounding contract (normative):** LLM output is read by humans, never
//! parsed into decisions — there is no path from a [`Completion`] to an
//! `OrderIntent` (spec 010 Decisions; see [`grounding::HumanReadOnly`]). Keys
//! come only from the environment (PD-2). No LLM on any decision path.

pub mod anthropic;
pub mod cohere;
pub mod config;
pub mod error;
pub mod gemini;
pub mod grounding;
pub mod openai_compat;
pub mod openai_family;
pub mod provider;
pub mod usage;

#[cfg(feature = "live-http")]
pub mod http;
#[cfg(feature = "live-http")]
pub mod live;

pub use config::{key_from_env, provider_for};
pub use error::LlmError;
pub use grounding::{ArchiveRecord, HumanReadOnly, InputBundle};
pub use provider::{
    completion_with_usage, ChatRequest, Completion, HttpRequest, LlmProvider, Message, Provider,
    Role, Usage,
};
pub use usage::{UsageAccumulator, UsageError, UsageTotals};
