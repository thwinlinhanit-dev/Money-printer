# Money Printer — Full-Loop Trading System Blueprint

*v2 brainstorm: research → simulation → backtesting → execution, for one person,
with real money. Written from two chairs: the trader's (edge, survival, sizing)
and the engineer's (determinism, safety, one code path).*

Companion doc: [`ARCHITECTURE_BRAINSTORM.md`](ARCHITECTURE_BRAINSTORM.md) covers
the data/intelligence layers in depth. This doc is the whole machine.

---

## 1. Operating philosophy — the trader's rules the system must enforce

These aren't platitudes; each one becomes a *mechanical constraint* in the code.

1. **Survival precedes profit.** The system's first job is to make ruin
   impossible, not to make money. → hard portfolio kill switch, per-strategy
   max drawdown, position limits enforced *below* the strategy layer where
   strategy code cannot override them.
2. **Expectancy is the only score.** Win rate is vanity. →
   `E = p·avg_win − (1−p)·avg_loss`, tracked per strategy, after fees and
   slippage, always.
3. **You don't have an edge until the machine says so twice.** Backtest → walk
   -forward → paper → small live. Each gate is a promotion with written criteria.
   Feelings never promote a strategy; only the funnel does.
4. **Most ideas die. That's the point.** A research system's throughput is
   measured in *ideas killed per week*. Cheap, honest kills are the product.
5. **Costs are the strategy.** At retail size, fees + slippage + funding decide
   whether an edge exists at all. Model them pessimistically from day one:
   taker fees, half-spread, and a slippage haircut — then double it.
6. **Size is where legends are made or buried.** The same signal at 2x wrong
   size is a losing strategy. Sizing is its own engine (§7), never a constant
   in strategy code.
7. **Regimes flip; strategies don't feel it.** Every strategy declares the
   regime it expects; the allocator de-weights it when the regime detector
   disagrees.
8. **The market pays you for what others can't or won't do** — hold risk
   through discomfort (carry), provide liquidity in panic (cascade fading),
   watch 200 symbols at 3am (breadth). It does not pay a solo trader for being
   fast: never enter the HFT arms race.
9. **Journal or it didn't happen.** Every order, every signal, every override,
   every kill — logged with the *why*. The journal is a table, not a diary.
10. **You are running a fund of one.** Track it like a business: monthly P&L
    attribution per strategy, cost of infrastructure, and the honest benchmark
    ("would holding BTC have beaten all of this?").

---

## 2. Where a solo trader's edge actually lives (be brutally honest)

| Edge class | Solo-viable? | Why / why not |
|---|---|---|
| Speed (HFT, latency arb) | ❌ | Colocation arms race; you lose to firms by microseconds |
| **Carry & structure** (funding, basis) | ✅✅ | Publicly visible, unglamorous, capacity-limited so pros with size skip the small stuff |
| **Panic liquidity** (liq-cascade mean reversion) | ✅ | Requires stomach + automation at 3am; retail edge is *availability* |
| **Breadth** (trend/momentum across 100+ symbols) | ✅ | Decades of evidence in every asset class; boring, drawdown-heavy, works |
| **Event/flow reaction** (funding flips, OI purges, listings) | ✅ | Your recorder sees it; discretionary traders react in minutes, you in seconds |
| Cross-venue stat arb | ⚠️ | Real but execution-sensitive; transfer latency and inventory risk eat naive versions |
| Prediction (ML on price) | ⚠️ | Last, not first; only over your own recorded features with leak-proof evaluation |
| Insider/private info | ❌ | No. |

**Portfolio thesis:** run 3–6 *uncorrelated small edges* rather than one hero
strategy: e.g., carry (funding/basis) + trend breadth + cascade fading. Carry
pays in chop, trend pays in expansion, fading pays in panic — the blend is the
edge.

---

## 3. The machine — one event core, four modes

