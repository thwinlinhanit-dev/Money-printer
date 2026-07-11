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
| 005 | [Backtester & Simulation](005-backtester.md) | intelligence | 🔨 implementing |
| 006 | [Strategy API & Funnel](006-strategy-api.md) | intelligence | 🔨 implementing |
| 007 | [Execution: OMS, Risk Gate, Reconciler](007-execution.md) | execution | 🔨 implementing |
| 008 | [Risk & Sizing Engine](008-risk-sizing.md) | risk | 🔨 implementing |
| 009 | [Ops, Monitoring & Alerting](009-ops-alerting.md) | ops | ✅ ready |
| 010 | [Research Workflow & LLM Agents](010-research-llm.md) | intelligence | 🔨 implementing |
| 011 | [WASM Terminal](011-terminal.md) | decision plane | 📝 draft |

Status values: `draft` → `ready` (implementable) → `implementing` →
`implemented` → `superseded`. Update this table in the same commit as the work
(rule W-7).

**Recommended implementation order:** 001 → 002 → 003 → 005(L0) → 004 → 006 →
005(L1) → 008 → 007 → 009. Vertical slices beat horizontal completeness (W-4).

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
RES, UI.

## How to implement a spec

Use the skill: `.claude/skills/implement-spec/SKILL.md`. Short version:
read spec → restate requirements as a test list → build the smallest vertical
slice → make acceptance criteria pass as tests → update status → commit with
requirement IDs (W-2, W-3).
