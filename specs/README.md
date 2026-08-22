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
| 011 | [WASM Terminal](011-terminal.md) | decision plane | ✅ ready |
| 012 | [Zero-Copy Event Pipeline](012-zero-copy-pipeline.md) | core | 📝 draft |
| 013 | [WS Backpressure Policy](013-ws-backpressure.md) | collectors | 📝 draft |
| 014 | [Event Log Fsync Policy](014-event-log-fsync.md) | core/storage | 📝 draft |
| 015 | [carry-v1 Strategy](015-carry-v1.md) | intelligence | 🔨 implementing |
| 016 | [Feature Materialization](016-feature-materialization.md) | intelligence | 📝 draft |
| 017 | [Screener Hit Journal & Grading](017-screener-grading.md) | intelligence | 🔨 implementing |
| 018 | [One Runtime, Four Modes (Paper/Shadow/Live Mode Switch)](018-mode-switch.md) | ops | ✅ ready |
| 019 | [Collector Binary & Systemd](019-collector-binary.md) | collectors | 📝 draft |
| 020 | [Binance REST Snapshot](020-binance-snapshot.md) | collectors | 📝 draft |
| 021 | [Bot Command Journal](021-bot-journal.md) | ops | 📝 draft |
| 022 | [Screener Evaluation Cadence](022-screener-cadence.md) | intelligence | 📝 draft |
| 023 | [String Interning in Features](023-string-interning.md) | intelligence | 📝 draft |
| 024 | [Market-Data Integrity Gate](024-market-data-integrity.md) | data plane | ✅ implemented |
| 025 | [Signal Catalog](025-signal-catalog.md) | intelligence | ✅ implemented |
| 026 | [Cross-Venue Gap Detector](026-cross-venue-gap-detector.md) | data plane | ✅ implemented |
| 027 | [Historical Bootstrap](027-historical-bootstrap.md) | data plane | ✅ implemented |
| 028 | [Hyperliquid Whale Position Collector](028-hyperliquid-whale-positions.md) | data plane | ✅ implemented |
| 029 | [Liquidation Aggregation & Estimated Liq Bands](029-liquidation-features.md) | intelligence | ✅ implemented |
| 030 | [Macro Data Collector (HIP-3 + FRED)](030-macro-collector.md) | data plane | ✅ implemented |
| 031 | [Deribit Options Recorder](031-deribit-options-recorder.md) | data plane | ✅ implemented |
| 032 | [Multi-Symbol Collector Fan-Out](032-multi-symbol-collectors.md) | data plane | 📝 draft |
| 033 | [Wallet Identity in Trades (TradeWithAddr)](033-wallet-identity.md) | data plane | ✅ implemented |
| 034 | [Exchange Netflow Indexer](034-exchange-netflow.md) | data plane | ✅ implemented |
| 035 | [Swing Trading Focus](035-swing-focus.md) | architecture/pipeline | ✅ implemented |
| 036 | [Volume-Profile Liquidity + Range-Reclaim](036-volume-profile-liquidity.md) | intelligence | ✅ implemented (MVP; §7 deferred) |
| 037 | [Options Greeks Computation Engine](037-options-greeks-computation.md) | options analytics | 📝 draft |
| 038 | [IV Surface Builder & Volatility Analytics](038-iv-surface-builder.md) | options analytics | 📝 draft |
| 039 | [Cross-Exchange Options Flow Aggregator](039-options-flow-aggregator.md) | options analytics | 📝 draft |
| 040 | [IBIT ETF Options Integration](040-ibit-etf-integration.md) | options analytics | 📝 draft |
| 041 | [Real-Time Analytics Terminal](041-analytics-terminal.md) | frontend/UI | 📝 draft |

Status values: `draft` → `ready` (implementable) → `implementing` →
`implemented` → `superseded`. Update this table in the same commit as the work
(rule W-7).

**Recommended implementation order:** 001 → 002 → 003 → 005(L0) → 004 → 006 →
005(L1) → 008 → 007 → 009. Vertical slices beat horizontal completeness (W-4).
Newer specs (012–023) are Phase 2+ and do not block Phase 0.

**Options analytics pipeline:** 031 (recording, implemented) → 037 (Greeks) →
038 (IV surface) → 039 (flow) → 040 (IBIT) → 041 (terminal). Each layer
depends on the one before it; 037–039 can be built in parallel after 031.

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
RES, UI, ZCP (012), BKP (013), FSP (014), MAT (016), GRD (017), MOD (018),
INT (024), WHL (028), MAC (030), OPT (031), BDC (001 codec amendment), MSC (032),
WAL (033), NFL (034), SWG (035), SLQ (036), GRE (037), IVS (038), OFI (039),
IBI (040), TER (041).

## How to implement a spec

Use the skill: `.claude/skills/implement-spec/SKILL.md`. Short version:
read spec → restate requirements as a test list → build the smallest vertical
slice → make acceptance criteria pass as tests → update status → commit with
requirement IDs (W-2, W-3).
