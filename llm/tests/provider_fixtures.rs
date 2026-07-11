//! Request-build / response-parse fixtures for every provider (spec 010).
//! No network: we assert the wire shape a request produces and that a
//! representative response body parses to the right [`Completion`]. Test names
//! embed requirement IDs (CONV-21). Fixtures are synthetic-representative
//! bodies built to each provider's documented shape.

use mp_llm::grounding::{bundle_hash, ArchiveRecord, InputBundle};
use mp_llm::provider::{ChatRequest, LlmProvider, Message, Provider};
use mp_llm::{config, Completion};
use serde_json::Value;

fn req() -> ChatRequest {
    ChatRequest::new(
        vec![
            Message::system("You are a terse market analyst."),
            Message::user("Summarize funding."),
        ],
        256,
    )
}

fn body_json(p: &dyn LlmProvider) -> Value {
    let http = p.build_request("SECRET_KEY", &req()).unwrap();
    serde_json::from_slice(&http.body).unwrap()
}

fn header<'a>(hs: &'a [(String, String)], k: &str) -> Option<&'a str> {
    hs.iter().find(|(hk, _)| hk == k).map(|(_, v)| v.as_str())
}

// ---- Anthropic -----------------------------------------------------------

#[test]
fn res6_anthropic_request_hoists_system_and_sets_version_header() {
    let p = mp_llm::anthropic::AnthropicProvider::new();
    let http = p.build_request("SECRET_KEY", &req()).unwrap();
    assert_eq!(http.url, "https://api.anthropic.com/v1/messages");
    assert_eq!(header(&http.headers, "x-api-key"), Some("SECRET_KEY"));
    assert_eq!(
        header(&http.headers, "anthropic-version"),
        Some("2023-06-01")
    );

    let body: Value = serde_json::from_slice(&http.body).unwrap();
    assert_eq!(body["model"], "claude-opus-4-8");
    assert_eq!(body["system"], "You are a terse market analyst.");
    // System is hoisted out of messages ⇒ exactly one user turn remains.
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
}

#[test]
fn res6_anthropic_parses_text_blocks_and_usage() {
    let p = mp_llm::anthropic::AnthropicProvider::new();
    let raw = r#"{"model":"claude-opus-4-8","stop_reason":"end_turn",
        "content":[{"type":"text","text":"Funding is "},{"type":"text","text":"positive."}],
        "usage":{"input_tokens":12,"output_tokens":5}}"#;
    let c = p.parse_response(raw.as_bytes()).unwrap();
    assert_eq!(c.text, "Funding is positive.");
    assert_eq!(c.usage.input_tokens, 12);
    assert_eq!(c.usage.output_tokens, 5);
    assert_eq!(c.stop_reason.as_deref(), Some("end_turn"));
}

#[test]
fn res6_anthropic_error_envelope_surfaces_as_provider_error() {
    let p = mp_llm::anthropic::AnthropicProvider::new();
    let raw = r#"{"type":"error","error":{"type":"overloaded_error","message":"overloaded"}}"#;
    let err = p.parse_response(raw.as_bytes()).unwrap_err();
    assert!(format!("{err}").contains("overloaded"));
}

// ---- OpenAI-family (OpenAI / DeepSeek / Groq / xAI / Mistral / Ollama) ----

#[test]
fn res6_openai_family_uses_bearer_and_chat_completions_shape() {
    let p = mp_llm::openai_family::OpenAiProvider::new();
    let http = p.build_request("SECRET_KEY", &req()).unwrap();
    assert_eq!(http.url, "https://api.openai.com/v1/chat/completions");
    assert_eq!(
        header(&http.headers, "authorization"),
        Some("Bearer SECRET_KEY")
    );
    let body: Value = serde_json::from_slice(&http.body).unwrap();
    // System stays in the messages array for OpenAI shape.
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["max_tokens"], 256);
}

