# 011 — WASM Terminal (Cryexc-style) — DRAFT

## Purpose
The decision-plane endgame: a high-FPS order-flow terminal (footprints,
liquidity heatmaps, DOM, CVD) rendering *this system's* recorded and live
data — the Cryexc experience plus a private dataset no free tool has.
Status: **draft** — build in Phase 7, after the money loop works (blueprint
§12.7: the terminal is the most fun and least compounding artifact; it waits).

## Scope
In: rendering core, data protocol, views, hosting shell. Out: any trading
controls in v1 of the terminal (read-only by design — PD-1 hygiene), alerts
UI (Telegram owns alerts).

## Design (direction, to be firmed before implementation)

- **Core:** Rust + egui compiled to WASM (mirrors Cryexc's C++/ImGui choice
  while reusing this repo's language, crates, and feature code — the
  footprint aggregation that renders is the FEA code that trades).
  Immediate-mode redraw; shared ring buffers between WS ingestion and render.
- **Shell:** SvelteKit + Tailwind hosting the canvas; PWA manifest for
  mobile-install. Canvas-only UI accepted tradeoffs: no SEO/screen-reader
  support (personal tool).
- **Data sources, two planes (Cryexc Lesson 4):**
  1. Direct public exchange WebSockets from the browser (zero backend) for
     live trades/book.
  2. **History Protocol v1** (our own): a small HTTP API over the warm/cold
     stores — `GET /hist/{venue}/{symbol}/footprint?tf=&from=&to=`,
     `/heatmap`, `/cvd`, `/funding`, … returning Arrow IPC frames.
     Read-only, token-authed, served by a `histd` binary over ClickHouse/
     Parquet. Versioned like any schema (CONV-20).
- **Views v1:** footprint chart, liquidity heatmap (GPU texture path like
  Cryexc's WebGL2 experiment), DOM ladder, aggregated CVD panel, funding/OI
  strip, screener-hit markers overlaid on charts (the intelligence layer
  drawn onto price).
- **Performance budget:** 120 FPS target desktop / 60 mobile; per-frame
  allocation ≈ 0; visibility culling + dirty-flag caching for footprint
  cells; heatmap history as texture uploads, not vertices.

## Requirements (to be finalized when scheduled)
- **UI-1** Rendering core MUST reuse `features`/`core` crates for all
  aggregation (one code path extends to the eyes).
- **UI-2** Terminal MUST be read-only: no order entry, no venue keys in the
  browser, ever (v2 discussion requires owner + new spec).
- **UI-3** History Protocol MUST be versioned, read-only, token-authed, and
  documented in this spec before `histd` is built.
- **UI-4** Browser WS ingestion MUST reuse the collector normalizers via
  wasm target (COL fixtures re-run under wasm in CI).
- **UI-5** Degrade gracefully on feed loss: stale banners, never frozen
  stale-looking data (FEA-8 spirit, visually).

## Acceptance criteria
- Defined when the spec leaves draft. Placeholder targets: 60+ FPS with 4
  active views on a mid-range laptop; footprint parity test vs offline
  feature engine output (UI-1).

## Decisions
- 2026-07-10: deferred to Phase 7 deliberately; read-only v1 is a safety
  decision, not a technical one.

## Open questions
- egui vs custom wgpu renderer for the heatmap path — prototype when scheduled.
- Hosting: local-only vs authenticated public URL — owner's call (privacy).
