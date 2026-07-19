# Specs — Index & Conventions

Specs are the source of truth for implementation. Design intent lives in
`docs/`; if a spec and a design doc disagree, **the spec wins** (and the doc
should be updated).

## Status

| # | Spec | Area | Status |
|---|------|------|--------|
| 000 | [Conventions](000-conventions.md) | engineering ground rules | 🔨 implementing |
| 001 | [Event Schema](001-event-schema.md) | core types | ✅ implemented |
| 002 | [Collectors](002-collectors.md) | data plane | 🔨 implementing |
| 003 | [Storage](003-storage.md) | data plane | 🔨 implementing |
| 004 | [Feature Engine](004-feature-engine.md) | intelligence | 🔨 implementing |
| 005 | [Backtester & Simulation](005-backtester.md) | intelligence | ✅ implemented |
| 006 | [Strategy API & Funnel](006-strategy-api.md) | intelligence | ✅ implemented |
| 007 | [Execution: OMS, Risk Gate, Reconciler](007-execution.md) | execution | 🔨 implementing |
| 008 | [Risk & Sizing Engine](008-risk-sizing.md) | risk | ✅ implemented |
| 009 | [Ops, Monitoring & Alerting](009-ops-alerting.md) | ops | 🔨 implementing |
| 010 | [Research Workflow & LLM Agents](010-research-llm.md) | intelligence | ✅ implemented |
| 011 | [WASM Terminal](011-terminal.md) | decision plane | 📝 draft |
| 012 | [Zero-Copy Event Pipeline](012-zero-copy-pipeline.md) | core | 📝 draft |
| 013 | [WS Backpressure Policy](013-ws-backpressure.md) | collectors | 📝 draft |
| 014 | [Event Log Fsync Policy](014-event-log-fsync.md) | core/storage | 📝 draft |
| 015 | [carry-v1 Strategy](015-carry-v1.md) | intelligence | 📝 draft |
| 016 | [Feature Materialization](016-feature-materialization.md) | intelligence | 📝 draft |
| 017 | [Screener Hit Journal & Grading](017-screener-grading.md) | intelligence | 📝 draft |
| 018 | [Paper/Shadow/Live Mode Switch](018-mode-switch.md) | ops | 📝 draft |
| 019 | [Collector Binary & Systemd](019-collector-binary.md) | collectors | 📝 draft |
| 020 | [Binance REST Snapshot](020-binance-snapshot.md) | collectors | 📝 draft |
| 021 | [Bot Command Journal](021-bot-journal.md) | ops | 📝 draft |
| 022 | [Screener Evaluation Cadence](022-screener-cadence.md) | intelligence | 📝 draft |
| 023 | [String Interning in Features](023-string-interning.md) | intelligence | 📝 draft |

Status values: `draft` → `ready` (implementable) → `implementing` →
`implemented` → `superseded`. Update this table in the same commit as the work
(rule W-7).

**Recommended implementation order:** 001 → 002 → 003 → 005(L0) → 004 → 006 →
005(L1) → 008 → 007 → 009. Vertical slices beat horizontal completeness (W-4).
Newer specs (012–023) are Phase 2+ and do not block Phase 0.

## Spec format

Every spec uses the same skeleton:

```
# NNN — Title
Purpose / Scope (in & out)
Design (diagrams, data shapes)
Requirements   — numbered, testable: <PREFIX>-<n>, MUST/SHOULD language
Acceptance criteria — checklist; each item becomes an automated test
Decisions      — dated log of ambiguity resolutions (agents append here, rule W-5)
Open questions — needs human input; do NOT guess these
```

Requirement prefixes: CONV, EVT, COL, STO, FEA, SIM, STR, EXE, RSK, OPS,
RES, UI, ZCP (012), BKP (013), FSP (014), MAT (016), GRD (017), MOD (018).

## How to implement a spec

Use the skill: `.claude/skills/implement-spec/SKILL.md`. Short version:
read spec → restate requirements as a test list → build the smallest vertical
slice → make acceptance criteria pass as tests → update status → commit with
requirement IDs (W-2, W-3).
