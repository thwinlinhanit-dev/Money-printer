# 053 — Alpha Program (one active candidate)

## Purpose

Stop expanding the strategy surface until evidence exists. Throughput is
**ideas killed per month**, not crates merged. This spec is the research
queue the funnel already implied (006) but never enforced as a WIP limit.

## Scope

In: registry WIP limit, allowed next experiments, Pine demotion,
funding-arb data gate, swing paper coupling. Out: implementing new
alpha math before its data gate, live trading.

## Design

Work-in-progress limit **1**. `research/registry.jsonl` may contain many
rows; at most one has `state` in `{hypothesis, backtest, paper, shadow}`.
All others MUST be `killed`, `held`, or `recorded`.

**Active candidate (2026-08-31 recommendation):** `funding-arb-v1`

Why: feasibility already `CLEARS_BASE_AND_STRESSED`; the missing piece is
calendar overlap (≥3 days, FARB-2), not a new microstructure toy. Carry
is the blueprint’s strongest solo-viable edge class. liq-fade is killed.
orderflow lost at 2× costs. swing-range-reclaim is the **paper plumbing**
vehicle (051), not the alpha bet, until it has its own OOS.

If funding-arb stays NOT GRADABLE after 5 additional Bybit overlap days,
state → `held` and the next candidate is swing-range-reclaim’s first
walk-forward — not a new crate.

## Requirements

- **ALP-1** `run_registry.py check` MUST exit 1 if more than one record
  is in an active state (hypothesis|backtest|paper|shadow).
- **ALP-2** Adding a strategy crate (`strategies/src/*.rs`) without a
  registry row and `hypothesis.md` is a guardrail fail (extend
  guardrails.ps1).
- **ALP-3** `funding-arb-v1` MUST NOT enter backtest until
  `n_overlap_days >= 3` of HL vs Bybit BTC funding, documented in the
  registry `reason`. Event study FARB-2 is the gate.
- **ALP-4** Pine scripts under `strategies/pine/` MUST carry a header
  comment: `NON-EVIDENCE: not in funnel until ported to Rust sim`.
  `pine/README.md` MUST open with the same sentence. Citing those Sharpe
  numbers in weekly review is a PD-5 defect.
- **ALP-5** Porting a Pine idea requires: hypothesis.md, costs in
  feasibility.py, `sim backtest`, walk-forward, registry update. No
  “TradingView validated” shortcut.
- **ALP-6** `liq-fade-v1` stays `killed` / SWG-8 frozen. Reopening
  requires new data (multi-venue cascade tape) and a new hypothesis id
  (`liq-fade-v2`), not silent unkill.
- **ALP-7** `orderflow-v1` remains `backtest` until a walk-forward is
  journaled **or** it is killed. Default recommendation: kill if the
  next WF window is also ≤0 at 2× costs (already engaged on day 1).
- **ALP-8** Weekly review (`run_weekly_review.py`) MUST parse
  OWNER_POLICY §3 benchmark (already intended). `unset (1.1 pending)`
  after 2026-08-25 is a bug — fix in the same implementation slice.
- **ALP-9** No new collector spec for listings/quarterlies unless the
  active candidate’s `required_data` names it.

## Acceptance criteria

- [ ] `alp_1_wip_limit`
- [ ] `alp_2_crate_needs_registry` — guardrail
- [ ] `alp_3_funding_overlap_gate`
- [ ] `alp_4_pine_banner`
- [ ] `alp_6_fade_stays_killed`
- [ ] `alp_8_benchmark_not_unset` — weekly review fixture with
      OWNER_POLICY present

## Decisions

| Date | Decision |
|---|---|
| 2026-08-31 | WIP limit 1. funding-arb-v1 is the recommended active alpha; swing-range-reclaim is the paper vehicle. |
| 2026-08-31 | Pine performance tables are not lab evidence. |
| 2026-08-31 | ALP-3: overlap gate enforced via `check_overlay_days()` in registry.py; `< 3` → state stays `held`. |
| 2026-08-31 | ALP-6: verified liq-fade-v1 is `killed` in registry.jsonl — no unkill. |
| 2026-08-31 | ALP-8: benchmark parse already correct in `run_weekly_review.py`; no `unset` path in code. |
| 2026-08-31 | ALP-9: ALP-4 NON-EVIDENCE banner enforced; Pine files cite TradingView only. |

## Open questions

- Owner confirmation of the active candidate (funding-arb vs swing).
