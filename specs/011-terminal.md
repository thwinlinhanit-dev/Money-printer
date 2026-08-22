# 011 — WASM Terminal (Cryexc-style) — History Protocol v1 frozen (READY)

## Purpose
The decision-plane endgame: a high-FPS order-flow terminal (footprints,
liquidity heatmaps, DOM, CVD) rendering *this system's* recorded and live
data — the Cryexc experience plus a private dataset no free tool has.
Status: **ready** — History Protocol v1 frozen so the first vertical slice
(`histd` serving read-only frames over recorded trades Parquet) can be built.
The trading controls and WASM renderer remain Phase 7 (boundary: the terminal
is the most fun and least compounding artifact, per blueprint §12.7); the
read-only data plane (Slice A/B) is built first because it compounds and needs
no money-loop risk.

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
  2. **History Protocol v1**: a small read-only HTTP API over the recorded
     stores serving Arrow IPC frames. Frozen in the §History Protocol v1
     section below — the contract `histd` must satisfy (UI-3).
- **Views v1:** footprint chart, liquidity heatmap (GPU texture path like
  Cryexc's WebGL2 experiment), DOM ladder, aggregated CVD panel, funding/OI
  strip, screener-hit markers overlaid on charts (the intelligence layer
  drawn onto price).
- **Performance budget:** 120 FPS target desktop / 60 mobile; per-frame
  allocation ≈ 0; visibility culling + dirty-flag caching for footprint
  cells; heatmap history as texture uploads, not vertices.

## History Protocol v1

The frozen wire contract for the read-only history plane (UI-3). `histd` MUST
implement exactly this before any front-end is built; the browser consumes
this spec, never the internal storage layout.

- **Transport:** HTTP/1.1 `GET` only — no request bodies, no cookies, no TLS
  in v1 (local-only transport default, §Open questions). Responses are
  well-bounded frames via fixed `Content-Length`.
- **Versioning:** URL root `/hist/v1/...`. A breaking change to any response
  shape bumps the version segment; a `schema_ver` also rides inside the Arrow
  IPC footer of every frame (CONV-20).
- **Frame format:** Apache Arrow IPC file frames
  (`Content-Type: application/vnd.apache.arrow.file`). The exposed row shapes
  mirror the read-time transforms in `storage::analytics` (`FootprintBar`,
  `FootprintBucketRow`, `OiwaSeries`) so the renderer never depends on
  internal crate structs.
- **Auth (UI-3):** single static read-only bearer token.
  - Request MUST carry `Authorization: Bearer <token>`.
  - Token is provisioned from env var `HISTD_TOKEN` at process start (CONV-17:
    secrets live outside the repo; never committed — PD-2).
  - Missing/wrong token → `401`. Compare is constant-time; the token value is
    never logged (CONV-14).
- **Endpoints v1** — all read over the recorded trades Parquet stream via the
  `Dataset` reader (STO-4):
  - `GET /hist/v1/{venue}/{symbol}/footprint?tf_ns=<n>&from_ns=<n>&to_ns=<n>[&bucket_usd=<usd>]`
    → Arrow IPC frame of `FootprintBar` rows (spec 003 §Analytics). With
    `bucket_usd`, returns the block-bucketed `FootprintBucketRow` interval ×
    price footprint/heatmap grid.
  - `GET /hist/v1/{venue}/{symbol}/cvd?tf_ns=<n>&from_ns=<n>&to_ns=<n>`
    → per-interval cumulative volume delta (Σagg-buy − Σagg-sell) from the
    same trades stream with floor-division alignment.
  - `GET /hist/v1/{venue}/{symbol}/oiwa?tf_ns=<n>&from_ns=<n>&to_ns=<n>`
    → OI-weighted funding series (`OiwaSeries`). Available once funding/OI
    streams are Parquet stored (spec 003 Decision has v1 Parquet trades-only);
    until then this endpoint returns `501` honestly — never fabricated data
    (PD-5).
- **Param rules:** `tf_ns` defaults to `60_000_000_000`; `from_ns` and `to_ns`
  MUST both be present and `from <= to` (else `400`); unknown venue/symbol or
  an empty time range → `404` on the route (unknown symbol in the symbol gate)
  or an empty frame (known symbol, no rows); unknown query params → `400`.
  Rows are returned in ascending `bucket_ts_ns`; bucketing uses
  `ts.div_euclid(tf_ns) * tf_ns` so views line up across endpoints (spec 003,
  alignment discipline).
- **Errors:** `400` malformed request, `401` missing/wrong token, `404`
  unknown route/venue/symbol, `501` known-but-not-yet-stored stream, `500`
  compute fault (ERROR-logged, CONV-14). Error bodies are JSON
  `{"error": "<code>", "detail": "<message>"}` and never echo the token.
- **Ops:** `GET /hist/v1/health` (no auth) returns `{"status":"ok","schema":"v1"}`.
  `mp-histd --check-config` validates the token presence and config and exits
  (CONV-18); `--version` embeds the git SHA.

## Requirements
- **UI-1** Rendering core MUST reuse `features`/`core` (and
  `storage::analytics` for read-time frames) crates for all aggregation — one
  code path extends to the eyes. The Slice A/B `histd` path MUST be a thin
  transport over existing aggregation, never a parallel compute reimplementation.
- **UI-2** Terminal MUST be read-only: no order entry, no venue keys in the
  browser, ever (v2 discussion requires owner + new spec).
- **UI-3** History Protocol MUST be versioned, read-only, token-authed, and
  documented in this spec before `histd` is built — §History Protocol v1 is
  that documentation; `histd` MUST implement `v1` exactly as written there.
- **UI-4** Browser WS ingestion MUST reuse the collector normalizers via a
  wasm target once a wasm build exists (COL fixtures re-run under wasm in CI).
  Not blocking on Slice A/B — the read-only `histd` plane builds first.
- **UI-5** Degrade gracefully on feed loss: stale banners, never frozen
  stale-looking data (FEA-8 spirit, visually).
- **UI-6** Any new external dependency with network access (HTTP server, WS
  client, Arrow IPC in JS) MUST be approved by the owner before use (CLAUDE.md
  safety table: new external dependency with network access → ask first).

## Acceptance criteria
- `ui_3_hist_router`: given a crafted request line, the router maps the
  versioned path exactly (`/hist/v1/...`; bare `/hist/...` → `404`).
- `ui_3_hist_auth`: missing/bad bearer token → `401`; correct token
  (constant-time compare) proceeds; the token value never appears in the error
  body or any log line.
- `ui_3_hist_param_validation`: bad `tf_ns` / missing `from_ns` or `to_ns` /
  reversed range / unknown query param → `400` with the spec'd JSON error.
- `ui_3_hist_footprint_frames`: `analytics::footprint_bars` output over a
  fixture dataset round-trips into an Arrow IPC frame that the reader decodes
  back to identical rows (parity with `mp-query footprint --json`).
- `ui_3_hist_cvd_aligns`: the CVD endpoint consumes the same `div_euclid`
  bucket grid as footprint (cross-check on a fixture — alignment discipline).
- `ui_3_hist_unknown_symbol_404`: unknown venue/symbol → `404`, never
  `200`/`500`.
- `ui_3_hist_oiwa_501_honest`: funding/OI endpoint → `501` while the stream is
  not Parquet stored (no fabricated data, PD-5).
- `ui_1_hist_no_parallel_compute`: `histd` builds aggregate frames only via
  `storage::analytics`/`features` (re-export contract, never re-implement).
- UI-4 (wasm normalizer) and UI-5 (feed-loss degrade) are non-functional gates
  defined with the renderer build; Slice A/B gates on the enabled rows above.

## Decisions
- 2026-07-10: deferred to Phase 7 deliberately; read-only v1 is a safety
  decision, not a technical one.
- 2026-08-19: **draft → ready** on the ordered approach: the read-only data
  plane (Slice A/B) is built before the WASM renderer because it compounds and
  carries zero money-loop risk. Firming decisions:
  1. History Protocol v1 = plain HTTP/1.1 `GET` + single static bearer token
     (`HISTD_TOKEN`) + Arrow IPC file frames. No TLS/cookies in v1 (local-only
     default).
  2. Slice A/B plane reads ONLY recorded Parquet via `Dataset`; it touches no
     decision-path code (UI-1/UI-2).
  3. `501` for not-yet-stored streams (PD-5 honesty) until spec 003 writes
     their Parquet (v1 Parquet is trades-only per spec 003 Decision).
  4. No browser WASM in v1 — the WASM/egui renderer remains Phase 7 (UI-4
     deferred; read-only `histd` builds first).

## Open questions
- egui vs custom wgpu renderer for the heatmap path — prototype when the WASM
  renderer is scheduled (after Slice A/B ships).
- Hosting: local-only (v1 default) vs authenticated public URL — owner's call
  (privacy).
- HTTP server transport: stdlib-socket implementation vs adding a server crate
  — needs owner approval for a new network dependency (CLAUDE.md + UI-6).
