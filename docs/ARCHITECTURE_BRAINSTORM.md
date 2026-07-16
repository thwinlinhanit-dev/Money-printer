# Trading Research & Intelligence System — Architecture Brainstorm

*Inspired by Cryexc (C++ + Dear ImGui + WASM trading terminal), pivoted from
"terminal" to "research & intelligence platform".*

---

## 1. What Cryexc actually teaches us

Cryexc is a charting terminal, but the interesting part isn't the charts — it's
five architectural decisions that transfer directly to building an intelligence
system:

### Lesson 1 — Exchange data is free at the point of use
Every major crypto venue (Binance Futures, Bybit, OKX, Hyperliquid, Coinbase,
Kraken, Lighter…) exposes **unauthenticated public WebSockets** for trades, L2
book deltas, mark price/funding, open interest, and liquidations. Cryexc
connects straight from the browser and pays nothing for live data. The
corollary for us: **the expensive thing is not access, it's retention.**
Historical tick/L2 data is what vendors charge $1k–10k/month for. A recorder
running 24/7 turns free streams into a compounding private asset.

### Lesson 2 — Immediate mode + ring buffers solve the high-frequency problem
Retained-mode UIs (React/DOM) choke on thousands of updates per second because
every tick mutates a tree. Cryexc redraws everything every frame (~120 FPS)
from **shared ring buffers** the feed handlers write into. The same pattern
applies server-side: feed handler writes to a lock-free ring buffer; consumers
(feature engine, recorder, alerter) read at their own pace. One producer, many
readers, no locks on the hot path.

### Lesson 3 — One codebase, two compile targets
Cryexc compiles the same C++ core natively (dev/desktop) and to WASM via
Emscripten (distribution). For a research system this is the killer trick:
**the exact code that computes features live is the code the backtester runs
over recorded data.** No "research Python vs production C++" drift — the
number-one silent killer of quant projects.

### Lesson 4 — Separate the free live plane from the paid history plane
Cryexc's optional HTTP backend implements a tiny "History Protocol v1" for
extended history, keeping the live app fully serverless. Same split for us:
live intelligence should work with zero infrastructure (direct WS), and the
history/storage plane is an *optional, separately-scaled* service with a thin,
versioned protocol between them.

### Lesson 5 — Performance is a budget, spent deliberately
Visibility culling, dirty flags, cached aggregates, GPU textures for heatmaps.
Translated: don't recompute a volume profile on every tick — mark it dirty and
recompute at render/consumer rate; don't ship raw L2 to consumers — ship
aggregated snapshots at a fixed cadence; put the genuinely heavy stuff
(heatmap history) in columnar/GPU-friendly layouts.

### Tradeoffs to respect (Cryexc's own list)
- Rate limits and regional blocks (e.g. Binance blocks US IPs) are real —
  design for multi-venue redundancy and proxy/VPS placement.
- ~300 MB RAM and sustained CPU for a single terminal session — a 24/7
  multi-symbol recorder needs an order of magnitude more care.
- Canvas UI sacrifices accessibility/SEO — fine for a personal tool.

---

## 2. The pivot: terminal → intelligence system

A terminal shows you the market *now*. An intelligence system also:

1. **Remembers** — records everything it sees (memory).
2. **Computes** — turns raw ticks into features and detections (perception).
3. **Reasons** — screens, backtests, ranks, and explains (brain).
4. **Speaks** — alerts, dashboards, and generated reports (voice).

Three planes, mirroring Cryexc's live/history split:

```
┌────────────────────────────────────────────────────────────────────┐
│  DECISION PLANE (voice)                                            │
│  WASM terminal · screener UI · alerts (Telegram/Discord) ·         │
│  daily LLM market brief · research notebooks                       │
├────────────────────────────────────────────────────────────────────┤
│  INTELLIGENCE PLANE (perception + brain)                           │
│  Feature engine (order flow, liquidity, derivatives, cross-venue)  │
│  Detectors (absorption, sweeps, liq cascades, divergences)         │
│  Regime models · stat research · event-driven backtester           │
├────────────────────────────────────────────────────────────────────┤
│  DATA PLANE (memory)                                               │
│  WS collectors → normalizer → ring buffers                        │
│      ├─ hot:  in-memory ring buffers (seconds–minutes)             │
│      ├─ warm: ClickHouse/QuestDB (weeks, queryable)                │
│      └─ cold: Parquet on object storage (forever, cheap)           │
└────────────────────────────────────────────────────────────────────┘
```

