# Agent Force Multipliers — how this repo makes any agent strong

## The honest premise

You cannot upload intelligence into an agent. What you *can* do is change the
environment so that (a) less intelligence is required per task, and (b) the
intelligence applied compounds instead of evaporating. In practice:

```
effective power = raw capability × context quality × feedback speed × guardrail strength
```

Raw capability is the only factor you don't control — and it's the one with
the *least* leverage, because the other three multiply it. A mediocre agent
inside a repo with honest tests, layered context, and mechanical guardrails
will beat a brilliant agent in a repo of tribal knowledge and manual checks.
This document is the playbook for the three factors we control, and the
status of each in this repo.

---

## Lever 1 — Verifiable ground truth (feedback speed)

**Principle:** an agent is exactly as good as its ability to check its own
work. Every claim an agent makes should be checkable by running something,
and the loop should be seconds–minutes, not "looks right to me".

**In this repo:**
- Every spec requirement has an ID; every acceptance criterion maps to a test
  whose name embeds that ID (CONV-21). "Done" is a test list, not a vibe.
- Golden fixtures: the determinism check (CONV-12/SIM-14) — replay the same
  events, demand byte-identical decisions. This turns the subtlest bug class
  (nondeterminism) into a red/green signal any agent can act on.
- `sim replay-live` (SIM-11) turns "does live match backtest?" — normally a
  debate — into a diff.
- Property tests required for the treacherous parts (serialization, book
  reconstruction, OMS transitions, sizing math — CONV-22): they find the cases
  the agent didn't think of, which is precisely the failure mode of any
  bounded intelligence.

**Rule of thumb when extending the repo:** before building a feature, build
the way to check it. A fixture is a prompt the codebase writes for the agent.

## Lever 2 — Mechanical guardrails (make errors impossible, not forbidden)

**Principle:** any rule that lives only in documentation depends on the
agent's memory and obedience. Any rule enforced by a compiler, CI script, or
directory structure costs zero intelligence to follow. Convert rules from
prose to mechanism whenever possible.

**In this repo:**
- Strategies physically cannot do venue I/O: the crate has no network
  dependency (CONV-3), the `Ctx` API exposes none (STR-1). PD-4 is a compile
  error, not a request.
- CI guardrails (`ops/ci/guardrails.sh`) grep-enforce: no wall-clock calls
  outside allowed crates (PD-3), no secret patterns or `.env` files (PD-2),
  no `mode = "live"` in checked-in config (PD-1), spec index consistency (W-7).
- Promotion to live requires an interactive human confirmation (STR-3) — the
  agent *cannot* click it. The safety asymmetry (auto-demote, human-promote)
  is workflow, not willpower.
- Fail-closed defaults everywhere: NaN suppresses the signal (CONV-8), gapped
  book silences features (FEA-8), low-coverage data fails the run (SIM-6).
  An agent that forgets an edge case gets safety, not corruption.

**Rule of thumb:** every time an agent (or human) makes a mistake that a
machine could have caught, add the machine check in the same PR as the fix.

## Lever 3 — Layered, addressable context (context quality)

**Principle:** agents fail less from stupidity than from missing or drowned
context. The fix is layering (read cost proportional to task size), stable
names (addressability), and zero synonyms.

**In this repo:**
- Four layers, each pointing down: `CLAUDE.md` (rules, 5 min) → `specs/`
  (what, per-task) → `.claude/skills/` (how, per-task-type) → code.
  An agent implementing spec 004 never needs to read spec 007.
- Everything addressable by ID: requirements (`COL-7`), gates (`G3`), checks
  (`RG-6`). IDs appear in commits and test names, so `grep RG-6` finds the
  rule, the code, the test, and the history. Traceability is *searchability*.
- One vocabulary: field names are law (EVT-1); "no synonyms" is a rule
  because every synonym is a place where two agents diverge silently.

**Rule of thumb:** when an agent asks a question the repo should have
answered, the bug is in the docs — fix the doc layer where the answer belongs.

## Lever 4 — Recorded judgment (compounding)

**Principle:** the expensive part of good work is the judgment calls. If they
evaporate when the session ends, every agent re-derives (or re-botches) them.
Record decisions where the next reader will trip over the same question.

**In this repo:**
- Every spec has a *Decisions* log (dated, appended by whoever resolves an
  ambiguity — W-5) and an *Open questions* section (human-only; guessing them
  is forbidden).
- Killed strategies keep an `AUTOPSY.md` forever (STR-6) — the kill log
  teaches the next strategy author what the data already refuted.
- Venue quirks go into spec 002's table and adapter module docs (add-venue
  skill) — paid-for knowledge stays bought.

**Rule of thumb:** if you had to think for more than a minute to resolve
something, the resolution belongs in a Decisions section, not just in code.

## Lever 5 — Right-sized tasks (specs are prompts)

**Principle:** agent quality degrades superlinearly with task scope. The spec
system is really a *prompt decomposition* system: each spec is a
self-contained brief with requirements, examples, and acceptance tests —
i.e., exactly the shape of prompt agents perform best on.

**In this repo:** W-4 (vertical slices), the recommended implementation order
in `specs/README.md`, and the skills that begin every task with "restate the
requirements as a test list" — forcing scope to crystallize before code.

**Rule of thumb:** if a task spans three specs, it's three tasks. Cut by
data-flow seams (the event stream is the natural knife).

## Lever 6 — Self-review ritual (the second pass is free capability)

**Principle:** the cheapest capability upgrade for any agent is making it
read its own diff adversarially before declaring done. First-pass output is
generation; second-pass is criticism; they use different failure modes and
catch each other.

**In this repo:** the `self-review` skill (`.claude/skills/self-review/`) —
a mandatory pre-push pass: requirement-ID audit, PD-5 honesty check ("did I
loosen anything to get green?"), determinism sweep, secret sweep, and the
"would the next agent understand this from the artifacts alone?" test.
Workflow rule W-8 makes it part of definition-of-done.

## Lever 7 — Clean escalation boundary (knowing when to stop)

**Principle:** the most damaging agent behavior is confidently guessing on
questions that belong to the owner. Powerful ≠ autonomous everywhere;
powerful = autonomous inside a bright boundary, and instantly honest at it.

**In this repo:** Prime Directives define the never-zone; *Open questions*
sections and the "must ask first" column in CLAUDE.md define the ask-zone;
everything else is the go-zone. A failed gate, an unanswerable hypothesis,
or an unmet criterion is reported as a *result*, not routed around (PD-5).

---

## Roadmap: multipliers to add as code lands

| When | Add | Lever |
|---|---|---|
| First Rust code | `cargo deny` + dep-direction test (CONV-3 mechanical) | 2 |
| First crate | Pre-push hook running fmt/clippy/test + guardrails locally | 1,2 |
| Spec status → implemented | Requirement-ID audit in CI: every ID of an implemented spec must appear in ≥1 test name (already scaffolded in guardrails.sh) | 1,3 |
| Backtester exists | Golden fixture in CI (SIM-14) — the determinism canary | 1 |
| First strategy | Mutation testing on risk gate & sizing (do the tests actually bite?) | 1 |
| Multi-agent work | PR-review skill: one agent implements, a second reviews against the spec with fresh context (criticism is cheaper than generation) | 6 |
| Ongoing | Every incident/bug ⇒ regression test + guardrail + runbook, same PR | 2,4 |

## The one-sentence version

Make the repo the smart one: honest tests as ground truth, machines enforcing
the rules, context layered and addressable, judgment written down, tasks cut
to spec-size, a mandatory second look, and a bright line where the human
decides — then any competent agent, today's or next year's, plugs in at full
power.
