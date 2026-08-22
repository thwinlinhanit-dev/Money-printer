# 041 — Real-Time Analytics Terminal

## Purpose

Serve all computed options analytics (specs 037–040) to a real-time web
dashboard — the user-facing terminal where traders view and interact with
GEX profiles, IV surfaces, flow analytics, and cross-market signals. This
is the decision-support UI: it does NOT execute trades; it presents
analytics for human consumption.

Modeled after derivativesmonkey.com's architecture: a Vite-built React SPA
with WebSocket streaming, lazy-loaded panels, and per-asset routing.

## Scope

**In:** Frontend application (React/TypeScript), WebSocket data server,
chart rendering (GEX profiles, IV surfaces, term structure, flow tape,
heatmaps), per-asset routing, IBIT section, mobile responsiveness.

**Out:** Trading execution (spec 007/OMS), strategy management (spec 006
funnel), account management, API key management, order placement.

## Design

### Architecture

```
Feature Engine (spec 004)
  │  FeatureUpdate { feature_id, symbol, ts_ns, value, ver }
  ▼
Terminal Server (new binary in ops/)
  │  Subscribe to feature engine output (in-process or IPC)
  │  Serialize to JSON
  ├──▶ WebSocket (wss://) — live feature updates, 1s push cadence
  └──▶ REST API — historical queries, config, status
  ▼
Frontend (Vite React SPA)
  │  Connect to WebSocket for live data
  │  Fetch historical data from REST API
  ├──▶ Dashboard panels (lazy-loaded)
  ├──▶ Charts (Lightweight Charts, Plotly for 3D, Recharts for stats)
  └──▶ Mobile-responsive layout
```

### WebSocket Protocol

```json
// Server → Client: feature update
{
  "type": "feature",
  "feature_id": "gex.profile.btc",
  "symbol": "BTCUSDT",
  "ts_ns": 1785000000000000000,
  "value": 42.5,
  "metadata": {
    "strike": 100000,
    "expiry": "2026-09-26",
    "kind": "call"
  }
}

// Server → Client: batch update (efficiency)
{
  "type": "batch",
  "updates": [
    { "feature_id": "gex.profile.btc", ... },
    { "feature_id": "iv.term.btc.1m", ... }
  ],
  "ts_ns": 1785000000000000000
}

// Client → Server: subscribe to specific features
{
  "type": "subscribe",
  "features": ["gex.profile.btc", "iv.term.btc.1m", "flow.block.btc.1h"],
  "asset": "btc"
}

// Client → Server: request historical data
{
  "type": "history",
  "feature_id": "iv.term.btc.1m",
  "from_ns": 1784000000000000000,
  "to_ns": 1785000000000000000
}
```

### Push Cadence

- **GEX profile, IV surface:** on each OptionTicker batch (real-time,
  ~1s updates from Deribit)
- **Flow features:** on window close (1h/4h/24h cadence)
- **Vol regime, percentile:** on daily bar close
- **Batch updates:** server-side batching with 500ms max delay to reduce
  WebSocket frame count while maintaining near-real-time feel

### Page Structure

All analytics pages are per-asset, routed as `/{asset}/{page}`:

