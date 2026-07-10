# 007 — Execution: Risk Gate, OMS, Venue Adapters, Reconciler

## Purpose
The only part of the system that can lose money by being wrong in a new way.
Paranoid by design: strategies propose, the gate disposes, the OMS never
forgets an order, and the reconciler trusts the venue over memory.

## Scope
In: risk gate, OMS state machine, venue trading adapters, reconciler, kill
switches, credentials, paper/shadow wiring. Out: sizing math (008), alerting
transport (009).

## Design

```
OrderIntent ─▶ SIZER (008) ─▶ RISK GATE ─▶ OMS ─▶ VenueAdapter ─▶ exchange
                                 │           │         │
                              verdict log  order store  private WS (acks/fills)
                                             │
                                        RECONCILER ◀── REST positions/orders/balances
```

Mode wiring: `live` → real adapter; `paper` → FillSimulator (spec 005) fed by
live top-of-book; `shadow` → intents logged, nothing executes. One config
knob, `mode`, guarded by PD-1 (agents never set it to `live`).

### Risk gate (stateless checks + small state, ~zero deps)
Ordered checks; first failure rejects with reason; ALL verdicts logged:

| # | Check | Default limit |
|---|---|---|
| RG-1 | mode allows orders (live/paper) | — |
| RG-2 | venue+symbol on allow-list | explicit list |
| RG-3 | order notional ≤ max_order_notional | $500 live-small |
| RG-4 | resulting |position| ≤ max_position_notional(strategy,symbol) | per funnel stage |
| RG-5 | resulting gross exposure ≤ max_gross(strategy) and ≤ max_gross(portfolio) | 1× / 3× equity |
| RG-6 | price sanity: |px − mark| ≤ max_px_dev | 2% |
| RG-7 | rate: orders/min per strategy ≤ limit | 30 |
| RG-8 | daily realized+unrealized loss(strategy) > −budget | per funnel.toml |
| RG-9 | daily loss(portfolio) > −portfolio_budget | 3% equity |
| RG-10 | kill switches: none tripped (strategy/venue/global) | — |
| RG-11 | reconciler status for venue == CLEAN | — |

Reject ⇒ intent dropped + WARN + counter. RG-8/9 breach additionally trips
the corresponding kill switch (fail-closed, one-way until human reset).

### OMS order state machine (normative)

```
        submit          ack            fill(s)
Intent ─▶ RiskChecked ─▶ Submitted ─▶ Acked ─▶ PartFilled ─▶ Filled
                            │            │        │
                            │            └─▶ CancelPending ─▶ Cancelled
                            │            └─▶ Rejected
                            └─(no ack in ack_timeout_ms, conn loss)─▶ UNKNOWN
UNKNOWN ─(query by client_order_id)─▶ Acked | Rejected | NotFound(⇒Failed)
```

- Client order IDs per CONV-19; **submit is idempotent**: resubmit with same
  id after crash MUST NOT create a second venue order (adapter uses venue
  client-id dedupe; adapters for venues lacking dedupe MUST query-before-send).
- `UNKNOWN` is a first-class state: while any order is UNKNOWN for a venue,
  new intents for that venue are frozen (RG-11 path) until resolution.
- Order store is a WAL (jsonl) — the OMS reloads it on start and resolves
  every non-terminal order before accepting new intents.

### Reconciler
Every `recon_interval_s` (default 30s) AND on every reconnect/start:
fetch open orders, positions, balances via REST; diff vs internal.
States: `CLEAN` | `DIVERGED(details)`. On DIVERGED: freeze venue (RG-11),
alert (P1), and if `auto_flatten = true` for that class of divergence
(unknown position found), submit reduce-only market flatten — config default
is alert-only; auto_flatten enablement is a human decision.

### Kill switches (one-way latches, human reset only)
- per-strategy (RG-8/G5), per-venue (error-rate > threshold, reconciler
  DIVERGED, repeated UNKNOWNs), global (RG-9, dead-man from ops 009,
  `FLATTEN` command from Telegram).
