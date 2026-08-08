# Independent Full-System Audit — 2026-08-06 (team pass)

Independent adversarial audit of the whole workspace (10 Rust crates + `research/`
Python package + specs + ops + the dirty working tree). Lead auditor ran baseline
verification first-hand; four parallel sub-auditors (A safety/hygiene, B
core/storage/features, C collectors/ops/llm, D risk/strategies/sim/oms) did deep
code-level reviews of the same tree. Findings below were re-verified by the lead
(not re-trusted from docs), with the two P2s on new code confirmed first-hand.

**Scope:** tree at branch `feature/signal-catalog-footprint`, HEAD `1e97ff0`,
plus a **dirty working tree** (~32 modified + untracked new files, incl.
uncommitted `mp-materialize`, `guardrails.ps1`, ws egress runbook).

---

## Baseline (verified by the lead auditor, this session)

| Check | Result |
|---|---|
| `cargo check --workspace` | clean |
| `cargo test --workspace` | **all green — 66 suites, 368 tests, 0 failures, 0 doc-test failures** |
| `py -3 -m pytest research/tests` | **49/49 pass** (`python` not on PATH; use `py -3`) |
| `cargo clippy --workspace --all-targets` | clean (0 warnings) |
| `ops/ci/guardrails.ps1` | **"all checks passed"** (ran first-hand on native Windows host) |
| Secrets / credentials sweep | clean (PD-2) — no real `.env`/`config.toml`/key material tracked |
| Committed secret patterns (`sk-`, `AKIA`, private keys) | none found |
| PD-1 (never live) | structurally clean — no venue adapter, no credentials in code |

---

## Prime-Directive posture

| Directive | Verdict | Notes |
|---|---|---|
| **PD-1** Never live | PASS structurally | No execution venue adapter; `Mode::Live` remains constructible via env/`mode.toml` (convention only — open compile/init-time barrier). Kill switches intact & one-way (human-required reset); risk-limit defaults unchanged. |
| **PD-2** No secrets | PASS | Guardrails + manual sweep clean. `llm/src/config.rs`/`http.rs` never log key material (strip URL/bodies from transport errors). |
| **PD-3** Determinism | PASS on decision paths | Wall clock confined to `core/src/wall_clock.rs` + infra; features/strategies/sim use injected clock / seeded RNG (`SplitMix64`) / `BTreeMap` order. |
| **PD-4** Strategies ≠ venues | PASS | No venue/credential deps in strategies/features; `Ctx` exposes no venue handles at compile time. |
| **PD-5** Honesty over green | PASS | No `#[ignore]`, no loosened tolerances, no hidden-failure tests. |
| **PD-6** Spec before code | PASS in spirit | Specs 000–031 present; uncommitted materialize work documented in `POST_CLEAN_PLAN`/spec 016. |

---

## Findings (verified)

### P2 — genuine defects in the new/uncommitted code

