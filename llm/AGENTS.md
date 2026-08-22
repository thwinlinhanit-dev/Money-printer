# llm

## Purpose

Unified LLM provider abstraction for research brief generation and analysis. Supports Anthropic, Cohere, Gemini, and OpenAI-compatible endpoints with grounding, retries, and configurable routing.

## Ownership

- `src/provider.rs` — `Provider` trait and dispatch
- `src/anthropic.rs` — Anthropic API client
- `src/cohere.rs` — Cohere API client
- `src/gemini.rs` — Gemini API client
- `src/openai_family.rs` — OpenAI-compatible client
- `src/openai_compat.rs` — OpenAI compatibility layer
- `src/config.rs` — provider construction and key resolution (no config file)
- `providers.example.toml` — operator reference for the provider model/env map (NOT loaded by the crate; mirrors the compile-time constants in `src/*.rs`; no secrets — PD-2)
- `src/error.rs` — error types
- `src/grounding.rs` — response grounding/validation
- `src/http.rs` — HTTP transport

## Verification

- `cargo test -p mp-llm`
- `cargo test -p mp-llm --test provider_fixtures`

## Child DOX Index

None.
