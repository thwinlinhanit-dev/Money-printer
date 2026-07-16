# 006 — Strategy API & Promotion Funnel

## Purpose
The contract strategy code lives under, and the gauntlet it must survive to
touch money. The API is deliberately small; the funnel is deliberately slow.

## Scope
In: Strategy trait, context, lifecycle, packaging, hypothesis template, funnel
gates + state machine, journaling. Out: sizing math (008), execution (007).

## Design

### Strategy trait (normative)

```rust
pub trait Strategy: Send {
    fn id(&self) -> StrategyId;                       // stable slug, e.g. "carry-v1"
    fn universe(&self) -> Universe;                    // symbols/venues it trades
    fn subscriptions(&self) -> Vec<FeatureId>;         // features it consumes
    fn warmup(&self) -> Duration;
    fn declared_regime(&self) -> RegimeMask;           // where it expects to profit
    fn on_feature(&mut self, u: &FeatureUpdate, ctx: &Ctx) -> Vec<OrderIntent>;
    fn on_fill(&mut self, f: &Fill, ctx: &Ctx) -> Vec<OrderIntent>;
    fn on_timer(&mut self, t: TimerId, ctx: &Ctx) -> Vec<OrderIntent>;  // via ctx.set_timer
    fn state(&self) -> serde_json::Value;              // for journaling/restart
    fn params(&self) -> &ParamSpace;                   // walk-forward grid (SIM-9)
}
```

- Strategies receive **FeatureUpdates, not raw events** (features are the
  tested vocabulary; raw-event strategies need a spec amendment).
- `Ctx` provides: `now_ns()` (injected clock), `position(symbol)`,
  `equity_allocated()`, `rng()`, `set_timer(after)`, `log(structured)`.
  Nothing else. No I/O handles, by construction (PD-3/PD-4).
- `OrderIntent { intent_id: Ulid, strategy: StrategyId, venue, symbol,
  side, kind: Market | Limit{px} | Cancel{target}, qty_units: SizeUnit,
  time_in_force: Ioc|Gtc|PostOnly, reduce_only: bool, tag: SmallString }`
  — `SizeUnit::RiskUnits(f64)` is the default: strategies express size in
  risk units; the sizing engine (008) converts to contracts. Raw-qty intents
  are allowed only for reduce-only exits.

### Packaging
`strategies/{id}/` contains: `src/` (the impl), `hypothesis.md` (REQUIRED
before code — template below), `params.toml`, `funnel.toml` (current stage +
gate evidence links), `tests/`.

### hypothesis.md template (normative headings)
```
# {id} — Hypothesis
## Edge: what inefficiency, who pays us and why do they accept the loss?
## Regime dependency: declared_regime + why
## Falsification: what result kills this idea (written BEFORE backtests)
## Expected characteristics: horizon, trade rate, hit-rate shape, capacity
## Risks: what breaks it (crowding, venue change, regime flip)
```

### Funnel state machine

```
Idea → Hypothesis → Backtest → WalkForward → Paper → LiveSmall → LiveScaled
                        │            │          │        │            │
                        └────────────┴──────────┴────────┴────────────┴──▶ Killed (terminal, with autopsy)
```

Gates (numbers are defaults in `funnel.toml`; changing them needs owner sign-off — PD-5):

| Gate | From → To | Promote criteria (ALL) | Auto-demote criteria (ANY) |
|---|---|---|---|
| **G1** | Backtest → WF | expectancy > 0 in the **2×-cost column** (SIM-8); ≥ 100 trades; maxDD ≤ declared budget; not `optimistic_maker`-dependent (SIM-12) | — |
| **G2** | WF → Paper | OOS expectancy ≥ 50% of IS; expectancy > 0 in ≥ 70% of WF windows; plateau check passes (no sign flip ±30%) | — |
| **G3** | Paper → LiveSmall | ≥ 2 weeks paper; paper expectancy within 30% of same-period sim replay; zero operational faults; owner clicks | paper/sim divergence > 50% |
| **G4** | LiveSmall → LiveScaled | ≥ 4 weeks live at fixed min risk; live expectancy within 40% of paper; slippage ≤ modeled × 1.5; reconciliation clean; owner clicks | strategy DD ≥ budget ⇒ demote to Paper (automatic) |
| **G5** | LiveScaled (ongoing) | — | DD budget hit ⇒ Paper; rolling 60d expectancy < 0 ⇒ LiveSmall; regime mask mismatch ⇒ allocator de-weights (RSK-7) |

**Demotion is automatic and instant; promotion always requires the human's
click (G3/G4).** Agents may prepare the evidence, never click the button.

## Requirements
- **STR-1** Trait, Ctx, OrderIntent MUST match the design exactly; `Ctx` MUST
  NOT expose I/O, wall time, or venue handles (compile-enforced: strategies
  crate has no such deps — CONV-3).
- **STR-2** A strategy without a completed `hypothesis.md` (all headings
  non-empty) MUST be rejected by the funnel CLI at registration.
