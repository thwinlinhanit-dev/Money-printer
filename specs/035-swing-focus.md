# 035 — Swing Trading Focus

**Status:** Implemented (SWG-1..8 all landed: strategy API + features + backtester + risk + execution + collectors contract)  
**Supersedes (in part):** `liq-fade-v1` requirements in [004-feature-engine.md](004-feature-engine.md), [005-backtester.md](005-backtester.md), [007-execution.md](007-execution.md), [008-risk-sizing.md](008-risk-sizing.md)  
**Depends on:** [000-conventions.md](000-conventions.md), [001-event-schema.md](001-event-schema.md), [002-collectors.md](002-collectors.md), [003-storage.md](003-storage.md)

## Purpose

Redirect strategy, feature, backtest, and execution requirements away from tick/L2 microstructure (HFT-horizon signals: absorption, footprint, queue-position fills) toward multi-day to multi-week swing horizons. 

This spec does not replace [002-collectors.md](002-collectors.md) or [003-storage.md](003-storage.md) wholesale — it narrows which parts of the pipeline are load-bearing for swing strategies and removes latency-sensitive requirements that swing strategies have no use for.

**Guiding principles carried over unchanged:**
- `PD-1..6` remain binding.
- No live trading enablement here.
- No wall-clock reads on decision paths.
- `Clock` stays injected.
- Crate dependency direction remains unchanged.
- No weakened test criteria.

## Scope & Non-Goals

### Scope (In)
- Data requirements narrowed to multi-day and 4h horizons.
- Bar-aggregated feature calculations (HTF regime, value area, funding, OI, cross-asset correlation, structural levels).
- Strategy API amendments (`holding_period_hint`, `rebalance_cadence`, multi-position support).
- Event-driven bar-replay backtester & walk-forward validation requirements.
- Volatility-targeted and portfolio-level risk sizing.
- Bar-close execution decision cadence.
- Formal freeze list for HFT/microstructure modules.

### Non-Goals (Out)
This spec explicitly does NOT require:
1. L2 order book reconstruction or maintenance (snapshot+diff, sequence-gap resync) as a dependency for any swing strategy.
2. Tick-level footprint / volume-at-price construction.
3. Latency, queue-position, or partial-fill-at-price-level simulation.
4. Sub-minute bar types (tick, volume, range, delta bars).
5. Absorption / exhaustion / sticky-wall-style microstructure alert types.
6. `liq-fade-v1` or any strategy whose edge thesis depends on sub-hour reaction time.

*Note:* None of the above are deleted from the codebase by this spec — they may still exist for other purposes (e.g. monitoring) — but no swing strategy, backtest, or acceptance criterion may depend on them. Treat `liq-fade-v1` as frozen, not removed, pending a separate decision on whether to keep it as a second, independently-gated system.

## Data Requirements (narrows 002-collectors)

Swing strategies MUST be able to run on only the following data types, all of which are either already collected or a light addition to existing collectors:

| Data | Granularity | Source | Status |
| :--- | :--- | :--- | :--- |
| **OHLCV** | daily, 4h | REST/kline per venue | Likely already present |
| **Funding rate** | per 8h interval, per venue | REST | Likely already present |
| **Open interest** | daily snapshot minimum, 4h preferred | REST | Check collectors |
| **Liquidation totals** | daily aggregate (not tick-level) | REST/WS aggregate | New, lightweight |
| **Cross-asset reference** (BTC, and optionally a macro proxy) | daily | REST | New, lightweight |

No new tick or L2 WebSocket subscriptions are required for this spec. Any collector work here is additive to [002-collectors.md](002-collectors.md), not a rewrite.

## Feature Engine (replaces microstructure requirements in 004-feature-engine)

