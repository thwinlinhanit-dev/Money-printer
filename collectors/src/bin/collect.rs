//! Thin alias for `mp-collector` (kept so older scripts keep working).
//! Prefer: `cargo run -p mp-collectors --features live-ws --bin mp-collector`.
//!
//! This binary must never drift from mp-collector: `include!` reuses the
//! exact same `main()` (flag parsing, tracing setup, run loop) so the alias
//! and the primary entry point are always the same code (audit — the old
//! stub only printed help and exited).

include!("mp-collector.rs");