```
/                          → Landing page (hero, feature overview)
/btc/dashboard             → Main dashboard
/btc/tape                  → Trade tape (live)
/btc/term                  → IV term structure
/btc/iv-rv-vrp             → IV vs RV / VRP
/btc/iv-percentile         → IV percentile bands
/btc/surfaces              → 3D IV surface
/btc/greeks                → Greeks aggregation
/btc/gex-profile           → GEX profile
/btc/levels                → Key support/resistance
/btc/heatmaps              → GEX domain heatmap
/btc/thermography          → Options thermography
/btc/tail-strike           → Tail risk analysis
/btc/order-flow            → Order flow analytics
/btc/volume-flow           → Volume flow by strike/expiry
/btc/open-interest         → OI analysis
/btc/scanner               → Options scanner
/btc/basis                 → Spot-futures basis
/btc/futures/*             → Futures analytics (basis, term, funding, history, OI)
/ibit/                     → IBIT dashboard (spec 040)
/ibit/vol                  → IBIT volatility
/ibit/greeks               → IBIT Greeks
/ibit/flow                 → IBIT flow
/ibit/risk                 → IBIT risk (GEX)
/ibit/cross                → IBIT vs Deribit cross-market
/ibit/surface              → IBIT IV surface
/ibit/chart                → IBIT chart with options overlay

v2 routes (NOT in v1 scope — link out or stub): /{asset}/strategy and
/{asset}/backtest belong to the strategy funnel (spec 006) and WASM terminal
(spec 011); /{asset}/calculator is a client-side BS pricer (no server data);
/{asset}/posts needs auth + persistence. The v1 router returns a "coming in
v2" stub for these paths rather than silently 404ing.
```

### Dashboard Panel Layout

The main dashboard (`/{asset}/dashboard`) uses a grid layout with
configurable panels:

```
┌─────────────────────────────────────────────────┐
│  MetricCards: Spot | IV_ATM | VRP | OI_Total    │
├────────────────────┬────────────────────────────┤
│  GEX Profile       │  Term Structure            │
│  (horizontal bar)  │  (line chart, multi-tenor)  │
├────────────────────┼────────────────────────────┤
│  Activity Donut    │  Block RFQ Feed             │
│  (call/put ratio)  │  (recent large trades)      │
├────────────────────┼────────────────────────────┤
│  Trade Tape        │  Source Status               │
│  (live scrolling)  │  (exchange health)           │
└────────────────────┴────────────────────────────┘
```

Panels are lazy-loaded (code-split per Vite `import()`). Each panel
independently subscribes to the WebSocket features it needs. Panels can be
resized and reordered (persisted to localStorage).

### Chart Components

| Component | Library | Used For |
|---|---|---|
| GEX Profile | Lightweight Charts (histogram) | Horizontal bar chart, strike on Y, GEX on X |
| IV Surface | Plotly.js (3D surface) | 3D surface: strike × expiry × IV |
| Term Structure | Recharts (line) | ATM IV across tenors, multi-line |
| Trade Tape | Custom (virtual scroll) | Live scrolling trade list |
| Heatmaps | Canvas (custom) | GEX domain heatmap, OI concentration |
| Flow Bubble | Recharts (scatter) | Trade flow bubble chart |
| Greeks Table | Responsive Table | Net Greeks by expiry/strike |
| Strategy Cards | Custom | Strategy payoff charts |

### Performance Requirements

- **WebSocket latency:** < 500ms end-to-end from feature emission to browser
  render (p95, includes the 500ms server-side batch delay); the server-side
  push alone targets < 100ms from feature update to WebSocket frame write.
  Acceptance `ter_1` measures the server-side budget.
- **Chart render:** < 200ms for GEX profile, < 500ms for 3D surface
- **Memory:** < 200MB for the full dashboard (lazy-load offscreen panels)
- **Mobile:** fully responsive; core metrics + tape visible on 375px width
- **Offline:** graceful degradation — show last known data with staleness
  indicator when WebSocket disconnects

### Authentication

- Login via Auth0 (same pattern as derivativesmonkey.com)
- JWT token passed in WebSocket handshake and REST headers
- v1: auth is optional (analytics are free/public). Login enables:
  - Custom dashboard layouts (persisted server-side)
  - Alert configuration (spec 009 ops integration)
  - Research post history

### OG Image Generation

Dynamic Open Graph images via `/api/og?asset=btc` for social sharing:
- Render a summary card with spot price, IV level, net GEX
- PNG 1200×630, server-side rendered (e.g. Satori/Resvg)

## Requirements

- **TER-1** The terminal server MUST subscribe to feature engine output
  and push updates to connected WebSocket clients within 500ms of feature
  emission. The push MUST be batched (max 500ms delay) to reduce frame
  count without perceptible lag.

