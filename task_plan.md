# Task Plan: Serious Research Lab Roadmap

## Goal

Turn this project into a trustworthy trading-research lab: reliable data,
reproducible research, mechanical promotion gates, and no real-money trading
until evidence supports it.

## Next Step

Let the nightly pipeline run on green guardrails (fixed 2026-08-25) and collect
the remaining clean days: streak is 4/7 toward the promotion gate.

## Current Phase

Phase 2 — Make Phase 0 trustworthy

## Phases

### Phase 1: Establish non-negotiable lab controls

- [x] Define the lab objective as evidence production before trading returns.
- [x] Preserve the repository's no-live-trading and human-promotion boundary.
- [x] Write owner-owned capital, benchmark, and maximum-loss policy.
  (`docs/OWNER_POLICY.md` — BINDING 2026-08-25: conservative defaults
  ($200 live-small floor, 15%/1%/3% loss ladder, BTC benchmark) confirmed by
  the owner in writing; changes go through §6 amendments.)
- **Status:** complete

### Phase 2: Make Phase 0 trustworthy

- [x] Verify the daily scorecard, determinism, canonical recorder, and backup
  all work continuously. (2026-08-25: determinism_ok=true over 31 scorecards;
  guardrail blocker CONV-21 found and fixed — see Errors.)
- [ ] Achieve and archive the required clean-recording streak.
  (2026-08-25 deep-dive: streak is 7/7 CLEAN (08-18..08-24) — promotion is
  blocked ONLY by the burst-free-window condition (stale bursts on 08-21..24)
  plus, latently, missing determinism artifacts for 08-19/20/21 — now written.
  Bursts = HL feed stalls >15s during the venue's ~3h connection rotation;
  zero-burst days occur naturally (18/19/20). Pipeline logistics fixes
  shipped: drain core-first ordering, deferred-not-failed determinism,
  14-day self-healing backfill pass. Earliest honest promotion ≈ 09-01 if a
  fully burst-free week lands.)
- **Status:** in_progress

### Phase 3: Expand the research corpus safely

- [ ] Bootstrap labelled historical data separately from live recordings.
- [ ] Record a defined, liquid multi-symbol universe with data-quality gates.
- **Status:** pending

### Phase 4: Operate a reproducible research process

- [ ] Require pre-registered hypotheses, cost models, experiment records, and
  independent out-of-sample evaluation.
- [ ] Establish the research registry, economic feasibility gate, event-study
  library, and automatic strategy-autopsy record.
- [ ] Rank, kill, or hold each idea using written gates.
- **Status:** pending

### Phase 5: Rehearse and, only by owner decision, trade small

- [ ] Run qualified candidates through paper and shadow modes.
- [ ] Require operational and model-fidelity evidence before any live-small
  decision.
- **Status:** pending

## Decisions Made

| Decision | Rationale |
|---|---|
| Treat validated evidence as the deliverable | No strategy has passed the funnel on the present corpus. |
| Keep capital at risk at zero through research and rehearsal | ROADMAP and PD-1 require written gates and human promotion. |
| Owner policy drafted by agent, confirmed by owner | Agent proposed conservative defaults; owner reply "ok confirm" made `docs/OWNER_POLICY.md` binding (agents propose, never bind). |
| Prioritize operations and data breadth over more strategy crates | The current bottleneck is corpus quality and scale, not strategy API capability. |
| Add lab-control capabilities before alpha features | Registry, feasibility, quality impact, and execution calibration make results more trustworthy. |

## Errors Encountered

| Error | Attempt | Resolution |
|---|---|---|
| None in this planning pass | 1 | Not applicable |
| Existing worktree whitespace issue | 1 | `git diff --check` reports a pre-existing blank line at EOF in `core/tests/golden_values.rs`; this plan did not modify it. |
| Guardrails CONV-21 blocked the daily gate (no scorecards after 2026-08-21) | 1 | Root cause: IBIT/terminal/cohort work sat untracked in git and guardrails scan only tracked files; specs 040/041/042 then failed the ID-bearing-test rule. Fixed by staging all untracked sources/tests/specs, adding a real `wcg_12_*` catalog-entry test, relabeling spec 041 honestly to `implementing` (v2 frontend pending), and gitignoring `bin_local/` + runtime journal file. Guardrails green 2026-08-25. |