1. **[P2] `paper-tail` builds carry-v1 with an empty event universe → silently never trades.**
   - `sim/src/bin/sim.rs:291` passes `&[]` to `strategy_named`; `universe_from_events(&[])`
     returns an empty `Universe`. `strategies/src/carry_v1.rs:352` gates every funding
     update on `self.universe.symbols.contains(&u.symbol)` → always false (and the
     venue fallback `Hyperliquid` ≠ the engine's `Venue::Bybit`), so carry-v1 emits zero
     intents in `paper-tail` — yet still writes a `RunRecord`/decision log certifying a
     useless empty session. One-shot `paper` correctly passes real events. Paper-only →
     P2. (Auditor D; lead-verified against code.)

2. **[P2] `ws_probe.mjs` misclassifies a Binance SUBSCRIBE *error* as an ack → defeats its fail-closed exit.**
   - `ops/scripts/ws_probe.mjs:74` treats `{ "error": {...}, "id": 1 }` (a subscribe
     rejection) as the ack because it checks only `j.id === 1 && !j.e`, so it exits **0**
     on a subscribe failure instead of the documented exit 2. (Auditor C; lead-verified.)

3. **[P2] `post_p1_webhook` echoes curl stderr verbatim on failure → can re-expose the webhook URL/token the verdict deliberately hides (PD-2).**
   - `ops/src/alert.rs:268-274` returns curl's stderr (URL-bearing on transport errors)
     in the error string, while the command deliberately never echoes the URL because it
     may embed a sink key. Failure path leaks where the success path is clean. (Auditor C.)

4. **[P2] `engine_git_sha` participates in the W-6 no-overwrite content hash → legitimate metadata-only re-materialization hard-errors.**
   - `storage/src/feature_store.rs:126,141-160`: the offline `{date}.parquet` content
     hash includes the footer `engine_git_sha`, so re-running `mp-materialize` with the
     real `--git-sha` after an `unknown` run is byte-identical data yet rejected as
     "refusing to overwrite with different content". Fail-safe (never a clobber) but
     mislabels provenance re-stamping as data divergence. (Auditor B.)

5. **[P2] Version allocation keys only on `params_hash`, ignoring `feature_ver` → a feature-code upgrade can't open a new version.**
   - `storage/src/feature_store.rs:230-257` (`resolve_version`) documents "changed
     params **or feature version** allocates a new ver" but only consults `params_hash`;
     a semantics change with unchanged params reuses `ver=0` and then trips the W-6
     overwrite guard. Latent (all `TickFeature::ver()` are 1) but the advertised
     contract is unimplemented. (Auditor B.)

### P3 — hardening / correctness margins

6. **[P3] SIM-4 funding guard satisfiable by funding ticks observed while flat** — `sim/src/engine.rs:316-322,367` counts all funding events regardless of hold, so a re-open after a gap can be certified without its own held-window funding. (D)
7. **[P3] `contract_multiplier` never populated anywhere** — sizer/gate always fall back to `1.0` (`engine.rs:583-587,613-617`), so inverse/`$`-per-contract semantics are documented but never realized. (D)
8. **[P3] `PaperSession` dedups on a single last key** — an out-of-order new frame is dropped as a "duplicate" (`paper.rs:41-49`); correct under append-only monotonic tail only. (D)
9. **[P3] Multi-strategy `Ctx::equity_allocated()` returns full shared equity + `alloc_weight` hardcoded `1.0`** — per-strategy risk budgeting not modeled (`engine.rs:479,577`). (D)
10. **[P3] Offline materializer can't extend a partially-materialized day** (fixed `{date}.parquet` + W-6 guard → hard error). (B)
11. **[P3] `HitJournal::record` partitions by processing-day clock, not the hit's own event day** → `read_range` by event date can miss records (`features/src/hit_journal.rs`). (B)
12. **[P3] `SignalCatalog::register` uses `last_mut().unwrap()` without a CONV-13 infallibility comment** (`features/src/signal_catalog.rs:295`). (B)
13. **[P3] `webhook_url_ok` is only a scheme prefix check** — accepts `http://` empty-authority etc., overstating its "well-formed" contract (`ops/src/alert.rs:294-296`). (C)
14. **[P3] P1-only sink invariant enforced only by `debug_assert_eq!`** (compiles out of release) — defense-in-depth gap (`ops/src/alert.rs:245`). (C)
15. **[P3] `ws_probe.mjs` exits 0 if socket dies mid-window after ack** — exit code not a reliable liveness signal for the broader disconnect case. (C)
16. **[P3] Guardrails don't mechanically verify kill-switch/risk-limit integrity** (governance-only) — observed, not a violation. (A)

---

## What is genuinely strong

- Baseline fully green: 368 Rust + 49 Python tests, clippy clean, guardrails pass.
- W-6 no-overwrite guard is genuine and fail-closed in both the streaming store and
  the new offline materializer; no silent clobber of recorded data.
- PD-1..6 structurally respected; secret handling (llm config/http) is careful.
- Recent quality trajectory strong; blocking findings down ~4–5 orders of magnitude
  from mid-July.

---

## Recommended next steps (priority order)

### Fix now (new-code defects, safe, no owner gate)
1. `paper-tail`: build the strategy from the live log's discovered universe on the
   first poll (or stream the universe) instead of `&[]` — closes P2 #1.
2. `ws_probe.mjs`: distinguish the SUBSCRIBE success `result` from an `error` field so
   a subscribe rejection exits 2 (P2 #2).
3. `post_p1_webhook`: sanitize curl stderr (strip URL) on the failure path (P2 #3).
4. `mp-materialize`: exclude `engine_git_sha` from the W-6 equality hash, and consult
   `feature_ver` in `resolve_version` (P2 #4/#5) — with spec 016 decisions + tests.
5. Commit the dirty tree in logical slices (ops P1 + guardrails; storage materialize;
   docs) — do not leave on a Downloads tree.

### Soon (hardening)
6. Always require `MP_OPSD_TOKEN` even on loopback.
7. Populate `contract_multiplier` from symbol metadata once the feed carries it (P3 #7).
8. Add the compile/init-time `Mode::Live` refusal barrier (open spec follow-up).

### Owner-gated (do not implement in agent sessions)
9. Provision real Telegram / P1 webhook credentials.
10. Paper/live venue adapters only after the Phase-0 gate + written evidence.

---

## Verdict

Codebase is **in strong shape**: builds, tests, clippy, and guardrails are green;
Prime Directives are structurally respected; prior audit fixes hold; capital at risk
is $0. The binding constraints remain the **ops / Phase-0 data-integrity** ones (0
consecutive clean days, data on a disposable path, symbol-set drift, ongoing Binance
WS sequence gaps) plus the **five P2s on new code** above — none of which are
data-destructive (W-6 stays intact) but two of which (paper-tail silence, ws-probe
fail-open) undermine their own fail-closed contracts.

*Independent team audit, 2026-08-06. Baseline verified first-hand (cargo/pytest/
clippy/guardrails); P2 findings re-checked against code by the lead; P3s reported by
sub-auditors with file:line evidence.*

