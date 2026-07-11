# 000 — Engineering Conventions

## Purpose
Ground rules every other spec assumes. Violations here are bugs even when the
code "works".

## Scope
In: languages, time, numbers, determinism, errors, logging, config, testing,
IDs. Out: domain logic (see specs 001+).

## Requirements

### Languages & layout
- **CONV-1** Core, collectors, storage, features, sim, oms, risk, ops MUST be
  Rust (stable toolchain, edition 2021+, workspace at repo root with one crate
  per top-level dir).
- **CONV-2** `/research` MAY be Python 3.12 (Polars, DuckDB, matplotlib).
  Python MUST NOT appear on any live decision path (collector→feature→
  strategy→oms).
- **CONV-3** Crate dependency direction MUST be acyclic and flow downward:
  `strategies → features → core`, `sim → core`, `oms → core`,
  `risk → core`. `strategies` MUST NOT depend on `oms`, `collectors`, or any
  networking crate (Prime Directive PD-4).

### Time
- **CONV-4** All timestamps are `i64` nanoseconds since Unix epoch, UTC.
  Field suffix `_ns`. No `chrono::Local`, no naive datetimes.
- **CONV-5** Decision-path code MUST obtain time only from the injected
  `Clock` trait (`core::Clock`): `SimClock` (event-driven) or `WallClock`.
  Direct `SystemTime::now()`/`Instant::now()` on decision paths is forbidden;
  allowed in ops/telemetry code only.

### Numbers & money
- **CONV-6** Prices and quantities are `f64` in v1. Every symbol has metadata
  (`tick_size`, `step_size`, `min_notional`, `contract_multiplier`); rounding
  to venue precision happens ONLY in venue adapters (oms).
- **CONV-7** Monetary aggregation (P&L, equity) MUST use Kahan/Neumaier
  summation or periodic re-derivation from positions to bound float drift.
- **CONV-8** NaN/inf MUST never propagate: feature outputs are validated;
  a NaN produced on a decision path is an ERROR-level fault, and the affected
  signal is suppressed (fail-closed), not defaulted.

### Determinism (expands PD-3)
- **CONV-9** Decision-path code MUST be a pure function of (events, config,
  seed). Forbidden: wall clock, environment reads, network/file I/O, thread
  timing dependence, unseeded RNG.
- **CONV-10** Any map iterated where order can affect decisions MUST be
  `BTreeMap`/sorted, not `HashMap`.
- **CONV-11** Randomness, where needed, MUST come from a seeded PRNG in the
  context (`Ctx::rng`), seed recorded in run config.
- **CONV-12** Replay identity: running the same event log with the same config
  MUST produce byte-identical decision logs. A CI job enforces this on a
  golden fixture (see SIM-14).

### Errors, logging, panics
- **CONV-13** Library crates use `thiserror`; binaries may use `anyhow` at the
  edge. `unwrap()`/`expect()` only in tests or with a `// SAFETY:` comment
  proving infallibility.
- **CONV-14** `tracing` structured logging. Levels: DEBUG per-event, INFO
  state changes, WARN anomalies (gap, reconnect, reject), ERROR faults needing
  attention. Secrets and full API payloads containing auth MUST never be logged.
- **CONV-15** Live processes MUST NOT panic on malformed external input
  (venue messages, config typos): parse errors are WARN + counter + skip.
  Panics are reserved for internal invariant violations.

### Config & secrets
- **CONV-16** Config is TOML, one file per binary, all fields explicit
  (`serde(deny_unknown_fields)`), checked in as `*.example.toml` with safe
  defaults. Real config lives outside the repo.
- **CONV-17** Secrets come only from environment variables or a secrets file
  path given by env var, loaded exclusively by `oms` and `ops` binaries
  (PD-2, PD-4).
- **CONV-18** Every binary supports `--check-config` (validate and exit) and
  `--version` (git SHA embedded at build).

### IDs & schema hygiene
- **CONV-19** Internal IDs are ULIDs (sortable, unique). Client order IDs:
  `mp-{strategy_id}-{ulid}` ≤ 32 chars after venue-specific trimming.
- **CONV-20** Serialized schemas (events, journal rows) carry a `schema_ver:
  u16`. Breaking change ⇒ bump + migration note in the owning spec.

### Testing
- **CONV-21** Every spec acceptance criterion maps to ≥1 automated test whose
  name embeds the requirement ID (e.g. `evt_3_trade_roundtrip`). (Rule W-2/W-3.)
- **CONV-22** Property tests (proptest) are REQUIRED for: event serialization
  round-trips, book reconstruction, OMS state machine, sizing math.
- **CONV-23** No test may hit the network. Venue message fixtures are recorded
  once into `testdata/` (sanitized) and replayed.
- **CONV-24** `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`
  MUST pass before any push.

## Acceptance criteria
- [x] Workspace builds with one crate per top-level dir; dep graph acyclic per CONV-3. Cargo rejects a dependency cycle at build time, and the guardrails `CONV-3` check verifies every `members` entry is a real top-level crate dir (`ops/ci/guardrails.sh`).
- [x] `core::Clock` exists with `SimClock` + `WallClock` (`core/src/time.rs`, `core/src/wall_clock.rs`); the guardrails `PD-3/CONV-5` grep rejects `SystemTime::now` outside the allowlisted `core/src/wall_clock.rs`, and CI runs it.
- [x] Golden determinism fixture test exists and passes (CONV-12). `sim_14_golden_hash_is_stable`, `sim_7_replay_is_deterministic` (`sim/tests/backtest.rs`).
- [x] CI runs fmt, clippy, tests, guardrails, and the Python research suite (CONV-24). `.github/workflows/ci.yml` (rust + research + guardrails jobs; feature-gated `live-ws`/`live-http` compiles checked too).

## Decisions
- 2026-07-10: f64 (not fixed-point) for v1 prices — simplicity; revisit in a
  v2 spec if precision incidents occur.
- 2026-07-10: `WallClock` (the sole sanctioned `SystemTime::now` caller) lives
  in `core/src/wall_clock.rs`, isolated so the guardrail can allowlist exactly
  that one file. It is infrastructure that gets *injected* in place of letting
  decision code read the clock — not itself decision-path logic. The clock
  guardrail matches actual calls (`::now(`) to avoid flagging doc-comment
  mentions of the forbidden pattern.

## Open questions
- None.
