# Money Printer — Agent Rulebook

This repo is a personal trading system: research → simulation → backtesting →
execution. You (an AI agent) are expected to implement it spec-first. Read this
file completely before writing any code.

## Orientation (read in this order)

1. `docs/SYSTEM_BLUEPRINT.md` — the full-loop design and philosophy. The *why*.
2. `specs/README.md` — spec index, status, and conventions. The *what*.
3. The specific spec you are implementing. The *exactly what*.
4. `.claude/skills/` — step-by-step workflows for recurring tasks. The *how*.

## The Prime Directives (non-negotiable, no exceptions)

- **PD-1 — Never enable live trading.** Do not set `mode = live`, wire real
  API keys, remove a kill switch, widen a risk limit, or weaken the risk gate.
  Only the human owner promotes anything to live. If a task seems to require
  it, stop and ask.
- **PD-2 — No secrets in the repo, ever.** No API keys, tokens, seed phrases,
  IPs of production hosts, or `.env` files with real values. Provide
  `*.example` templates instead. If you find a committed secret, stop, alert
  the owner, do not push.
- **PD-3 — Determinism is sacred.** Code on any decision path (strategies,
  features, sim) must not read the wall clock, use unseeded randomness,
  iterate hash maps in decision-relevant order, or perform any I/O. The clock
  is injected; events are the only input. See `specs/000-conventions.md` §Determinism.
- **PD-4 — Strategies never touch venues.** Strategy code emits `OrderIntent`
  only. All venue I/O lives in `/oms`. Never import venue adapters or
  credentials into `/strategies` or `/features`.
- **PD-5 — Honesty over green.** Never loosen a test, tolerance, cost model,
  or acceptance criterion to make something pass. If a spec criterion can't be
  met, report it — a failed gate is a valid, valuable result.
- **PD-6 — Spec before code.** If the change isn't covered by a spec, write or
  amend the spec first (small PR-able diff to `specs/`), then implement. Code
  that contradicts its spec is a bug even if it "works".

## Workflow rules

- **W-1** Work on the designated feature branch; never push to another branch
  without explicit permission.
- **W-2** Every requirement you implement references its ID (e.g. `EVT-3`) in
  the commit message and in a test name. Traceability is how the next agent
  trusts your work.
- **W-3** Definition of done for any spec work: requirements implemented,
  each acceptance criterion has an automated test, `cargo test` (or the
  relevant runner) passes, spec status updated in `specs/README.md`.
- **W-4** Small vertical slices. Prefer "collector connects + records trades
  for one venue, end-to-end, tested" over "all venues half-built".
- **W-5** When a spec is ambiguous, choose the safer/simpler interpretation,
  record the decision in the spec's *Decisions* section, and flag it in the PR.
- **W-6** Never delete or rewrite recorded market data. The data directories
  are append-only. Migration = write new + verify + only the human deletes.
- **W-7** Update `specs/README.md` status table in the same commit as the work.

## Engineering conventions (summary — full detail in specs/000)

- Language: **Rust** (stable) for core/collectors/sim/oms; **Python 3.12 +
  Polars/DuckDB** for `/research` only. No Python on live decision paths.
- Errors: `thiserror` for library errors, `anyhow` only at binary edges. No
  `unwrap()`/`expect()` outside tests and provably-infallible cases (comment why).
- Time: all timestamps are `i64` nanoseconds UTC. Two timestamps on every
  market event: `exch_ts_ns`, `recv_ts_ns`. Never use naive/local time.
- Money/prices: `f64` in v1 with per-symbol `tick_size`/`step_size` metadata;
  round only at the venue boundary. (Fixed-point migration is a v2 spec.)
- Logging: `tracing` with structured fields; never log secrets; DEBUG for
  per-event, INFO for state changes, WARN for anomalies, ERROR for faults.
- Tests: unit tests beside code; integration tests in `tests/`; every bug fix
  adds a regression test named `regression_<issue>_<desc>`.
- Naming: events and fields exactly as written in `specs/001-event-schema.md`.
  Do not invent synonyms (`ts` vs `timestamp` wars end here).

## Repo map (target layout — create dirs as their specs are implemented)

```
CLAUDE.md            this rulebook (AGENTS.md points here)
docs/                design docs (blueprint, brainstorm)
specs/               numbered implementation specs — source of truth
.claude/skills/      agent workflows
core/                event types, clock, ring buffers, event log      [spec 001]
collectors/          venue WS collectors → normalized events          [spec 002]
storage/             parquet writer, quality manifests, warm-store    [spec 003]
features/            streaming feature engine                         [spec 004]
sim/                 replay engine, fill models, cost model           [spec 005]
strategies/          Strategy impls, one dir each, with hypothesis.md [spec 006]
funnel/              walk-forward harness, experiment tracker, gates  [spec 005/006]
oms/                 order state machine, venue adapters, reconciler  [spec 007]
risk/                risk gate, sizing, allocator, kill switches      [spec 007/008]
ops/                 deploy, dead-man switch, telegram bot            [spec 009]
research/            notebooks, screener grading (Python)             [spec 004/005]
journal/             hypothesis entries, kill log, monthly reports
```

## Safety boundaries for agents (quick reference)

| You may freely | You must ask first | You must never |
|---|---|---|
| Implement specs, write tests | Amend a Prime Directive | Enable live mode (PD-1) |
| Run backtests/sims/paper mode | Change risk-gate default limits | Commit secrets (PD-2) |
| Add venues/strategies per spec | Add a new external dependency with network access | Bypass the risk gate or OMS |
| Refactor with tests green | Delete/rename a spec | Delete recorded data (W-6) |
| Update spec *Decisions* sections | Change the event schema (breaking) | Weaken tests to pass (PD-5) |