---

## 3. Data plane — the recorder is the whole moat

### Collectors
One process (or task) per venue, speaking that venue's WS dialect, emitting a
**normalized event schema**:

```
Event = { venue, symbol, type, exch_ts, recv_ts, seq, payload }
types: trade | book_delta | book_snapshot | funding | mark_price
       | open_interest | liquidation | index_price
```

Non-negotiables learned from everyone who's built one of these:
- **Two timestamps** (exchange + local receive) — lets you measure venue lag
  and do lead-lag research later.
- **Sequence-gap detection + automatic snapshot resync** for L2 books. A book
  with a silent gap is worse than no book.
- **Heartbeat/staleness watchdogs** per stream; reconnect with jittered
  backoff; log every disconnect (disconnect clusters are themselves a signal —
  they correlate with volatility).
- **Record raw + normalized.** Raw NDJSON/binary lets you re-normalize when a
  venue changes its schema (they do, without warning).

### Storage tiers
| Tier | Tech | Retention | Purpose |
|------|------|-----------|---------|
| Hot | lock-free ring buffers in the collector process | seconds–minutes | live features, terminal |
| Warm | ClickHouse (or QuestDB) | 2–8 weeks | ad-hoc SQL, screener backfills, dashboards |
| Cold | Parquet, partitioned `venue/symbol/date`, on S3/B2/local disk | forever | backtests, ML training |

Arrow as the in-memory interchange everywhere (C++/Rust core, Python research,
DuckDB all speak it natively).

### Sizing reality check
A top-10 symbol on Binance Futures does ~1–5M trades/day and far more book
deltas. Full L2 for ~20 symbols across 4 venues ≈ single-digit GB/day
compressed Parquet. Trivial for local disks; do it from day one. Trades-only
for 200 symbols is even cheaper — also do it from day one.

---

## 4. Intelligence plane — what to compute

### 4.1 Order-flow features (per symbol, streaming)
- **CVD (cumulative volume delta)** — per venue *and* aggregated across venues
  (aggregated CVD divergence vs. price is one of the most-watched discretionary
  signals; automating it is Tier-1 work).
- **Footprint aggregates** — bid/ask volume per price per bar; from these:
  imbalance stacks, unfinished auctions, high-volume nodes.
- **Absorption detector** — large passive volume printing at a level without
  price advancing (delta high, displacement low).
- **Sweep detector** — single aggressive order clearing multiple book levels
  across a short window.
- **Trade-size distribution shifts** — whale prints vs. dust; CDF of trade
  sizes over rolling windows vs. baseline.

### 4.2 Liquidity / book features
- Depth profiles at ±10/25/50 bps; **book imbalance**.
- **Heatmap persistence** — how long resting liquidity survives at a level
  (distinguishes real walls from spoof-and-pull).
- Pull/stack rate: liquidity added vs. cancelled near touch.
- Spread & top-of-book stability as a micro-volatility regime input.

### 4.3 Derivatives intelligence (crypto's unfair advantage — it's all public)
- **Funding rates** across venues → funding dashboard, extremes screener,
  funding-arb spread monitor (perp vs perp, perp vs spot basis).
- **Open interest** deltas joined with price: OI up + price up = new longs,
  OI down + price up = short covering, etc. (the classic 4-quadrant read).
- **Liquidation feed** → cascade detector (liq clusters within N seconds),
  estimated liquidation-level bands from OI + leverage assumptions.
- **Basis term structure** where dated futures exist.

### 4.4 Cross-venue intelligence
- **Lead-lag matrix** — which venue moves first (needs the dual timestamps
  from §3); useful both as a signal and to know which feed to trust.
