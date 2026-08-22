# Task Plan: Serious Research Lab Roadmap

## Goal

Turn this project into a trustworthy trading-research lab: reliable data,
reproducible research, mechanical promotion gates, and no real-money trading
until evidence supports it.

## Next Step

Verify the Phase-0 recorder, scorecard, backup, and canonical-host status
against the roadmap before expanding the research universe.

## Current Phase

Phase 1 — Lab foundations

## Phases

### Phase 1: Establish non-negotiable lab controls

- [x] Define the lab objective as evidence production before trading returns.
- [x] Preserve the repository's no-live-trading and human-promotion boundary.
- [ ] Write owner-owned capital, benchmark, and maximum-loss policy.
- **Status:** in_progress

### Phase 2: Make Phase 0 trustworthy

- [ ] Verify the daily scorecard, determinism, canonical recorder, and backup
  all work continuously.
- [ ] Achieve and archive the required clean-recording streak.
- **Status:** pending

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
| Prioritize operations and data breadth over more strategy crates | The current bottleneck is corpus quality and scale, not strategy API capability. |
| Add lab-control capabilities before alpha features | Registry, feasibility, quality impact, and execution calibration make results more trustworthy. |

## Errors Encountered

| Error | Attempt | Resolution |
|---|---|---|
| None in this planning pass | 1 | Not applicable |
| Existing worktree whitespace issue | 1 | `git diff --check` reports a pre-existing blank line at EOF in `core/tests/golden_values.rs`; this plan did not modify it. |