- **TER-2** The frontend MUST be a Vite-built React SPA with lazy-loaded
  route components. The initial bundle MUST be < 200KB gzipped; each
  route chunk < 50KB gzipped. Code splitting via dynamic `import()`.

- **TER-3** Per-asset routing MUST be `/{asset}/{page}` where asset ∈
  {btc, eth, sol, avax, xrp, zec} and page is one of the defined pages.
  The root `/` routes to the landing page; `/ibit/*` routes to the IBIT
  section (spec 040).

- **TER-4** WebSocket protocol MUST support: subscribe (feature filter),
  batch updates, history requests, and reconnection with jittered backoff.
  The protocol MUST be versioned (`ws_protocol_version` in handshake)
  to allow rolling upgrades.

- **TER-5** Charts MUST render within the performance budgets (TER-1
  latency targets). 3D IV surface MUST use Plotly.js (or equivalent WebGL
  renderer) for smooth rotation at 60fps on desktop. Canvas-based heatmaps
  MUST use requestAnimationFrame for smooth updates.

- **TER-6** The dashboard MUST be responsive: desktop (≥1024px) shows the
  full grid layout; tablet (768–1023px) shows 2-column; mobile (<768px)
  shows single-column stacked. Core metrics (spot, IV, VRP) and trade
  tape MUST be visible on mobile without scrolling.

- **TER-7** Staleness indicator: when WebSocket disconnects, the UI MUST
  show a visible staleness banner with last-update timestamp. Data shown
  MUST NOT be silently stale (the derivativesmonkey.com pattern: "No lag"
  promise requires honest staleness reporting).

- **TER-8** Historical data requests MUST be served from the REST API,
  reading from the feature store (spec 003/016 materialized Parquet).
  The REST API MUST support pagination and date-range queries.

- **TER-9** Panel configuration (resize, reorder, show/hide) MUST be
  persisted to localStorage (per-asset). v2: server-side persistence
  for authenticated users.

- **TER-10** Content Security Policy MUST be strict: no `unsafe-eval`,
  frame-ancestors set via HTTP header, WebSocket origin validation.
  Same CSP pattern as derivativesmonkey.com (source-reviewed).

- **TER-11** Analytics (PostHog, GA4) MUST be gated on hostname
  (production only, not dev/staging). Same pattern as
  derivativesmonkey.com's `__DM_ANALYTICS_ENABLED__`.

- **TER-12** Mobile touch: all interactive elements (chart zoom, panel
  resize, table scroll) MUST work with touch gestures. No hover-only
  interactions on mobile.

- **TER-13** Accessibility: keyboard navigation for all panels; screen
  reader labels for chart data; high-contrast mode toggle.

- **TER-14** Performance monitoring: the terminal MUST report render
  timings and WebSocket latency to Sentry (spec 009) for production
  monitoring. Alert on: WebSocket disconnect > 30s, render > 500ms,
  memory > 300MB.

## Acceptance criteria

- [ ] `ter_1_ws_push_latency_under_500ms` — measure time from feature
  emission to WebSocket frame delivery in integration test.
- [ ] `ter_2_initial_bundle_under_200kb` — `vite build --mode production`
  output analyzed; initial chunk ≤ 200KB gzipped. Plotly.js (~1MB+ gzipped)
  MUST appear ONLY in the lazy `/btc/surfaces` chunk — the acceptance fails
  if Plotly lands in the initial chunk (guard: someone imports it in the
  shell).
- [ ] `ter_3_routing_covers_all_assets` — test each `/{asset}/dashboard`
  resolves without 404.
- [ ] `ter_4_ws_reconnection` — kill server, restart, verify client
  reconnects within 5s with jittered backoff.
- [ ] `ter_6_responsive_layout` — screenshot at 375px, 768px, 1024px;
  core metrics visible at each breakpoint.
- [ ] `ter_7_staleness_banner` — disconnect WebSocket, verify banner
  appears within 2s with correct timestamp.
- [ ] `ter_8_rest_history_pagination` — request 1000 data points, verify
  paginated response with correct total count.
