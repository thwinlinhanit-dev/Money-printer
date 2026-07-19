# 022 — Screener Evaluation Cadence

## Purpose
Reduce CPU waste by limiting screener rule evaluation to a configurable interval (default 1 Hz) instead of evaluating on every feature update (1000+ Hz). Snapshots still update at feature rate.

## Scope
In: `eval_interval_ns` field on `Screener`, edge-triggered evaluation logic, configurable interval, snapshot-update-always. Out: screener rule logic, feature engine, hit journal.

## Design

### Current behavior (cost)
- `Screener::on_update` called on every `FeatureUpdate` (1000+ Hz per symbol).
- Every call evaluates all rules against current snapshot state.
- ~99.9% of evaluations produce no state change — wasted CPU.

### New behavior
- `Screener::on_update` always updates the snapshot state (no change).
- Rule evaluation only occurs when `now_ns - last_eval_ns >= eval_interval_ns`.
- `last_eval_ns` is updated after each evaluation.

### Edge-triggered evaluation
- When eval fires, it compares current snapshot against the previous evaluation's snapshot.
- If a rule transitions **inactive → active**: emit `ScreenerHit` with `kind: Entry`.
- If a rule transitions **active → inactive**: emit `ScreenerHit` with `kind: Exit`.
- Rules that remain in the same state across eval boundaries do NOT re-emit.
- **First evaluation after startup**: compare against a clean baseline (no pre-existing activations). The first eval establishes the "prior" state without emitting hits for rules that happen to be active.

### Config
```rust
pub struct ScreenerConfig {
    pub eval_interval_ns: i64,     // default: 1_000_000_000 (1 second)
    pub min_interval_ns: i64,      // minimum: 100_000_000 (100ms)
}
```

## Requirements
- **FEA-11** `Screener` MUST track `last_eval_ns` and only evaluate rules when `now_ns - last_eval_ns >= eval_interval_ns`.
- **FEA-12** `on_update` MUST always update snapshots (even between evals).
- **FEA-13** Default evaluation interval MUST be 1 second (1 Hz). Configurable via `ScreenerConfig.eval_interval_ns`.
- **FEA-14** Rule transitions MUST edge-trigger: first eval after crossing threshold emits hit; subsequent evals while still active do not. Exit transitions (active → inactive) also emit a hit.
- **FEA-15** The first evaluation after construction MUST establish the baseline state without emitting any hits (no false positives from pre-existing activations).
- **FEA-16** `ScreenerConfig.min_interval_ns` (100 ms) MUST be enforced: if configured lower, silently clamp to minimum.

## Acceptance criteria
- [ ] Cadence logic implemented
- [ ] Test: `fea_11_evaluates_at_interval` — 100 updates in 1s, verify 1 evaluation
- [ ] Test: `fea_12_snapshot_always_updated` — 100 updates, verify snapshot current
- [ ] Test: `fea_13_edge_trigger_still_works` — rule transitions true at T, false at T+1s, verify one entry hit and one exit hit
- [ ] Test: `fea_14_configurable_interval` — 100ms vs 1s, verify evaluation count
- [ ] Test: `fea_15_first_eval_no_hit` — rule already active before first eval, verify no hit emitted
- [ ] Test: `fea_16_min_interval_enforced` — configure 50ms, verify clamped to 100ms
- [ ] Benchmark: measure CPU before/after at 1000 updates/sec

## Decisions
- 2026-07-19: Default: 1s (human-relevant screener, not HFT).
- 2026-07-19: Minimum: 100ms (prevents excessive CPU).
- 2026-07-19: Snapshots still update every feature: screener needs current state at eval time.

## Open questions
- None.
