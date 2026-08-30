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
| 014 | [Event Log Fsync Policy](014-event-log-fsync.md) | core/storage | 🔨 implementing (FSP-1..5 implemented in `core/src/log.rs` with all five acceptance tests `fsp_1..5` passing, 2026-08-28; remaining: `[fsync]` TOML plumbing + SIGTERM wiring in non-mp-collector binaries — collector config scope, spec 019) |
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
| 037 | [Options Greeks Computation Engine](037-options-greeks-computation.md) | options analytics | ✅ implemented |
| 038 | [IV Surface Builder & Volatility Analytics](038-iv-surface-builder.md) | options analytics | ✅ implemented |
| 039 | [Cross-Exchange Options Flow Aggregator](039-options-flow-aggregator.md) | options analytics | ✅ implemented |
| 040 | [IBIT ETF Options Integration](040-ibit-etf-integration.md) | options analytics | implemented (v1: IBI-1..10 incl. --cross-out CLI; OPRA real-time = v2) |
| 041 | [Real-Time Analytics Terminal](041-analytics-terminal.md) | frontend/UI | 🔨 implementing (v1 server surface LIVE in `termd.py`: REST + pagination (ter_8), versioned RFC6455 WS push w/ origin check + ≤500ms batching (ter_1/4), CSP nosniff headers (ter_10); tests `tests/test_ws.py`; TER-2/3/5–7/9/11–14 = Vite React SPA / charts / Auth0 frontend still to build — CONV-21 requires ID-bearing tests for every requirement before final status) |
| 042 | [Wallet Cohort Grading](042-wallet-cohort-grading.md) | on-chain analytics | ✅ implemented (cohort.rs: all four WCG-7 families — whale_ratio, net_delta.{cohort}, smart_flow.{w}, concentration — snapshot_path wiring; wcg_1..11 incl. proptest) |
| 043 | [CEX Flow Velocity Features](043-cex-flow-velocity.md) | on-chain analytics | ✅ implemented (netflow_flow.rs: cfv_1..10 incl. golden determinism, stale-address eviction `[netflow_flow.stale_after_ns]`, velocity proptest) |
| 044 | [Per-Token AI Insight Agent](044-per-token-ai-insight.md) | intelligence | ✅ implemented (insight_composer.py + `/v1/insight` + Telegram bot `telegram_bot.py`; tok_1..10 in `tests/test_insight.py`; verify_grounded handles signed + %-scaled claims) |
| 045 | [Accumulation Detector Screener Rule](045-accumulation-detector.md) | intelligence | ✅ implemented (accumulation.rs: acc_1..10 incl. offline/online golden + cooldown bar-boundary; acc_5 forward-return study in `research/tests/test_accumulation.py`; sub-signal inputs `oi_regime.rs` verified LIVE on the 08-08..08-17 corpus — 6/8 legs streaming; honest n=0 RES-4 record in `research/out/acc_study_2026-08-24.md`, blocked only on spec 034 netflow data = free Etherscan key) |
| 046 | [DeFiLlama Regime Collector](046-defillama-regime-collector.md) | data plane | 🔨 implementing (DEF-1..8; normalizer + `mp-defillama` binary + fixtures; def_1..def_7 + def_malformed tests pass; cold-store write + watermark tests pending live integration) |
| 047 | [Coinalyze Cross-Exchange Validation Collector](047-coinalyze-validation-collector.md) | data plane | 🔨 implementing (COZ-1..10; normalizer + `mp-coinalyze` binary + fixtures; coz_1..coz_9 + coz_rest + coz_5_pacer tests pass; cold-store write + watermark tests pending live integration) |
| 048 | [Cross-Asset Correlation Feature](048-cross-asset-correlation.md) | intelligence | 🔨 implementing (COR-1..7; `corr.rs` + `CorrFeature` + engine registration; cor_1..cor_7 + proptest + cor_3 wiring test pass; regime wiring: `CorrRegimeFeature` + `regime_fit_from_features` extended with corr labels; 10 pairs incl. DeFiLlama/Coinalyze; cor_regime_* tests pass) |

Status values: `draft` → `ready` (implementable) → `implementing` →
`implemented` → `superseded`. Update this table in the same commit as the work
(rule W-7).

**Recommended implementation order:** 001 → 002 → 003 → 005(L0) → 004 → 006 →
005(L1) → 008 → 007 → 009. Vertical slices beat horizontal completeness (W-4).
Newer specs (012–023) are Phase 2+ and do not block Phase 0.

**Options analytics pipeline:** 031 (recording, implemented) → 037 (Greeks) →
038 (IV surface) → 039 (flow) → 040 (IBIT) → 041 (terminal). Each layer
depends on the one before it; 037–039 can be built in parallel after 031.

**On-chain analytics pipeline:** 028 (whale positions) + 033 (wallet identity)
→ 042 (cohort grading) → 045 (accumulation detector). 034 (netflow) →
043 (flow velocity) → 045 (accumulation detector). 044 (AI insight) depends
on all of 037–040 + 042 + 043.

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
IBI (040), TER (041), WCG (042), CFV (043), TOK (044), ACC (045), DEF (046),
COZ (047), COR (048).

## How to implement a spec

Use the skill: `.claude/skills/implement-spec/SKILL.md`. Short version:
read spec → restate requirements as a test list → build the smallest vertical
slice → make acceptance criteria pass as tests → update status → commit with
requirement IDs (W-2, W-3).
