# determinism-diff (P2)

Online vs offline feature/decision hashes diverged (SIM-11 / FEA determinism).

## Symptoms
- Determinism-diff alert with the mismatching hash and event range.

## Diagnosis
- This is a correctness bug, not a market event. Identify the first diverging
  event; check for wall-clock reads, unseeded RNG, or hashmap-order iteration
  on a decision path (PD-3).

## Remediation
- Do NOT loosen the check to silence it (PD-5). Freeze promotions, capture the
  minimal replay, and fix the non-determinism at its source. Add a
  `regression_<issue>` test.

## Escalation
Any determinism diff on a live strategy ⇒ `/kill <strategy>` until root-caused;
the edge is unmeasurable while online≠offline.
