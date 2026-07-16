---
name: implement-spec
description: Implement a numbered spec from specs/ end-to-end — read spec, derive test list, build vertical slice, satisfy acceptance criteria, update status. Use whenever the task is "implement spec NNN", "build the collector/backtester/OMS", or any feature covered by an existing spec.
---

# Implement a Spec

## Before you start
1. Read `CLAUDE.md` fully (Prime Directives PD-1..6 are binding).
2. Read the target spec in `specs/` AND spec `000-conventions.md`.
3. Read the specs it depends on (deps are listed in specs/README.md
   implementation order; e.g. 004 assumes 001's types and 003's Dataset reader).
4. Check `specs/README.md` status: if `implementing`, look for existing
   partial work (`git log --oneline`, search for the requirement prefix in
   code) — continue it, don't restart it.

## Procedure
1. **Restate requirements as a test list.** For each `<PREFIX>-<n>` in scope,
   write the test name first (`prefix_n_description`, per CONV-21). Post this
   list in your plan/commit body — it's the contract.
2. **Slice vertically (W-4).** Pick the smallest end-to-end path (e.g. for
   spec 002: one venue, one symbol, one stream, through to the event log)
   rather than scaffolding everything horizontally.
3. **Implement inside the target crate only.** Respect the dependency
   direction (CONV-3). If you need something from another spec's crate that
   doesn't exist yet, implement the minimal version IN THAT CRATE per ITS
   spec — never a local copy.
4. **Acceptance criteria → automated tests.** Every checklist item in the
   spec's Acceptance criteria section becomes at least one test. No checkbox
   without a test (PD-5: never weaken a criterion to pass).
5. **Ambiguity?** Choose the safer/simpler reading, append a dated line to
   the spec's `## Decisions` section in the same commit (W-5). Items under
   `## Open questions` are for the human — do NOT guess them; leave that part
   unimplemented and say so.
6. **Verify:** `cargo fmt --check && cargo clippy -- -D warnings && cargo test`
   (CONV-24). For collectors/oms also run the mock-venue integration tests.
7. **Update `specs/README.md`** status in the same commit (W-7).
8. **Commit** referencing requirement IDs (W-2), e.g.:
   `collectors: implement COL-1..COL-6 reconnect + normalization for bybit`.

## Definition of done (W-3)
- [ ] Every in-scope requirement has code + a test naming its ID
- [ ] Every acceptance criterion in scope has a passing automated test
- [ ] fmt/clippy/test clean; no network in tests (CONV-23)
- [ ] Spec Decisions updated for any judgment calls; status table updated
- [ ] No secrets, no live-mode enablement, no weakened limits (PD-1/2/5)

## Common traps in this repo
- Reading wall time on a decision path (use `Ctx`/`Clock` — PD-3/CONV-5).
- HashMap iteration affecting decisions (CONV-10).
- Making a strategy or feature do I/O "just for logging" — use `ctx.log`.
- Copying a struct definition instead of depending on `core` (one vocabulary).
- Implementing a venue quirk from memory — verify against `testdata/`
  fixtures or the venue's current docs; venues drift (COL spec table is a
  starting point, not gospel).
