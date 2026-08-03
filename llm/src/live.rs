//! Live provider client (feature `live-http`): the research caller's entry
//! point. Wraps one [`LlmProvider`] with a **reused** blocking HTTP client
//! (connection pooling, 120s timeout — see `http.rs`) and a shared
//! [`UsageAccumulator`] so token cost is never parsed and dropped.
//!
//! Owners construct it at the binary edge with an env-resolved key
//! (`key_from_env`, PD-2) and call [`complete`](LiveClient::complete).
//!
//! Not on any decision path (spec 010): briefs are drafted at 07:00 UTC jobs
//! and read by humans; the `HumanReadOnly` wrapper on the completion enforces
//! that structurally. Temperature comes from the caller's [`ChatRequest`] —
//! the deterministic default (`ChatRequest::new`) is 0.0 (PD-3 for research
//! reproducibility, RES-6).

use crate::completion_with_usage;
use crate::config::key_from_env;
use crate::error::LlmError;
use crate::grounding::HumanReadOnly;
use crate::http;
use crate::provider::{ChatRequest, LlmProvider, Usage};
use crate::usage::UsageAccumulator;

/// A provider call result: the human-read-only text plus its token usage.
#[derive(Debug, Clone)]
pub struct LiveCompletion {
    pub output: HumanReadOnly,
    pub usage: Usage,
    pub model: String,
}

/// One live provider client: reused HTTP client + accumulated cost.
pub struct LiveClient {
    provider: Box<dyn LlmProvider>,
    client: reqwest::blocking::Client,
    usage: UsageAccumulator,
}

impl LiveClient {
    /// Build with a resolved (possibly empty, keyless) API key is **not**
    /// taken here on purpose: pass no key material to the client struct — the
    /// key is read from env per call so a long-lived client never holds a
    /// secret longer than a request needs it. The client itself is the reuse
    /// boundary (satisfies "one `blocking::Client` per provider").
    pub fn new(provider: Box<dyn LlmProvider>) -> Result<Self, LlmError> {
        Ok(LiveClient {
            provider,
            client: http::default_client()?,
            usage: UsageAccumulator::new(),
        })
    }

    /// New client sharing an existing accumulator (e.g. one accumulator per
    /// research job across several providers).
    pub fn with_accumulator(
        provider: Box<dyn LlmProvider>,
        usage: UsageAccumulator,
    ) -> Result<Self, LlmError> {
        Ok(LiveClient {
            provider,
            client: http::default_client()?,
            usage,
        })
    }

    /// Send one chat request: build → POST (timeout + retry policy) → parse →
    /// record usage. The completion is `HumanReadOnly`-wrapped at the edge.
    pub fn complete(&self, req: &ChatRequest) -> Result<LiveCompletion, LlmError> {
        let key = key_from_env(self.provider.as_ref())?;
        let http_req = self.provider.build_request(&key, req)?;
        let body = http::send_blocking(&self.client, &http_req)?;
        let (completion, usage) = completion_with_usage(self.provider.as_ref(), &body)?;
        self.usage.record(usage)?;
        Ok(LiveCompletion {
            output: HumanReadOnly::from_text(completion.text.clone()),
            usage,
            model: completion.model,
        })
    }

    /// Running token totals for everything this client (or the shared
    /// accumulator) has recorded — the research caller's cost readout.
    pub fn totals(&self) -> crate::usage::UsageTotals {
        self.usage.totals()
    }

    /// Access the shared accumulator (to hand it to another client).
    pub fn accumulator(&self) -> &UsageAccumulator {
        &self.usage
    }
}
