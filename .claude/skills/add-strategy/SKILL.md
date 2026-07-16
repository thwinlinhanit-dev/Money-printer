---
name: add-strategy
description: Add a new trading strategy through the required lifecycle — hypothesis.md first, then implementation against the Strategy trait, tests, and funnel registration. Use for "add a strategy", "implement carry/trend/liq-fade", or turning a screener finding into a strategy.
---

# Add a Strategy

Specs that govern this: `006-strategy-api.md` (trait, funnel), `008` (sizing —
you emit RiskUnits, never contracts), `004` (the feature vocabulary you're
allowed to consume).

## Procedure

1. **Hypothesis before code (STR-2 — the funnel rejects you otherwise).**
   Create `strategies/{id}/hypothesis.md` from the template in spec 006 §
   hypothesis.md. The hard part is the first heading: *who pays us and why do
   they accept the loss?* If you cannot answer it, stop and report that —
   an unanswerable hypothesis is a finding, not a blocker to route around.
   The **Falsification** section must be written before any backtest runs.

2. **Check the feature vocabulary.** List the features you need against the
   spec 004 catalog. Missing feature? That's a spec-004 amendment + feature
   implementation FIRST (separate commit) — strategies consume only cataloged
   features (design decision in spec 006).

3. **Scaffold** `strategies/{id}/`:
   - `src/lib.rs` — implement `Strategy` (spec 006 trait, exactly).
   - `params.toml` — every tunable, with the walk-forward grid (`ParamSpace`).
   - `funnel.toml` — stage = `hypothesis`.
   - `tests/` — see step 5.

4. **Implementation rules**
   - Pure function of (FeatureUpdates, fills, timers, params, ctx) — PD-3.
   - Size in `RiskUnits`; raw qty only for reduce-only exits (spec 006).
   - Declare `declared_regime()` honestly — the allocator uses it (RSK-7);
     "all regimes" is almost always a lie and will be caught by regime-sliced
     backtest metrics.
   - `warmup()` must cover the longest feature window you consume.
   - Handle the demote signal: reduce-only until flat (STR-7).

5. **Mandatory tests (STR-7)**
   - determinism: identical feature sequence twice ⇒ identical intents
   - warmup silence
   - reduce-only-on-demote
   - one behavioral test per hypothesis claim (e.g. "enters within N bars of
     a liq.cluster ≥ threshold")

6. **Register & backtest**: `funnel register {id}`, then run
   `sim run` / `sim wf` / `sim plateau` / `sim mc` over the longest available
   manifest-clean data range (SIM-6). Store run IDs in `funnel.toml` evidence.

7. **Report gate G1 honestly (PD-5).** Expectancy must be positive in the
   2×-cost column with ≥100 trades. If it fails: write the autopsy draft,
   recommend `funnel kill`, and stop. A clean kill with a good autopsy is a
   successful outcome of this skill — say so in your summary rather than
   tuning parameters until something passes (that's the curve-fit churn
   failure mode, SYSTEM_BLUEPRINT §12.7).

## Never
- Never promote past Backtest yourself — G3/G4 are human-click gates (STR-3).
- Never widen `dd_budget`, costs, or gate thresholds to pass (PD-5).
- Never import oms/collectors/net crates (PD-4; CI enforces, don't fight it).