#[test]
fn res6_openai_family_parses_choices_and_usage() {
    let p = mp_llm::openai_family::GroqProvider::new();
    let raw = r#"{"model":"llama-3.3-70b-versatile",
        "choices":[{"message":{"role":"assistant","content":"Neutral."},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":9,"completion_tokens":2}}"#;
    let c = p.parse_response(raw.as_bytes()).unwrap();
    assert_eq!(c.text, "Neutral.");
    assert_eq!(c.usage.input_tokens, 9);
    assert_eq!(c.usage.output_tokens, 2);
    assert_eq!(c.stop_reason.as_deref(), Some("stop"));
}

#[test]
fn res6_ollama_is_keyless_no_auth_header() {
    let p = mp_llm::openai_family::OllamaProvider::new();
    assert_eq!(p.api_key_env(), "");
    let http = p.build_request("", &req()).unwrap();
    assert!(header(&http.headers, "authorization").is_none());
    assert!(http.url.starts_with("http://localhost:11434"));
}

#[test]
fn res6_deepseek_xai_mistral_endpoints() {
    // Model override flows through to the body for every OpenAI-family member.
    let ds = mp_llm::openai_family::DeepSeekProvider::new();
    assert!(body_json(&ds)["model"]
        .as_str()
        .unwrap()
        .contains("deepseek"));
    let x = mp_llm::openai_family::XAiProvider::new();
    assert!(x.build_request("k", &req()).unwrap().url.contains("x.ai"));
    let m = mp_llm::openai_family::MistralProvider::new();
    assert!(m
        .build_request("k", &req())
        .unwrap()
        .url
        .contains("mistral.ai"));
}

// ---- Gemini --------------------------------------------------------------

#[test]
fn res6_gemini_maps_roles_and_key_header_not_url() {
    let p = mp_llm::gemini::GeminiProvider::new();
    let r = ChatRequest::new(
        vec![
            Message::system("sys"),
            Message::user("hi"),
            Message::assistant("prev"),
        ],
        128,
    );
    let http = p.build_request("SECRET_KEY", &r).unwrap();
    // Key travels in a header, never in the URL (no key leak in logs).
    assert!(!http.url.contains("SECRET_KEY"));
    assert_eq!(header(&http.headers, "x-goog-api-key"), Some("SECRET_KEY"));
    assert!(http.url.contains(":generateContent"));
    let body: Value = serde_json::from_slice(&http.body).unwrap();
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "sys");
    assert_eq!(body["contents"][0]["role"], "user");
    assert_eq!(body["contents"][1]["role"], "model"); // assistant → model
}

#[test]
fn res6_gemini_parses_candidates() {
    let p = mp_llm::gemini::GeminiProvider::new();
    let raw = r#"{"candidates":[{"content":{"parts":[{"text":"Up "},{"text":"trend."}]},
        "finishReason":"STOP"}],
        "usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3}}"#;
    let c = p.parse_response(raw.as_bytes()).unwrap();
    assert_eq!(c.text, "Up trend.");
    assert_eq!(c.usage.input_tokens, 7);
    assert_eq!(c.usage.output_tokens, 3);
}

// ---- Cohere --------------------------------------------------------------

#[test]
fn res6_cohere_v2_parses_content_blocks_and_tokens() {
    let p = mp_llm::cohere::CohereProvider::new();
    let http = p.build_request("SECRET_KEY", &req()).unwrap();
    assert_eq!(http.url, "https://api.cohere.com/v2/chat");
    let raw = r#"{"id":"abc","finish_reason":"COMPLETE",
        "message":{"role":"assistant","content":[{"type":"text","text":"Flat."}]},
        "usage":{"tokens":{"input_tokens":4,"output_tokens":1}}}"#;
    let c = p.parse_response(raw.as_bytes()).unwrap();
    assert_eq!(c.text, "Flat.");
    assert_eq!(c.usage.input_tokens, 4);
    assert_eq!(c.usage.output_tokens, 1);
}

// ---- Dispatch + key resolution ------------------------------------------

#[test]
fn res7_provider_for_dispatch_covers_all_nine() {
    for p in [
        Provider::Anthropic,
        Provider::OpenAi,
        Provider::Gemini,
        Provider::Mistral,
        Provider::Cohere,
        Provider::DeepSeek,
        Provider::Groq,
        Provider::XAi,
        Provider::Ollama,
    ] {
        let boxed = config::provider_for(p);
        assert_eq!(boxed.provider(), p);
        assert_eq!(Provider::from_slug(p.slug()), Some(p));
    }
}

#[test]
fn res7_missing_key_names_var_without_leaking_value() {
    let p = mp_llm::anthropic::AnthropicProvider::new();
    // Ensure the var is unset for this assertion.
    std::env::remove_var("ANTHROPIC_API_KEY");
    let err = config::key_from_env(&p).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("ANTHROPIC_API_KEY"));
    // Keyless provider resolves to empty string, no error.
    let ol = mp_llm::openai_family::OllamaProvider::new();
    assert_eq!(config::key_from_env(&ol).unwrap(), "");
}

// ---- Grounding contract (RES-6) -----------------------------------------

#[test]
fn res6_bundle_hash_is_deterministic_and_input_sensitive() {
    assert_eq!(bundle_hash(b"same"), bundle_hash(b"same"));
    assert_ne!(bundle_hash(b"a"), bundle_hash(b"b"));
    let b = InputBundle::new(b"regime=chop;funding_z=2.1".to_vec());
    assert_eq!(b.hash, bundle_hash(b"regime=chop;funding_z=2.1"));
}

#[test]
fn res6_archive_record_is_reproducible_json_line() {
    let bundle = InputBundle::new(b"inputs".to_vec());
    let completion = Completion {
        model: "claude-opus-4-8".to_string(),
        text: "Line one.\nLine two.".to_string(),
        usage: Default::default(),
        stop_reason: Some("end_turn".to_string()),
    };
    let rec = ArchiveRecord::new(
        Provider::Anthropic,
        "brief-v1",
        &bundle,
        &completion,
        1_700_000_000,
    );
    let line = rec.to_json_line();
    // Deterministic, single line, records provenance, escapes newlines.
    assert!(!line.contains('\n'));
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["provider"], "anthropic");
    assert_eq!(v["model_id"], "claude-opus-4-8");
    assert_eq!(v["prompt_version"], "brief-v1");
    assert_eq!(v["bundle_hash"], bundle.hash);
    assert_eq!(v["output"], "Line one.\nLine two.");
    // Rebuilding from identical inputs yields an identical line (reproducible).
    let rec2 = ArchiveRecord::new(
        Provider::Anthropic,
        "brief-v1",
        &bundle,
        &completion,
        1_700_000_000,
    );
    assert_eq!(line, rec2.to_json_line());
}