MUST implement per asset per bar (daily and 4h):
- **HTF trend/regime state:** Trend direction + strength over multiple lookbacks (e.g. 20/50/100-bar), realized volatility regime (percentile vs trailing window).
- **Value-area context from bars, not ticks:** Session/weekly POC, value area high/low, single-print detection — computed from OHLCV aggregation, not footprint. This gets you the useful part of Cryexc-style TPO without any tick dependency.
- **Funding regime:** Current funding vs trailing distribution, funding extremes flag, cumulative funding cost if a position were held $N$ bars.
- **OI regime:** OI change rate, OI/volume ratio, divergence between OI trend and price trend.
- **Cross-asset beta / correlation:** Rolling correlation and beta to BTC (and macro proxy if added).
- **Structural levels:** Weekly VWAP + bands, prior day/week high-low.

MUST NOT require order book or trade-tape input for any of the above.

## Strategy API Impact (amends 006-strategy-api)

- Add `holding_period_hint: Range<Bars>` to strategy metadata — used by risk/sizing and sim, not enforced as a hard rule.
- Add `rebalance_cadence: Daily | FourHour` — replaces implicit per-tick evaluation; swing strategies evaluate on bar close only.
- Strategy API MUST support multiple concurrent open positions across assets (swing portfolios hold several positions at once, unlike a single-position scalp system) — if the current API assumes one position per strategy instance, this needs to change here, not be worked around downstream.

Promote `trend-breadth-v1` as the first strategy to run through this spec’s pipeline, since its shape already matches swing (regime + breadth are inherently multi-bar concepts). `carry-v1` is a natural second candidate given the funding-regime features above.

## Backtester / Sim (replaces HFT-sim requirements in 005-backtester)

MUST:
- Event-driven bar replay (daily/4h), not tick replay.
- Walk-forward validation with purged splits (embargo period between train/test to prevent leakage from overlapping labels).
- Funding cost applied per bar held, not just entry/exit fees.
- Slippage model MAY be a simple fixed-bps or spread-based estimate — queue-position and partial-fill-at-depth modeling is explicitly out of scope (see Non-Goals).
- Report Deflated Sharpe and multiple-testing-adjusted metrics when comparing more than one parameter set or strategy variant, per the existing EDGE validation discipline.

MUST NOT require the tick-replay engine as a dependency to produce a valid backtest report for a swing strategy.

## Risk / Sizing Engine (amends 008-risk-sizing)

MUST:
- Volatility-targeted position sizing per trade (using the vol-regime feature from §4).
- Correlation-aware portfolio-level exposure caps across concurrent swing positions — not just per-position limits, since swing runs multiple positions at once.
- Max concurrent positions parameter, enforced at the risk gate.
- Expected-return calculations account for cumulative funding cost over the expected holding period, not just spot P&L.
- Daily loss limit and max drawdown gates carry over unchanged from existing risk requirements.

## Execution / OMS (amends 007-execution)

- Decision cadence drops to bar-close (daily/4h) — no requirement for sub-second order placement latency.
- Standard limit/market order placement is sufficient; no requirement for iceberg detection, spoofing detection, or queue-jump logic.
- Order state machine and fill reconciliation requirements from [007-execution.md](007-execution.md) carry over unchanged — swing doesn’t relax correctness requirements, only latency/microstructure ones.

## Deprecation / Freeze List

The following are frozen (not deleted) pending a separate decision:
- `liq-fade-v1` strategy.
- Any tick-book reconstruction module whose only consumer is a microstructure feature or strategy.
- Footprint/absorption/sticky-wall feature types, if no non-swing consumer remains after this spec lands.

## Requirements