The cardinal engineering sin in trading systems is two code paths ("research
Python" vs "production bot"). The design center here is **one deterministic
event-driven core** that runs in four modes:

```
                    ┌───────────────────────────────────────────┐
                    │                EVENT CORE                 │
 events in ───────▶│  MarketEvent | SignalEvent | OrderIntent  │───────▶ effects out
                    │  Fill | PositionUpdate | RiskVerdict      │
                    └───────────────────────────────────────────┘
 MODE        events come from…            orders go to…          clock is…
 backtest    recorded Parquet replay      fill simulator          event timestamps
 paper       live WebSockets              fill simulator          wall clock
 shadow      live WebSockets              logged only (no fills)  wall clock
 live        live WebSockets              venue adapters (real)   wall clock
```

Rules that make this work:

- **Strategies are pure functions of events → intents.** No I/O, no wall-clock
  reads, no randomness without a seeded RNG. This is what makes backtests
  deterministic and replayable bug-for-bug.
- **The clock is injected.** `now()` comes from the event stream in backtest
  and the OS in live. Any strategy that calls the system clock directly fails
  code review.
- **Everything is an event, everything is logged.** The live system's event
  log *is* a backtest input: yesterday's live session can be replayed tonight
  and must produce identical decisions (the daily determinism check — the
  single highest-value test in the whole system).
- **Strategy API** (keep it brutally small):

```rust
trait Strategy {
    fn on_event(&mut self, ev: &MarketEvent, ctx: &Ctx) -> Vec<OrderIntent>;
    fn warmup(&self) -> Duration;          // history needed before live
    fn declared_regime(&self) -> Regime;   // what it expects to profit in
    fn max_gross_exposure(&self) -> f64;   // self-declared cap, risk gate may shrink
}
```

- **Intents, not orders.** Strategies emit `OrderIntent`; the risk gate (§7)
  and OMS (§6) decide what actually reaches a venue. Strategies cannot bypass
  this — architecturally, they don't hold venue credentials.

---

## 4. The strategy funnel — promotion gates with teeth

```
 IDEA ──▶ HYPOTHESIS ──▶ BACKTEST ──▶ WALK-FORWARD ──▶ PAPER/SHADOW ──▶ LIVE-SMALL ──▶ LIVE-SCALED
 (journal   (written,      (full cost    (rolling OOS,     (2–4 weeks vs     (fixed $risk,     (allocator-
  entry)     falsifiable)   model)        param stability)   live fills)       4+ weeks)         managed)
```

Written gate criteria (tune numbers, keep the *shape*):

| Gate | Promote if | Kill/demote if |
|---|---|---|
| Backtest → WF | E > 0 after 2× costs; DD acceptable; ≥ ~100 trades | Edge vanishes at realistic costs |
| WF → Paper | OOS keeps ≥ ~50% of IS performance; params stable across windows | OOS collapse, knife-edge params |
| Paper → Live-small | Paper fills within tolerance of sim fills; no operational faults | Sim/paper divergence (your fill model lies) |
| Live-small → Scale | Live tracks paper within tolerance for 4+ weeks | Live slippage exceeds model; execution errors |
| Any live strategy | — | Hits its max-DD budget → auto-demote to shadow, no human debate |

**Demotion is automatic; promotion is manual.** The system can always take
risk *off* by itself; adding risk requires a human click. That asymmetry is
the whole safety philosophy in one sentence.

Every idea gets a **hypothesis journal entry** before any code: what's the
edge, who's on the other side and why do they pay you, what regime does it
need, what kills it. Pre-registration is your defense against your own
hindsight bias — the deadliest counterparty you'll ever face.

---

## 5. Simulation & backtesting — fidelity is a dial, honesty is a constant

### Fill-model ladder (implement in order; each level unlocks strategy classes)

- **L0 — bar close ± haircut.** Fills at next bar open with slippage haircut.
  Good enough for: trend/momentum, carry, daily-horizon.
- **L1 — top-of-book replay.** Fills against recorded best bid/ask + taker
  fee. Good enough for: cascade fading, event reaction, intraday.
- **L2 — depth walk.** Market orders walk the recorded book; sized orders pay
  real impact. Needed for: anything at size, cross-venue.
- **L3 — queue-position model for makers.** Estimate fill probability from
  recorded trade flow at your price level. Needed for: passive/maker
  strategies. *Treat all maker backtests as upper bounds — even L3 lies.*

### Non-negotiable honesty rules
- **Costs**: taker fee + half-spread + slippage haircut on every fill;
  funding paid/earned on every perp position, every interval.
- **No look-ahead**: features computed strictly from events with
  `ts < decision_ts`; the event core makes violations structurally hard.
- **Purged & embargoed splits** for anything ML-flavored; plain walk-forward
  for everything else.
- **Monte Carlo on the trade sequence** (resample/reorder trades) →
  distribution of max drawdowns, not a single lucky path. Size to the 95th
  percentile drawdown, not the observed one.
- **Parameter plateau requirement**: performance must survive ±30% parameter
  wiggle. A strategy that needs `lookback=47` exactly is a curve-fit, not an edge.
- **Regime slicing**: report every backtest split by regime (trend/chop ×
  high/low vol). A strategy that only wins in one cell is a *conditional*
  strategy — fine, but the allocator must know.

### Simulation beyond backtesting
- **Shadow mode** (live data, decisions logged, no orders) is the cheapest
  truth serum — run every new strategy in shadow during its backtest phase;
  compare its live-shadow decisions to same-day replay decisions to catch
  nondeterminism early.
- **Chaos drills in paper mode**: kill the WS feed mid-position, deliver fills
  out of order, reject every 3rd order, restart the process — the strategy and
  OMS must converge to a sane state. Do this *before* live, on purpose,
  because live will do it to you by surprise.

---

## 6. Execution stack — where money actually changes hands

```
 OrderIntent → RISK GATE → OMS → Venue Adapter(s) → exchange
                  │           │        │
                  ▼           ▼        ▼
               verdict log  order state machine  reconciler (positions/balances)
```

- **Risk gate (pre-trade, dumb on purpose):** max order size, max position per
  symbol, max gross/net exposure, max daily loss, price-sanity band (reject
  orders > x% from mark), rate limits, venue allow-list. ~200 lines, zero
  dependencies on strategy code, unit-tested to death. The strategy is smart;
  the gate is paranoid.
- **OMS as an explicit state machine:** `intent → risk_checked → submitted →
  acked → partial → filled | cancelled | rejected | UNKNOWN`. The `UNKNOWN`
  state (sent, no ack, connection died) is the one that costs real money —
  handle it with: idempotent client order IDs, on-reconnect open-order query,
  and reconcile-before-resume.
- **Idempotency everywhere:** client-generated order IDs; duplicate submits
  are no-ops; crash + restart must never double a position.
- **Reconciliation loop:** every N seconds and on every reconnect, fetch venue
  positions/balances/open orders and diff against internal state. Mismatch →
  freeze new intents for that venue, alert, auto-flatten if configured.
  *Reconciliation, not order acks, is your source of truth.*
- **Kill switches, layered:** per-strategy (DD budget), per-venue (error rate,
  reconcile mismatch), global (daily loss limit, dead-man's switch if the
  recorder/feeds go silent, physical "flatten everything" command in Telegram).
- **Credential hygiene:** API keys with *trade-only* permissions (never
  withdrawal), IP-allowlisted to the VPS, stored in a secrets manager or
  encrypted file — never in the repo, never in strategy code, never readable
  by the strategy process (only the OMS process holds them).
- **Venue adapters** are the only venue-specific code: normalize order
  semantics (post-only, reduce-only, IOC), error codes, and rate-limit
  budgets. Start with **one venue** (Bybit or Hyperliquid are friendliest for
  API trading; Binance if geography allows) and add the second only after a
  month of clean reconciliation on the first.

---

## 7. Risk & sizing engine — the difference between a system and a bet

Runs *above* strategies, *below* the risk gate:

- **Vol-targeted sizing as the default:** position = (risk budget per trade) /
  (instrument volatility). Every strategy inherits it; none hardcode size.
- **Fractional Kelly as the ceiling:** estimate edge/odds from the live trade
  journal, cap allocation at **¼ Kelly** — full Kelly assumes you know your
  edge, and you don't; half-Kelly halves growth for a fraction of the
  drawdown; quarter-Kelly is what people who survive actually run.
- **Drawdown governor:** allocation multiplier decays as strategy DD
  approaches its budget (e.g. linear from 100% at 0 DD to 0% at max DD) —
  strategies bleed out gracefully instead of exploding.
- **Portfolio brain (the allocator):** weights strategies by rolling live
  expectancy × regime fit × correlation penalty (two strategies long the same
  crowded carry are one strategy). Rebalances daily; can only *shrink*
  intraday.
- **Risk-of-ruin math on the wall:** with per-trade risk `r` and edge from the
  journal, compute P(account −50%) and keep it under 1%. If capital is small,
  this forces the honest conclusion: per-trade risk 0.25–1.0%, which means the
  system's early job is *proving edge*, not printing — the compounding comes
  later, and only for survivors.

---

## 8. Research loop — the factory that feeds the funnel

- **Feature store:** every feature from the intelligence plane
  (ARCHITECTURE_BRAINSTORM §4) materialized to Parquet with strict
  `as_of` semantics, so research notebooks and live strategies read the *same
  numbers*.
- **Experiment tracker:** every backtest run stores config hash, data range,
  code version (git SHA), full metrics, and equity curve — queryable, so "did
  we already try this?" takes one SQL query, not a memory.
- **The screener hit journal is a strategy incubator:** screeners from v1 log
  hits; a weekly job grades forward returns per rule; rules with persistent
  post-hit drift graduate to hypothesis entries. This is the pipeline from
  *intelligence* to *P&L*.
- **Weekly research ritual (calendar, not vibes):** review journal, grade
  screener hits, kill or promote one thing. The system automates evidence;
  the human supplies judgment on a schedule.

---

## 9. Starting portfolio — first three strategies through the funnel

Chosen for edge-class diversity (§2), simulation friendliness (low fill-model
requirements), and use of data you're already recording:

1. **Funding-rate carry (perp basis harvesting).** Long/short perp vs hedge
   when funding is extreme; collect funding, exit on normalization. Needs only
   L0/L1 fills; edge is visible in public data; risk is squeeze during the
   hold — sized by the vol engine, hedged, small. *The "hello world" of real
   edges.*
2. **Trend/breakout breadth across 100+ perps.** Classic time-series momentum,
   weekly-horizon, vol-targeted, long and short. Needs L0 fills. Pays in
   expansions, bleeds in chop — which is exactly when carry pays. Decades of
   cross-asset evidence; the edge is discipline, which is what software is.
3. **Liquidation-cascade mean reversion.** After a detected liq cluster +
   price displacement, fade with tight structure-based stop, take profit at
   reversion bands, hard time stop. Needs L1 fills + your liq stream. Pays for
   providing liquidity in panic; the automation *is* the edge (it fires at
   3am; you don't).

Each gets a hypothesis entry, the full funnel, and a DD budget. Target state
after ~6 months: 2 of 3 alive at live-small, allocator running, and a kill
log you're weirdly proud of.

---

## 10. Build order — extend the v1 roadmap to the full loop

(Phases 0–2 from ARCHITECTURE_BRAINSTORM — recorder, dashboards/alerts,
features + screener — are prerequisites and unchanged.)

- **Phase 3 — Event core + backtester.** Event schema, replay engine, L0/L1
  fills, cost model, walk-forward harness, experiment tracker. *Validate:
  same-input replay is byte-identical; one idea honestly killed.*
- **Phase 4 — Paper + shadow.** Live feeds into the same core; fill simulator
  on live top-of-book; chaos drills. *Validate: 2 weeks paper, zero crashes,
  paper P&L within tolerance of same-period sim.*
- **Phase 5 — Execution MVP.** One venue adapter, OMS state machine, risk
  gate, reconciler, kill switches, trade-only keys. Strategy #1 (carry) goes
  live at minimum size. *Validate: 4 weeks live-small, reconciliation clean,
  live within tolerance of paper.*
- **Phase 6 — Portfolio.** Strategies #2/#3 through the funnel; allocator;
  DD governors; monthly P&L attribution report (LLM-drafted from the journal,
  human-read). *Validate: blended equity curve smoother than any component.*
- **Phase 7 — Compound.** Scale by the ¼-Kelly ceiling and capacity limits;
  new ideas enter only through the funnel; infra hardening (failover VPS,
  restore-from-backup drill).

Gate between every phase: **capital at risk may only increase after the
validation criterion is met in writing.**

---

## 11. Repo layout (target)

```
/core        event types, clock, ring buffers, event log (Rust)
/collectors  venue WS collectors → normalized events (v1 data plane)
/features    streaming feature engine (shared: live + research)
/strategies  pure Strategy impls, one dir each, with hypothesis.md
/sim         fill models L0–L3, cost model, replay engine
/funnel      walk-forward harness, experiment tracker, promotion state
/oms         order state machine, venue adapters, reconciler  [holds keys]
/risk        risk gate, sizing engine, allocator, kill switches
/ops         deploy (systemd/compose), dead-man's switch, Telegram bot
/research    notebooks, DuckDB queries, screener grading jobs
/journal     hypothesis entries, kill log, monthly reports
```

The permission boundary is physical: `/strategies` code runs in a process
with no network egress and no credentials; only `/oms` talks to venues.

---

## 12. Failure-mode wall — how systems like this actually die

1. **Doubled position after reconnect** (no idempotent IDs) → the UNKNOWN-state
   handling in §6 exists because of this exact wound, suffered by everyone.
2. **Backtest heaven, live hell** → costs modeled optimistically; maker fills
   assumed; look-ahead through feature timestamps. The funnel's
   paper-vs-sim tolerance gate is the tripwire.
3. **Slow bleed nobody notices** → no per-strategy attribution; the losing
   strategy hides inside blended P&L. Monthly attribution is not optional.
4. **The 3am manual override** → human sees a position, panics, flattens the
   one trade that would've paid for the quarter. Overrides are allowed but
   *logged and graded* — most people stop after seeing their own override
   scorecard.
5. **Key leak / withdrawal permission** → trade-only keys, IP allowlist,
   secrets never in repo. One checkbox on the exchange saves your net worth.
6. **Silent recorder death** → data gap poisons the month's research; dead-man
   alert within 5 minutes, or you find out in 11 days.
7. **Curve-fit du jour churn** → shipping a new hero strategy weekly while
   none clears the funnel. The funnel's slowness is a feature: it rate-limits
   self-deception.
8. **Scaling the lucky streak** → sizing up after 3 good weeks (sample of
   nothing). Only the allocator scales, only on rolling expectancy, only under
   the Kelly ceiling.

---

## 13. The scoreboard — run it like a fund

Monthly, one page, generated from the journal, read by the human:

- Equity curve + drawdown, blended and per strategy
- Expectancy, trade count, hit rate, avg win/loss — per strategy, after costs
- Live vs paper vs backtest tracking error (the "is the machine honest" row)
- Costs: fees, slippage vs model, funding paid/earned, infra spend
- Kills and promotions this month, with reasons
- **The benchmark row: vs holding BTC, vs T-bills.** If the system can't beat
  the lazy alternatives over a year, the honest move is to know it.

---

## 14. North star, restated

v1's north star was an institutional-feeling *research desk* for one person.
v2's is the full loop: **a small, honest, self-auditing fund of one** — a
machine that turns free market data into recorded history, history into
evidence, evidence into a portfolio of small uncorrelated edges, and edges
into compounding — while making it *architecturally impossible* to blow up in
any of the eight classic ways.

The edge isn't a secret signal. The edge is that the machine does, every day,
at 3am, without ego, what a disciplined trader knows they should do — and
keeps the receipts.