- Global trip ⇒ cancel all open orders, then (config) flatten reduce-only.

### Venue adapters (trading)
Per venue: order placement/cancel/amend semantics (post-only, reduce-only,
IOC), private WS (order updates, fills), REST for recon queries; rate-limit
budget shared with COL-4 machinery. Errors normalized to
`{Retryable, Fatal, RateLimited, InsufficientMargin, …}`.

### Credentials
Loaded from env by the oms binary only (CONV-17); keys MUST be trade-only,
withdrawal-disabled, IP-allowlisted (documented, and `oms doctor` warns if it
can't verify via venue API-key info endpoints).

## Requirements
- **EXE-1** Risk gate MUST implement checks RG-1..11 in order, stateless where
  possible, with every verdict (pass or reject) appended to `journal/verdicts.log`.
- **EXE-2** OMS MUST implement the state machine exactly; illegal transitions
  panic in debug, ERROR + freeze venue in release.
- **EXE-3** Idempotent submit MUST be proven by test: crash after send/before
  ack, restart, resubmit ⇒ exactly one venue order (mock venue, COL-14 harness
  extended with a trading mock).
- **EXE-4** UNKNOWN resolution MUST follow: query by client id → adopt venue
  truth; unresolvable after `unknown_max_s` (default 60s) ⇒ venue kill switch.
- **EXE-5** WAL recovery: kill -9 the oms at any point (property/chaos test
  over injected crash points) ⇒ restart reaches a consistent state with no
  duplicate orders and no forgotten orders.
- **EXE-6** Reconciler MUST run on interval + reconnect + start; DIVERGED
  behavior as designed; all diffs journaled.
- **EXE-7** Kill switches MUST be one-way latches requiring
  `oms reset-kill --i-am-human` (interactive confirm) to clear.
- **EXE-8** Paper mode MUST reuse the sim FillSimulator against live
  top-of-book, producing the same Fill events shape — gate G3's paper-vs-sim
  comparison depends on this being the identical code (SIM-5 principle).
- **EXE-9** Positions are tracked per (strategy, venue, symbol) virtually and
  netted per (venue, symbol) at the OMS before hitting the venue; fills are
  attributed back pro-rata to owing strategies, journaled.
- **EXE-10** `oms doctor` command: validates config, key permissions
  (trade-only), venue connectivity, clock skew vs venue (warn > 500ms), recon
  status — run at every start; refuses live mode if any check fails.
- **EXE-11** Every order, fill, verdict, recon diff, and kill event MUST be
  journaled (jsonl, schema_ver'd) — the journal is the input for G4 evidence
  and the monthly report (spec 009).
- **EXE-12** First trading venue: Bybit testnet, then Bybit live-small.
  Adapter fixture tests per COL-13 pattern for the private streams.

## Acceptance criteria
- [ ] State machine property test: random legal event sequences never reach an illegal transition; illegal injected events are rejected (EXE-2, CONV-22).
- [ ] Idempotency chaos test passes (EXE-3, EXE-5).
- [ ] UNKNOWN drill: mock venue eats an ack; order resolves by query; venue freezes on unresolvable (EXE-4).
- [ ] Reconciler divergence drill: foreign position injected in mock ⇒ freeze + alert; auto_flatten path covered by test but ships disabled (EXE-6).
- [ ] Kill switch latch + human reset flow tested (EXE-7).
- [ ] `oms doctor` fails closed on a withdrawal-enabled key in mock (EXE-10).
- [ ] End-to-end paper session on live Bybit testnet data: intent → sized → gated → simulated fill → journal, 24h without fault (EXE-8).

## Decisions
- 2026-07-10: auto_flatten ships OFF; alert-only until the owner has watched
  the reconciler behave for a month.
- 2026-07-10: venue #1 = Bybit (matches collector decision, testnet exists).

## Open questions
- Amend-vs-cancel/replace per venue: decide per adapter at implementation,
  document in the adapter's module doc.