- **SWG-1**: MUST restrict data collection dependencies for swing strategies to daily/4h OHLCV, 8h funding rates, daily/4h open interest, daily aggregate liquidations, and daily cross-asset reference data without requiring tick or L2 WebSocket subscriptions. *(IMPLEMENTED — see Decisions 2026-08-20 slice 6; data contract added to [002-collectors.md](002-collectors.md).)*
- **SWG-2**: MUST implement bar-aggregated feature calculations (HTF trend/regime, bar-aggregated value area, funding regime and cumulative costs, OI regime, cross-asset correlation/beta, structural levels) without depending on L2 order book or trade-tape inputs. *(IMPLEMENTED — features/src/swing.rs, `swg_2_*`.)*
- **SWG-3**: MUST update strategy metadata in [006-strategy-api.md](006-strategy-api.md) to include `holding_period_hint: Range<Bars>` and `rebalance_cadence: Daily | FourHour` evaluated on bar close only. *(IMPLEMENTED — `core/src/swing.rs`, `swg_3_*`; amended for the legacy `Event` cadence, see Decisions.)*
- **SWG-4**: MUST support multiple concurrent open positions per strategy instance across assets in the Strategy API. *(IMPLEMENTED — per-symbol `Ctx::position()`, `swg_4_*`.)*
- **SWG-5**: MUST implement event-driven bar replay (daily/4h) in [005-backtester.md](005-backtester.md) with walk-forward purged splits, per-bar holding funding drag, simple fixed-bps/spread slippage modeling, and Deflated Sharpe reporting. *(IMPLEMENTED — sim `--bar-tf-ns`/`--embargo-ns`/`--bars-per-year`, `swg_5_*`.)*
- **SWG-6**: MUST enforce volatility-targeted position sizing, correlation-aware portfolio-level exposure caps, max concurrent position limits, and cumulative funding drag in expected-return calculations within [008-risk-sizing.md](008-risk-sizing.md). *(IMPLEMENTED — risk `portfolio.rs` + gate RG-12/RG-13, `swg_6_*`; spec 008 amended.)*
- **SWG-7**: MUST evaluate execution decisions on bar close (daily/4h) using standard limit/market orders while keeping order state machine and fill reconciliation requirements unchanged from [007-execution.md](007-execution.md). *(IMPLEMENTED — sim engine dispatches `Daily`/`FourHour` strategies on bar close only (features + timers deferred to the boundary, never dropped), `swg_7_*`; spec 007 amended.)*
- **SWG-8**: MUST freeze `liq-fade-v1`, tick-book reconstruction modules with no non-swing consumers, and footprint/absorption feature types pending formal deprecation review. *(IMPLEMENTED — `FROZEN_STRATEGIES`, `swg_8_*`.)*

## Acceptance Criteria (EDGE gate, swing variant)

A swing strategy MUST clear the same adversarial-review and EDGE-validation process as any other strategy, with these swing-specific bars:

- [ ] Walk-forward backtest spans multiple market regimes (minimum: one trend regime, one chop/range regime), not a single short window.
- [ ] Costs include funding drag over realistic holding periods, not just entry/exit fees.
- [ ] Out-of-sample Sharpe and Deflated Sharpe both reported; strategy does not clear the gate on in-sample or single-split results alone.
- [ ] No dependency on any component listed in Non-Goals.

These four bars are strategy-gate items — they bind the FIRST swing strategy that clears EDGE, not the spec's requirements (SWG-1..8, all implemented). The machinery they presuppose (purged walk-forward, Deflated Sharpe, funding drag, bar-only data) is in place per the requirements above.

## Decisions

