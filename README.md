# Money Printer — Trading Research & Intelligence System

This repo is the home for a personal trading **research and intelligence platform**:
a system that captures market data, computes order-flow and derivatives analytics,
runs research/backtests over the recorded history, and surfaces everything through
fast dashboards, screeners, alerts, and generated reports.

The design takes direct inspiration from [Cryexc](https://cryexc.josedonato.com/app)
(a C++ / Dear ImGui / WebAssembly trading terminal), but pivots from *terminal*
(eyes) to *intelligence system* (eyes + memory + brain).

## For AI agents (and humans acting like them)

- [`CLAUDE.md`](CLAUDE.md) — the binding rulebook: Prime Directives, workflow
  rules, conventions, safety boundaries. Read it first. (`AGENTS.md` points here.)
- [`specs/`](specs/README.md) — numbered, testable implementation specs
  (000–009) covering the event schema, collectors, storage, feature engine,
  backtester, strategy API + promotion funnel, execution (OMS/risk gate),
  sizing, and ops. Specs are the source of truth; docs are the design intent.
- [`.claude/skills/`](.claude/skills/) — step-by-step workflows:
  `implement-spec`, `add-strategy`, `add-venue`.

## Design docs

**Start here:**

1. [`docs/SYSTEM_BLUEPRINT.md`](docs/SYSTEM_BLUEPRINT.md) — **the full-loop
   blueprint**: research → simulation → backtesting → execution. Operating
   philosophy, where solo edge actually lives, the one-event-core/four-modes
   architecture, the strategy promotion funnel, fill-model ladder, execution
   safety (OMS, risk gate, kill switches), sizing engine, first three
   strategies, and the failure-mode wall.
2. [`docs/ARCHITECTURE_BRAINSTORM.md`](docs/ARCHITECTURE_BRAINSTORM.md) — the
   data & intelligence deep-dive: lessons extracted from Cryexc, the recorder,
   feature engine, ranked build menu, stack recommendations, and pitfalls.

## The one-paragraph pitch

Exchanges give away their most valuable data — every trade, every book update,
funding, open interest, liquidations — over free, unauthenticated WebSockets.
Almost nobody *records* it, and recorded tick/L2 history is what everyone else
pays thousands per month for. Phase 0 of this project is simply: **run a recorder
and start compounding a private dataset**. Every later layer (features, signals,
backtests, ML, LLM-generated market briefs) is built on top of that asset.
