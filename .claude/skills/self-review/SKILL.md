---
name: self-review
description: Mandatory adversarial pass over your own diff before pushing (workflow rule W-8). Use at the end of ANY task that changed files — implementation, spec edits, docs — before commit/push and before declaring work done.
---

# Self-Review — the second pass

Read your own diff as a hostile reviewer who did not watch you work. First-
pass output is generation; this pass is criticism. They catch different bugs.

## Procedure

1. **Read the actual diff** (`git diff` / `git diff --staged`), not your
   memory of it. Line by line for code; section by section for specs/docs.

2. **Requirement audit (W-2/W-3):**
   - Every requirement ID you claim implemented: does a test name embed it?
   - Every acceptance-criterion checkbox you're about to claim: which test
     proves it? Name them explicitly in the commit body.
   - Did you update `specs/README.md` status in this same commit (W-7)?

3. **Honesty check (PD-5) — answer each, in writing, in your summary:**
   - Did I loosen any tolerance, cost, limit, timeout, or assertion to get
     green? (If yes: revert it and report the failure instead.)
   - Do any tests test the mock rather than the behavior?
   - Did I skip/ignore a failing case with a TODO? (TODOs on decision paths
     are not allowed to ship silently — surface them.)

4. **Prime Directive sweep (mechanical, do all four):**
   - `git diff --staged | grep -nE 'SystemTime::now|Instant::now'` — decision
     path? (PD-3)
   - `git diff --staged | grep -inE 'api[_-]?key|secret|token|BEGIN.*PRIVATE'`
     — anything real? (PD-2; `*.example` placeholders are fine)
   - `git diff --staged | grep -nE 'mode *= *"?live"?'` — never (PD-1)
   - New dependency added? Does it open network access from a crate that must
     not have it (PD-4/CONV-3)? New deps with network access need the human.

5. **Determinism sweep (if you touched core/features/strategies/sim):**
   HashMap iteration that reaches a decision (CONV-10)? Unseeded randomness?
   Float accumulation order changed? Wall time?

6. **The next-agent test:** could an agent with zero session context
   understand this change from the artifacts alone — commit message, spec
   Decisions entries, test names? If any judgment call you made lives only in
   your head, write it into the relevant spec's *Decisions* section now (W-5,
   Lever 4 of docs/AGENT_FORCE_MULTIPLIERS.md).

7. **Run the gates:** `ops/ci/guardrails.sh`, then `cargo fmt --check &&
   cargo clippy -- -D warnings && cargo test` (when the workspace exists).
   Paste real output in your summary — never claim green without running.

8. **Verdict, stated plainly:** either "ready: <what was verified, test
   names, gates run>" or "not ready: <what failed / what's unresolved>".
   A truthful "not ready" is a valid deliverable (PD-5).

## Anti-patterns this pass exists to catch
- The 95% task: code written, tests green, spec status table forgotten.
- The quiet tolerance bump that turns a failing fill-model test green.
- The helpful hardcode ("default to 0 if funding data missing" — SIM-4 says fail).
- The synonym: introducing `timestamp` where the schema says `_ns` fields.
- The invisible decision: resolving a spec ambiguity in code only.