- 2026-08-20: Assigned spec number 035 (`035-swing-focus.md`) as the next sequential spec following `034-exchange-netflow.md`, in accordance with the PDF's explicit renumbering instruction.
- 2026-08-20: `liq-fade-v1` frozen rather than deleted to preserve codebase history until an independent governance decision is made.
- 2026-08-20: Replaced tick-level footprint TPO requirements with bar-aggregated OHLCV approximations to eliminate tick-level dependencies for swing strategies while preserving market profile context.
- 2026-08-20 (impl): **Open Q1 resolved from code** — the `Strategy` trait and `Ctx::position(symbol)` are already per-symbol (sim's `SimCtx` keeps a `BTreeMap<SymbolId, f64>`), so the API ALREADY supports multiple concurrent positions across assets. The gap is that concrete v1 strategies (`carry_v1`, `liq_fade_v1`) collapse to a SINGLE `state` enum, so they cannot hold >1 position without a rewrite. SWG-4 is therefore an **implementation affordance**, not a new trait method.
- 2026-08-20 (impl): Shared swing types `BarRange` (closed `[min,max]` bar range) and `RebalanceCadence` live in **`core`** (`core/src/swing.rs`) — NOT `strategies` — so `sim` and `risk` can consume them without a CONV-3 violation (same pattern as `exec`/`OrderIntent`). `Strategy` gains `holding_period_bars()` and `rebalance_cadence()` as **default methods** (`[1, MAX]` / `Event`, the legacy per-tick cadence — amended from the originally-planned `Daily` default by the SWG-7 slice so the sim can distinguish per-tick v1 strategies from bar-close swing strategies), so existing strategies and the sim wiring keep compiling unchanged.
- 2026-08-20 (impl): **Slice 1 = SWG-3 + SWG-4 (strategy API) + SWG-8 (freeze marker).** `FROZEN_STRATEGIES` published from `mp_strategies`; `liq_fade_v1` documented frozen. Tests: `swg_3_*`, `swg_4_*`, `swg_8_*` in `strategies/tests/swing_focus.rs`.
- 2026-08-20 (impl): **Slice 2 = SWG-2 (feature engine, bar-only).** `realized_vol`, `trend_strength`, `value_area` (POC/high/low), and `RollingVwap` implemented as pure bar computations in `features/src/swing.rs` (`swg_2_*` unit tests) and registered as real `BarFeature`s in `engine_from_config` under the new `[swing]` section of `features.toml` (param `deny_unknown_fields` per FEA-7, params hash → FEA-6 ver). Integration test `swg_2_engine_from_config_registers_swing_bar_features` proves registration + warmup emission. All 8 swg_2 tests + the 40-feature feature_engine suite + full workspace build green. Spec status moves `ready` → `implementing` per the Decision above.
- 2026-08-20 (impl): **Slice 3 = SWG-5 (backtester/sim bars).** Event-driven bar replay for daily/4h is configurable via `--bar-tf-ns` (`sim backtest|wf|mc`; L0 bar fills already fill at next bar open — spec 005); bar-return Sharpe samples equity once per bar boundary at the same `bar_tf_ns`. Purged walk-forward splits: `WalkForwardParams.embargo_ns` / `sim wf --embargo-ns` insert a gap between train and test that is in NEITHER slice (label-leakage blocked). Per-bar holding funding drag: existing SIM-4 funding accrual + attribution (spec 005/008 audit fix-all) covers the funding drag over realistic holding periods — no new path needed. Fixed-bps/spread slippage: existing `slip_frac` (L0 next-open ± slip). Deflated Sharpe (Bailey & López de Prado) reported per window by `sim wf` (probit = Acklam rational approximation). Tests: `swg_5_*` in `sim/tests/{backtest,harness}.rs` + `metrics.rs` unit tests; full `mp-sim` suite green, clippy/fmt clean, workspace build green.
- 2026-08-20 (impl): **Slice 4 = SWG-6 (risk/sizing).** `risk/src/portfolio.rs` adds portfolio-level math: `correlation_adjusted_exposure` (`sqrt(wᵀρw)`, fail-closed on corrupt/ragged matrices), `cumulative_funding_cost`, and `expected_return_net_of_funding` (funding drag over the strategy's declared holding period). The risk gate grows RG-12 (`max_concurrent_positions` — only orders that OPEN a new symbol slot consume a breadth slot; adds/reduces never do) and RG-13 (`max_corr_adjusted_portfolio` — the caller precomputes the resulting value, gate stays dumb). `RiskLimits`/`RiskConfig`/`risk.toml` gain both parameters (defaults: 32 concurrent, corr cap > gross so the plain gross check binds first). The sim wires both inputs (breadth = distinct held symbols; corr-adjusted = resulting gross under perfect-correlation worst case — the sim has no cross-asset correlation model; live/paper callers with a matrix use `correlation_adjusted_exposure`). Tests: `swg_6_*` in `risk/tests/portfolio.rs` (13 tests incl. RG-12/13 + funding semantics + fail-closed). Full `mp-risk` suite green.
- 2026-08-20 (impl): **Slice 5 = SWG-7 (execution).** `RebalanceCadence` gains the legacy `Event` variant + `is_bar_close()`; the `Strategy` default cadence becomes `Event` so the v1 per-tick strategies keep per-event dispatch while swing strategies opt into `Daily`/`FourHour`. The sim engine gates BOTH feature dispatch and timer dispatch by bar boundary for bar-close strategies; a swing strategy's mid-bar timer is DEFERRED to the next bar boundary (never silently dropped), enabled by timer-to-strategy attribution (`pending_timers`/`queued_timers` now carry the owning strategy index). Standard market/limit orders and the fill/reconciliation machinery are unchanged. Tests: `swg_7_*` in `sim/tests/swing_execution.rs` (3 tests: bar-close feature gating for Daily+FourHour, Event cadence keeps per-event dispatch, timer deferral). Full `mp-sim`/`mp-strategies`/`mp-core` suites green (existing determinism/backtest hashes unchanged).
- 2026-08-20 (impl): **Slice 6 = SWG-1 (collectors contract, verified).** Audited the collectors crate + spec 002 against the spec 035 §3 data table: 8h funding (all venues, WS/REST) and open interest (Binance 30s REST + WS tickers) are fully collected; daily aggregate liquidations are derived offline from the collected per-event liq streams (`mp-query liq --interval-secs 86400`, spec 029 `liq.agg`); cross-asset reference = FRED daily macro (spec 030) + HIP-3 + BTC as a normal traded symbol. Daily/4h OHLCV has NO dedicated kline REST collector, but bars are derivable offline from the already-collected trade tape (`features/src/bar.rs` `BarBuilder`, `mp-query bars --interval-secs 14400|86400`) and historically via `research/panel.py` (Binance Vision archive) — so no NEW tick/L2/WebSocket subscription is required anywhere in the swing set. SWG-1 is satisfied by RESTRICTION + VERIFICATION, not new code: the contract is documented in spec 002 and no collector additions are needed.
- 2026-08-20 (decision): **Open Q2 closed — daily AND 4h bars both supported.** The collectors/features/sim pipeline carries both granularities: the bar engine derives either from the tape (`--interval-secs 86400|14400`), features register per-bar timeframe, and the sim replays at `--bar-tf-ns`. No scope-tightening to daily-only is needed; the 4h horizon stays available for strategies that want it.
- 2026-08-20 (decision): **Open Q3 closed — `liq-fade-v1` stays frozen in place** (SWG-8) pending the separate deprecation review; it is not moved to an archived crate now, so history and the existing graders (`grade_wf.py`, spec 029) keep working without a migration.
- 2026-08-20: spec 035 status → `implemented` (all SWG-1..8 landed); the four acceptance bars above remain the forward gate for the first concrete swing strategy.

## Open Questions (resolve before implementation)

- ~~Does the current strategy API already support multiple concurrent positions per strategy, or does §5 require an API change beyond metadata fields?~~ **Resolved 2026-08-20 from code** — per-symbol `Ctx::position()` + `SimCtx` BTreeMap already support it; concrete single-`state` strategies need rewrites (see Decisions).
- ~~Is 4h the right minimum bar, or is daily-only sufficient for v1 to keep scope tight?~~ **Resolved 2026-08-20** — both daily and 4h are supported end-to-end (see Decisions).
- ~~Should `liq-fade-v1` stay in the workspace at all, or move to a separate/archived crate to keep this spec's scope visibly clean?~~ **Resolved 2026-08-20** — stays frozen in place per SWG-8 until the deprecation review (see Decisions).
