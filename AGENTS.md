# DOX framework

- DOX is highly performant AGENTS.md hierarchy installed here
- Agent must follow DOX instructions across any edits

## Core Contract

- AGENTS.md files are binding work contracts for their subtrees
- Work products, source materials, instructions, records, assets, and durable docs must stay understandable from the nearest applicable AGENTS.md plus every parent AGENTS.md above it

## Read Before Editing

1. Read the root AGENTS.md
2. Identify every file or folder you expect to touch
3. Walk from the repository root to each target path
4. Read every AGENTS.md found along each route
5. If a parent AGENTS.md lists a child AGENTS.md whose scope contains the path, read that child and continue from there
6. Use the nearest AGENTS.md as the local contract and parent docs for repo-wide rules
7. If docs conflict, the closer doc controls local work details, but no child doc may weaken DOX

Do not rely on memory. Re-read the applicable DOX chain in the current session before editing.

## Update After Editing

Every meaningful change requires a DOX pass before the task is done.

Update the closest owning AGENTS.md when a change affects:

- purpose, scope, ownership, or responsibilities
- durable structure, contracts, workflows, or operating rules
- required inputs, outputs, permissions, constraints, side effects, or artifacts
- user preferences about behavior, communication, process, organization, or quality
- AGENTS.md creation, deletion, move, rename, or index contents

Update parent docs when parent-level structure, ownership, workflow, or child index changes. Update child docs when parent changes alter local rules. Remove stale or contradictory text immediately. Small edits that do not change behavior or contracts may leave docs unchanged, but the DOX pass still must happen.

## Hierarchy

- Root AGENTS.md is the DOX rail: project-wide instructions, global preferences, durable workflow rules, and the top-level Child DOX Index
- Child AGENTS.md files own domain-specific instructions and their own Child DOX Index
- Each parent explains what its direct children cover and what stays owned by the parent
- The closer a doc is to the work, the more specific and practical it must be

## Child Doc Shape

- Create a child AGENTS.md when a folder becomes a durable boundary with its own purpose, rules, responsibilities, workflow, materials, or quality standards
- Work Guidance must reflect the current standards of the project or user instructions; if there are no specific standards or instructions yet, leave it empty
- Verification must reflect an existing check; if no verification framework exists yet, leave it empty and update it when one exists

Default section order:
- Purpose
- Ownership
- Local Contracts
- Work Guidance
- Verification
- Child DOX Index

## Style

- Keep docs concise, current, and operational
- Document stable contracts, not diary entries
- Put broad rules in parent docs and concrete details in child docs
- Prefer direct bullets with explicit names
- Do not duplicate rules across many files unless each scope needs a local version
- Delete stale notes instead of explaining history
- Trim obvious statements, repeated rules, misplaced detail, and warnings for risks that no longer exist

## Closeout

1. Re-check changed paths against the DOX chain
2. Update nearest owning docs and any affected parents or children
3. Refresh every affected Child DOX Index
4. Remove stale or contradictory text
5. Run existing verification when relevant
6. Report any docs intentionally left unchanged and why

## User Preferences

When the user requests a durable behavior change, record it here or in the relevant child AGENTS.md

- Owner trading policy (capital, benchmark, max-loss) lives in `docs/OWNER_POLICY.md` — BINDING since 2026-08-25 (owner confirmed the agent-drafted values in writing). Agents still never change its limits (PD-1): any amendment goes through the owner via §6. Weekly reviews ground their benchmark on §3 and fail closed to `unset` if the file or fields go missing.
- Docs moratorium (2026-09-03, until D1–D6): `docs/COMPLETION-MASTER-PLAN.md` is the only active plan — no new specs/plans/AGENTS.md layers while it holds. Frozen planning docs live read-only under `docs/archive/` (FROZEN banners); operational state comes from `docs/STATUS.md`, regenerated from `mp-ops status`. Full rules: CLAUDE.md §Docs moratorium.

## Child DOX Index

| Child AGENTS.md | Scope |
|---|---|
| [collectors/AGENTS.md](collectors/AGENTS.md) | Market data collection crate — Binance, Bybit, Coinbase, HyperLiquid, Kraken, OKX WS/REST ingestion, normalization, event log writing |
| [core/AGENTS.md](core/AGENTS.md) | Core types crate — event schema, arena allocator, book, codec, log, mode switch, symbols |
| [sim/AGENTS.md](sim/AGENTS.md) | Backtesting engine crate — harness, fill models, account, decision log, metrics |
| [strategies/AGENTS.md](strategies/AGENTS.md) | Strategy API crate — strategy trait, carry-v1, liq-fade-v1, swing-range-reclaim-v1, trend-breadth-v1, funnel |
| [oms/AGENTS.md](oms/AGENTS.md) | Order management crate — state machine, reconciliation, execution tracking |
| [risk/AGENTS.md](risk/AGENTS.md) | Risk management crate — Kelly sizing, allocator, killswitch, governor, portfolio caps (SWG-6), risk gates |
| [storage/AGENTS.md](storage/AGENTS.md) | Parquet storage crate — compaction, SCD2, feature store, manifest, audit |
| [features/AGENTS.md](features/AGENTS.md) | Feature engineering crate — bars, engine, screener, catalog, hit journal, options Greeks/IV/flow analytics, IBIT cross-market, cohort grading, netflow velocity, accumulation detector |
| [llm/AGENTS.md](llm/AGENTS.md) | LLM provider abstraction crate — Anthropic, Cohere, Gemini, OpenAI compat |
| [ops/AGENTS.md](ops/AGENTS.md) | Operations crate — alerting, bot journal, daemon, runbooks, systemd, CI |
| [specs/AGENTS.md](specs/AGENTS.md) | Design specification documents (000–045) |
| [research/AGENTS.md](research/AGENTS.md) | Python research package — brief, grading, event studies, coverage reader |
| [data/AGENTS.md](data/AGENTS.md) | Market data storage — raw event logs, cold Parquet, manifests |