- Price divergence alerts (venue A prints through venue B's book).
- Aggregated volume dominance: where is the marginal flow actually happening.

### 4.5 Higher-level brain
- **Regime detection** — realized-vol regimes, trend/chop classification
  (HMM or simple threshold ensembles; start dumb, stay honest).
- **Screener engine** — user-defined rules over the streaming features
  ("OI +5% in 1h AND funding < 0 AND aggregated CVD rising"), each hit logged
  with a snapshot for later hit-rate analysis. *Logging screener hits and
  grading them a week later is the cheapest research program that exists.*
- **Event studies** — automatic: what happens in the 30 min after a liq
  cascade / funding flip / sweep, measured over your recorded history.
- **ML (later, carefully)** — sequence models over order-flow features for
  short-horizon direction. Only after the backtester exists; microstructure ML
  without leak-proof evaluation is a machine for generating false confidence.
- **LLM research agents** — the genuinely new (2025+) layer:
  - *Daily market brief*: agent reads your own dashboards' data (funding
    extremes, OI shifts, regime state, screener hits) and writes a morning
    report. Grounded in your data, not headlines.
  - *Anomaly explainer*: when a detector fires, an agent pulls news/announcement
    feeds and drafts "what likely caused this" with citations.
  - *Narrative tracker*: cluster news/social mentions per asset over time,
    correlate narrative intensity with your flow features.

---

## 5. Decision plane — how it surfaces

1. **Alerts first, UI later.** Telegram/Discord webhooks from the screener and
   detectors deliver 80% of the value at 5% of the effort of a terminal.
2. **Dashboards** — Grafana straight on ClickHouse is the zero-effort option
   for funding/OI/CVD panels.