- **STR-3** `funnel` CLI MUST implement the state machine: `funnel status`,
  `funnel promote {id}` (checks gate evidence, requires `--i-am-human` flag +
  interactive confirm for G3/G4), `funnel demote {id} --reason`, `funnel kill
  {id} --autopsy <file>`. State persists in `strategies/{id}/funnel.toml`.
- **STR-4** Gate evidence MUST link to experiment-tracker run IDs (SIM-10);
  promotion with missing/stale evidence (> 30 days) is refused.
- **STR-5** Every stage transition MUST append to `journal/funnel.log`
  (jsonl: ts, id, from, to, reason, evidence run_ids, actor human|auto).
- **STR-6** Killed strategies keep their directory forever with `AUTOPSY.md`
  (what we believed, what the data said, lesson) — the kill log is a product (W-6).
- **STR-7** Strategy unit tests MUST include: determinism (same
  feature sequence twice ⇒ identical intents), warmup silence, and
  reduce-only-on-demote behavior (receives demote signal ⇒ only reduce-only
  intents until flat).
- **STR-8** The three launch strategies (`carry-v1`, `trend-breadth-v1`,
  `liq-fade-v1` per SYSTEM_BLUEPRINT §9) each get a hypothesis.md written
  BEFORE implementation; their specs are their hypothesis files.
- **STR-9** A `NullStrategy` and a deliberately-awful `CoinFlipStrategy` MUST
  exist as fixtures; the funnel docs use CoinFlip's (failing) run as the
  worked example of a G1 kill.

## Acceptance criteria
- [x] Compile-time isolation: strategies crate has no net/oms deps (STR-1) — enforced by `ops/ci/guardrails.sh` PD-4 check + crate manifest.
- [x] Funnel full lifecycle: register(hypothesis) → G1 evidence → promote → human-gated G3 → auto-demote on DD breach → kill with autopsy (STR-3). `str_3_funnel_full_lifecycle_and_gates`.
- [x] Promotion without the human flag fails at G3/G4 (STR-3). Same test (`NeedsHuman`).
- [x] Transition journal lines are valid JSONL with the right schema (STR-5). `str_5_transitions_journal_as_jsonl`.
- [x] Null/CoinFlip fixtures exist; CoinFlip is deterministic under a seed (STR-7/9). `str_9_*`, `str_7_*`.
- [x] CoinFlip fails G1 on fixture data — the worked example of a G1 kill (STR-9). `str_9_coinflip_fails_g1_on_fixture_data`, `str_9_g1_passes_only_when_all_conditions_met`, `sim_12_g1_rejects_optimistic_maker_dependent_edge` (in `sim/tests/harness.rs`; G1 reads the backtest Metrics so it lives in `mp-sim`).
- [x] Stale evidence (> 30 days) refuses promotion; run ids land in the evidence log (STR-4). `str_4_stale_evidence_is_refused`.
- [x] Kill requires a complete Autopsy (believed / data said / lesson) and renders `AUTOPSY.md` (STR-6). `str_6_autopsy_renders_the_kill_artifact`.
- [x] `funnel` CLI: register/promote/demote/kill/show on a state file; `--i-am-human` gates G3/G4; every transition journaled; kill writes `AUTOPSY.md` (STR-3/5/6). `str_3_funnel_cli_gates_human_promotions_and_writes_autopsy` (end-to-end against the built binary).
- [x] Strategy trait + I/O-free Ctx surface pinned (STR-1). `str_1_strategy_trait_matches_design_and_ctx_exposes_no_io`.
- [x] The three launch strategies carry real hypotheses (STR-8). `str_8_launch_strategies_have_written_hypotheses`.

## Decisions
- 2026-07-10: strategies consume features only (not raw events) in v1 —
  narrows the deterministic surface and forces the feature catalog to grow
  deliberately.
- 2026-07-10 (impl): `OrderIntent`/`Fill`/`SizeUnit` live in `core::exec` (not
  oms) so strategies can emit them without a venue dependency (PD-4). `Ctx` is a
  trait exposing only now/position/equity/seeded-rng/timer/log — no I/O handle
  exists to misuse. The funnel is a library state machine (STR-3) with
  human-gated promotion + automatic demotion; the `funnel` CLI binary wrapping
  it, evidence-staleness (STR-4), and per-strategy dirs (STR-8 hypotheses
  already exist) are deferred. `state()` returns nothing yet (deferred).
- 2026-07-11 (fix-all): STR-4 implemented — `EvidenceRef {run_id,
  created_ts_ns}` replaces bare strings and `promote` refuses evidence older
  than 30 days (`EVIDENCE_MAX_AGE_NS`); STR-6 implemented — `kill` takes a
  typed `Autopsy` (all three sections non-empty) and the CLI writes
  `AUTOPSY.md`. The `funnel` binary implements STR-3's letter (hand-rolled
  argv, wall clock only at the binary edge via the sanctioned `WallClock`
  for staleness). All requirement IDs tested; status → `implemented`.

## Open questions
- Multi-strategy netting at the venue (one position, many strategies): v1
  keeps per-strategy sub-accounts virtual and nets at OMS (EXE-9); revisit if
  margin efficiency demands real netting.