- [ ] `ter_9_panel_persistence` — resize a panel, reload page, verify
  the size is restored.
- [ ] `ter_10_csp_no_unsafe_eval` — verify CSP header does not contain
  `unsafe-eval`.
- [ ] `ter_11_analytics_disabled_in_dev` — verify `__DM_ANALYTICS_ENABLED__`
  is false on localhost.
- [ ] `ter_14_sentry_render_timing` — verify Sentry capture of render
  performance metrics in production.
- [ ] `ter_12_mobile_touch` — every interactive element (chart zoom, panel
  resize, table scroll) is operable via touch; no hover-only control exists
  (manual checklist on a 375px device/emulator).
- [ ] `ter_13_accessibility` — keyboard traversal reaches every panel and its
  controls; charts expose screen-reader labels; high-contrast toggle renders
  (axe-core scan + manual pass).

## Decisions

- 2026-08-22: New spec. This is the user-facing layer for all options
  analytics (specs 037–040). Without a terminal, the computed features
  exist only as Parquet files and feature engine outputs — the terminal
  makes them actionable for human traders.

- 2026-08-22: React + Vite (not WASM terminal from spec 011). The WASM
  terminal (spec 011) is for the STRATEGY pipeline (backtester, sim,
  strategy management). The analytics terminal is a SEPARATE application:
  it consumes pre-computed features, does not run the feature engine or
  strategy engine in the browser. Different deployment, different stack.

- 2026-08-22: WebSocket for live data, REST for historical. The WebSocket
  carries incremental updates (efficient, real-time); REST carries bulk
  historical queries (cached, paginated). This is the standard pattern
  for financial terminals (TradingView, derivativesmonkey.com).

- 2026-08-22: Auth0 for authentication (same as derivativesmonkey.com).
  Analytics are free/public in v1; auth enables personalization and
  alerting. The auth worker (`/auth0-worker.js`) handles token refresh
  in a Web Worker to avoid main-thread blocking.

- 2026-08-22: 3D IV surface uses Plotly.js (WebGL), not Three.js.
  Plotly is purpose-built for scientific visualization with built-in
  rotation, zoom, and hover tooltips. Three.js would require custom
  shader work for the same UX. The splash screen animation (if any)
  can use Three.js separately.

- 2026-08-22: Panel-based dashboard (not a single monolithic page) because
  different traders care about different panels. The grid layout with
  configurable panels is the derivativesmonkey.com pattern and works well
  for financial data density.

- 2026-08-22: Separate from the WASM terminal (spec 011). The two
  terminals serve different users:
  - WASM terminal: quant/developer — runs backtests, manages strategies,
    inspects feature pipelines
  - Analytics terminal: trader/researcher — views market analytics,
    monitors flow, researches vol regimes
  They share the feature store (Parquet) but have separate UI stacks
  and deployment targets.

## Open questions

- Should the terminal server run as part of the ops binary (spec 009)
  or as a separate binary? The ops binary already handles alerting and
  monitoring; adding a WebSocket server is a natural extension. But the
  WebSocket server has different resource characteristics (long-lived
  connections, memory per client). Defer to implementation.
- Deployment: Vercel (like derivativesmonkey.com) vs self-hosted?
  The terminal is a static SPA + WebSocket server. Vercel handles the
  SPA; the WebSocket server needs a persistent process (Vercel does not
  support WebSockets natively — needs a separate host or a Vercel
  alternative). Decision: SPA on Vercel, WS server on the existing VPS.
- Mobile: should the mobile version be a simplified view (core metrics
  only) or the full dashboard? Derivativesmonkey.com serves the full
  dashboard on mobile with responsive layout. Follow that pattern;
  a simplified mobile view loses information density.
- Social sharing: should the OG image be dynamic (real-time spot/IV) or
  static? Dynamic is better for engagement (each share shows current
  market state) but requires server-side rendering. Derivativesmonkey.com
  does dynamic (`/api/og?asset=btc`). Follow that pattern.