3. **The Cryexc-style WASM terminal** — the fun endgame: C++ (or Rust +
   egui, the ecosystem's Rust twin of Dear ImGui) core compiled to WASM,
   SvelteKit shell, direct exchange WS *plus* your own History Protocol
   backend serving recorded footprints/heatmaps that no free terminal has.
4. **Research notebooks** — DuckDB/Polars over the cold Parquet; the same
   feature code exposed to Python via bindings (pyo3 / pybind11).
5. **Generated reports** — the LLM brief lands in your inbox/Telegram at 07:00.

---

## 6. Stack recommendation

| Layer | Recommendation | Alternative | Why |
|-------|---------------|-------------|-----|
| Core language | **Rust** | C++ (Cryexc's choice) | Same perf & WASM story (wasm-bindgen), fearless concurrency for 24/7 collectors, egui ≈ Dear ImGui |
| Live transport | Direct exchange WS; **NATS** if multi-process | Redpanda/Kafka | NATS is one binary; Kafka is overkill until it isn't |
| Warm store | **ClickHouse** | QuestDB, TimescaleDB | Best-in-class for tick analytics SQL |
| Cold store | **Parquet + object storage** | — | Universal, cheap, DuckDB/Polars/Arrow native |
| Research | **Python + Polars + DuckDB** | — | Speed without leaving Arrow |
| Backtester | Event-replay engine in the core language, over recorded events | vectorized pandas | Only event-driven replay preserves microstructure truth |
| Dashboards | **Grafana** now, WASM terminal later | Streamlit | Zero code to start |
| Alerts | Telegram bot / Discord webhook | ntfy.sh | Reaches your phone |
| LLM layer | Claude API w/ tool use over your ClickHouse | — | Grounded agents, not vibes |

(If you specifically want to walk Cryexc's exact path: C++20 + Dear ImGui +
Emscripten + SvelteKit shell is proven — the post is the proof.)

---

## 7. Ranked build menu (effort → payoff)

### Tier 1 — weekends, immediate payoff
| # | Project | Why |
|---|---------|-----|
| 1 | **Tick + funding + OI + liquidation recorder** (trades for ~50 symbols, 3–4 venues → Parquet) | The moat. Everything else needs it. Start tonight. |
| 2 | **Funding/basis dashboard + extremes alert** | Pure REST/WS polling + Grafana; finds carry setups |
| 3 | **Liquidation cascade monitor → Telegram** | Simple clustering on the liq stream; high signal-to-effort |
| 4 | **Aggregated multi-venue CVD with divergence alerts** | Automates the most popular discretionary order-flow read |

### Tier 2 — weeks, compounding payoff
| # | Project | Why |
|---|---------|-----|
| 5 | **Full L2 recorder with gap-checked book reconstruction** | Unlocks liquidity features & serious backtesting |
| 6 | **Screener engine + hit journal** (rules over streaming features, every hit graded later) | Turns hunches into measured hit rates |
| 7 | **Event-replay backtester over recorded events** | The honest evaluation machine |
| 8 | **Lead-lag / cross-venue divergence matrix** | Needs your own dual-timestamped data — unbuyable |

### Tier 3 — months, the fun endgame
| # | Project | Why |
|---|---------|-----|
| 9 | **WASM terminal (Rust/egui or C++/ImGui)** rendering *your* recorded footprints & heatmaps via your History Protocol | Cryexc's UX + data no free tool has |
| 10 | **LLM daily brief + anomaly-explainer agents** grounded in your ClickHouse | Research leverage, delivered at 07:00 |
| 11 | **ML short-horizon models on order-flow features** | Only after #7 exists |

---

## 8. Phased roadmap

- **Phase 0 (now):** stand up the recorder (#1) on a cheap VPS *outside
  restricted regions* (Binance blocks US IPs). Validate: 7 days of gap-free
  Parquet.
- **Phase 1:** derivatives dashboard + first alerts (#2, #3). Validate: an
  alert you actually acted on.
- **Phase 2:** streaming feature engine + aggregated CVD + screener with hit
  journal (#4, #6). Validate: 30 days of graded screener hits.
- **Phase 3:** L2 recording + backtester (#5, #7). Validate: one strategy idea
  honestly killed by replay (a kill is a success).
- **Phase 4:** lead-lag research, WASM terminal, LLM brief (#8–#10).
- **Phase 5:** ML, only for ideas that survived Phase 3's machinery.

Each phase produces something you use daily even if the project stops there.

---

## 9. Pitfalls (write these on the wall)

1. **Gaps poison everything.** A backtest over data with silent holes is
   fiction. Gap-detect, log, and mark quality per partition from day one.
2. **Venue schema drift.** Exchanges rename fields and change semantics
   (e.g., Binance throttled its liquidation stream to max 1 msg/sec — your
   "all liquidations" feed is a *sample*). Keep raw captures; version your
   normalizer.
3. **Rate limits & geo-blocks** apply to you, not just Cryexc's users.
   Snapshot REST calls (book resync) are the usual limit-breaker — budget them.
4. **Clock discipline.** NTP-sync the recorder host or the lead-lag work is
   noise.
5. **Microstructure ML overfits by default.** Purged/embargoed splits,
   fees+slippage in every number, and a pre-registered hypothesis journal
   (the screener hit journal *is* that journal).
6. **Backtest fills are a model, not truth.** Without queue-position modeling,
   assume taker fills only; treat maker-fill backtests as upper bounds.
7. **Don't build the terminal first.** It's the most fun and the least
   compounding artifact. Recorder → alerts → features → backtests → terminal.
8. **24/7 ops is part of the system.** systemd + restart policies +
   dead-man's-switch alert ("recorder silent for 5 min") — or the moat quietly
   stops accreting on day 11.

---

## 10. Free data cheat sheet (public, no auth)

| Venue | Trades | L2 deltas | Funding/Mark | OI | Liquidations |
|-------|--------|-----------|--------------|-----|--------------|
| Binance Futures | ✅ aggTrade | ✅ depth@100ms | ✅ markPrice | REST | ✅ forceOrder (throttled ~1/s) |
| Bybit | ✅ | ✅ | ✅ | ✅ WS | ✅ |
| OKX | ✅ | ✅ books | ✅ | ✅ | ✅ |
| Hyperliquid | ✅ | ✅ | ✅ | ✅ | via fills |
| Coinbase | ✅ | ✅ level2 | spot only | — | — |
| Kraken (spot+futs) | ✅ | ✅ | ✅ (futs) | ✅ (futs) | — |

(Verify per-venue details when implementing; they drift — see pitfall #2.)

---

## 11. North star

Cryexc proves one person can ship an institutional-feeling *terminal* on free
data. The same architecture, pointed at **retention and reasoning** instead of
rendering, gets one person an institutional-feeling *research desk*: a private
tick archive, a feature engine that never sleeps, an honest backtester, and an
agent that reads it all and briefs you every morning.

The terminal shows the market. The system *studies* it.
