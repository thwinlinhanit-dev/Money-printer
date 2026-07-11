# 008 — Risk & Sizing Engine

## Purpose
Convert strategy risk-unit intents into contracts, cap everything by
quarter-Kelly and drawdown governors, and allocate capital across strategies.
Sizing is where identical signals become winners or corpses.

## Scope
In: per-trade sizing, strategy allocation, DD governor, Kelly ceiling,
allocator, portfolio limits feeding the risk gate. Out: gate checks (007),
strategy logic (006).

## Design & math (normative formulas)

### Per-trade sizing (vol targeting)
Strategy emits `qty_units: RiskUnits(u)` where `u = 1.0` means "one standard
risk unit". Conversion:

```
risk_capital     = equity × alloc_weight(strategy)              (allocator, below)
per_unit_risk    = risk_capital × per_trade_risk_pct            (default 0.5%)
instrument_vol   = vol.rv.{tf}.{w} × mark_price                 (FEA catalog; $ vol per contract per bar-horizon,
                                                                 horizon matched to strategy holding period)
qty_contracts    = u × per_unit_risk / (k_stop × instrument_vol)
```
`k_stop` = strategy's stop distance in vol units (from params.toml, default 1.5).
Rounded to `step_size`, floored at `min_notional` (below floor ⇒ no trade, not
a tiny trade).

### Drawdown governor (per strategy)
```
g(dd) = clamp(1 − dd / dd_budget, 0, 1)^gamma      (gamma default 1.0, linear)
effective_alloc = alloc_weight × g(current_dd)
```
`current_dd` from live equity attribution (EXE-11 journal). At g = 0 the
strategy is auto-demoted (G5). Recovery of g follows equity recovery — no
manual bumps.

### Kelly ceiling (per strategy, from live journal only)
From rolling `kelly_window` (default 90d) of LIVE trades:
`p` = hit rate, `b` = avg_win/avg_loss ⇒ `f* = p − (1−p)/b`.
```
alloc_weight ≤ kelly_fraction × f*         (kelly_fraction default 0.25)
```
Backtest trades NEVER feed Kelly (backtests flatter). With < `min_trades`
(default 30) live trades, alloc is pinned at `alloc_floor` (default 2% of
equity) — LiveSmall exists to gather this sample.

### Allocator (daily, at 00:10 UTC)
```
raw_w(s)  = base_w(s) × regime_fit(s) × corr_penalty(s) × g(dd_s)
w(s)      = min(raw_w, kelly_cap(s)) , renormalized so Σw ≤ max_deployed (default 0.8)
```
- `base_w` = rolling live expectancy rank (positive expectancy required).
- `regime_fit` = 1.0 if current `regime.vol/trend` ∈ declared_regime, else
  `regime_penalty` (default 0.5).
- `corr_penalty` = 1/(1 + Σ positive pairwise corr of daily strategy returns
  over 60d, counted above 0.5 threshold).
- **Intraday the allocator may only shrink** (risk-off unilaterally; risk-on
  waits for the daily run) — the asymmetry principle.

### Portfolio limits (published to risk gate)
`max_gross(portfolio)`, `max_net(portfolio)`, `max_per_symbol`,
`portfolio_daily_loss_budget` — computed from equity nightly, written to a
limits file the gate hot-reloads (gate stays dumb, RSK does the thinking).

## Requirements
- **RSK-1** Sizing formula MUST be implemented exactly as above, unit-tested
  against hand-computed fixtures including rounding and min_notional floor.
- **RSK-2** DD governor MUST use live attributed equity, update on every fill
  and mark, and demote at g=0 via the funnel's auto-demote path (G5).
- **RSK-3** Kelly estimation MUST use live trades only, with the min_trades
  pin; the 0.25 fraction is config but raising it above 0.5 requires the
  `--i-am-human` interactive confirm (PD-5 spirit).
- **RSK-4** Allocator MUST run daily, journal its inputs and outputs
  (`journal/alloc.log`), and enforce shrink-only intraday.
- **RSK-5** Where Monte Carlo results exist (SIM-9), `dd_budget` for a
  strategy entering LiveSmall MUST default to `p95(maxDD_mc) × 1.25` — sized
  to the distribution, not the single observed path.
