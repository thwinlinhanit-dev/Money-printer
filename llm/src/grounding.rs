//! The grounding contract (spec 010, RES-6/RES-7). Two jobs:
//!
//! 1. **Reproducibility (RES-6):** every LLM job archives `{input bundle,
//!    prompt version, model id, provider, output}`. The input bundle is
//!    content-hashed so a brief is reproducible or it doesn't ship.
//! 2. **No LLM on any decision path (spec 010 Decisions, CONV-2 spirit):**
//!    completion text is wrapped in [`HumanReadOnly`], which exposes the string
//!    for display/archival but offers **no** path to an `OrderIntent` or any
//!    decision type. The guarantee is structural, not a comment.

use crate::provider::{Completion, Provider};

/// Deterministic 64-bit FNV-1a content hash of an input bundle. Non-crypto,
/// sufficient for archival identity in v1 (documented in spec 010 Decisions).
pub fn bundle_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The exact bytes fed to the model, plus their hash. Construct from the
/// canonical serialized inputs (SQL/Parquet exports rendered into the prompt).
#[derive(Debug, Clone)]
pub struct InputBundle {
    pub bytes: Vec<u8>,
    pub hash: u64,
}

impl InputBundle {
    pub fn new(bytes: Vec<u8>) -> Self {
        let hash = bundle_hash(&bytes);
        InputBundle { bytes, hash }
    }
}

/// Completion text destined for a human reader only. There is deliberately no
/// method that yields a decision/order type — an LLM output cannot be parsed
/// into the decision path (spec 010). It can be read, displayed, and archived.
#[derive(Debug, Clone)]
pub struct HumanReadOnly(String);

impl HumanReadOnly {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Consume into the raw string — for writing to `journal/briefs/` or a
    /// Telegram message. Still human-destined; callers on decision paths have
    /// no business here (enforced by review + Lever-2 permissions, RES-7).
    pub fn into_string(self) -> String {
        self.0
    }
}

/// An append-only archive record for one LLM job (RES-6). Serialize to JSON
/// and write under `journal/briefs/` (W-6 append-only). Carries no secrets.
#[derive(Debug, Clone)]
pub struct ArchiveRecord {
    pub provider: Provider,
    pub model_id: String,
    pub prompt_version: String,
    pub bundle_hash: u64,
    pub output: HumanReadOnly,
    /// Injected clock timestamp (ns UTC) — never `SystemTime::now` here (PD-3).
    pub created_ts_ns: i64,
}

impl ArchiveRecord {
    /// Build the record from a parsed completion and its grounding inputs.
    pub fn new(
        provider: Provider,
        prompt_version: impl Into<String>,
        bundle: &InputBundle,
        completion: &Completion,
        created_ts_ns: i64,
    ) -> Self {
        ArchiveRecord {
            provider,
            model_id: completion.model.clone(),
            prompt_version: prompt_version.into(),
            bundle_hash: bundle.hash,
            output: HumanReadOnly(completion.text.clone()),
            created_ts_ns,
        }
    }

    /// Canonical one-line JSON for the append-only journal. Deterministic key
    /// order (RES-6 reproducibility).
    pub fn to_json_line(&self) -> String {
        // Hand-rolled to guarantee key order and avoid pulling serde derive
        // onto this small record; strings are JSON-escaped.
        format!(
            "{{\"provider\":{},\"model_id\":{},\"prompt_version\":{},\"bundle_hash\":{},\"created_ts_ns\":{},\"output\":{}}}",
            esc(self.provider.slug()),
            esc(&self.model_id),
            esc(&self.prompt_version),
            self.bundle_hash,
            self.created_ts_ns,
            esc(self.output.as_str()),
        )
    }
}

/// Minimal JSON string escaper for archive lines.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