- **RSK-6** All parameters live in `risk.toml` (deny_unknown_fields); every
  change is journaled (old→new, ts, actor).
- **RSK-7** Regime mismatch de-weighting MUST read the live regime features
  (FEA catalog) — never a human's opinion field.
- **RSK-8** The engine MUST expose `explain(intent) -> SizingTrace` (every
  term of the formula with values) — journaled with each sized intent; when
  sizing surprises you, the trace answers why.
- **RSK-9** Property tests: sizing is monotonic in u; g(dd) ∈ [0,1];
  Σ final weights ≤ max_deployed; no NaN for any finite input (CONV-8/22).

## Acceptance criteria
- [x] Hand-computed sizing incl. floor/rounding + zero-vol edge (RSK-1). `rsk_1_sizing_matches_hand_computation`, `rsk_1_min_notional_floor_zeroes_tiny_trades`, `rsk_1_zero_vol_is_no_trade_not_nan`.
- [x] DD governor values at 0/half/budget/beyond (RSK-2). `rsk_2_dd_governor_shape`.
- [x] Kelly pin below min_trades, estimate above, negative-edge clamp (RSK-3). `rsk_3_kelly_cap_pins_below_min_trades_and_estimates_above`.
- [x] Allocator caps + regime penalty + renormalize; intraday shrink-only (RSK-4). `rsk_4_allocator_caps_and_renormalizes`, `rsk_4_intraday_shrink_only`.
- [x] SizingTrace complete for every sized intent (RSK-8). Asserted in RSK-1 tests.
- [x] Property suite: monotonic sizing, g∈[0,1], Σweights≤budget, finite (RSK-9). `rsk_9_*`.
- [x] MC-derived default DD budget `p95(maxDD)×1.25` (RSK-5). `rsk_5_dd_budget_defaults_to_p95_mc_times_1_25`.
- [x] `risk.toml` parses with deny_unknown_fields and journals every change old→new/ts/actor (RSK-6). `rsk_6_risk_toml_parses_rejects_unknown_keys_and_journals_changes` + `risk.toml.example`.
- [x] Regime fit computed from live regime feature values, feeding the allocator (RSK-7). `rsk_7_regime_fit_reads_live_features_not_opinion`.
- [x] SizingTrace recomposes the sizing formula term-by-term (RSK-8). `rsk_8_sizing_trace_exposes_every_term_of_the_formula`.

## Decisions
- 2026-07-10: quarter-Kelly ceiling and live-trades-only estimation are
  deliberate conservatism; revisit only with 6+ months of live attribution.
- 2026-07-10 (impl): pure functions over plain inputs (no event/journal coupling
  yet); the DD governor, Kelly, allocator, and sizer are composable and each
  fail-closed (bad/`NaN`/zero inputs → zero size or zero weight, never a NaN).
  Live-only Kelly is documented + a `trades` count gate; a compile-time
  "backtest trade type ≠ live trade type" barrier arrives with the journal
  (spec 006/007). `risk.toml` config struct, `explain()` wiring into the OMS
  journal, and MC-derived `dd_budget` (RSK-5) land when spec 007 consumes this.
- 2026-07-11 (audit): RSK-5 implemented as `dd_budget_from_mc(p95) = p95×1.25`
  (`rsk_5_*`), consuming the SIM-9 Monte-Carlo `p95_max_dd`. RSK-8's
  `SizingTrace` (already produced with every `SizedOrder`) now has an ID-named
  test proving the terms recompose the formula (`rsk_8_*`). Remaining before
  `implemented`: RSK-6 `risk.toml` parsing + change journaling, RSK-7 regime
  de-weighting from live features.
- 2026-07-11 (fix-all): RSK-6 implemented (`RiskConfig::from_toml` +
  `journal_change`, `risk.toml.example` with the owner-only defaults) and
  RSK-7 implemented (`regime_fit_from_features` maps the FEA `regime.vol`/
  `regime.trend` encodings to the declared-mask fit that feeds
  `StrategyInput::regime_fit` — never an opinion field). All requirement IDs
  tested; status → `implemented`.

## Open questions
- Correlation window (60d) vs strategy holding periods — calibrate during
  Phase 6; record the choice here.
