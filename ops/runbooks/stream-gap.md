# stream-gap (P2)

A venue stream showed a sequence/continuity gap > 5 min (COL-6).

## Symptoms
- Gap alert with venue + symbol; quality manifest marks the window degraded.

## Diagnosis
- Collector logs: reconnect storm, venue-side outage, or local network?
- Check the venue status page and the manifest gap record (STO-4).

## Remediation
- Collector auto-reseeds the book on the next snapshot; confirm a fresh
  `BookSnapshot(GapResync)` appears.
- Do NOT backfill recorded data by hand (W-6, append-only). The gap is a fact;
  downstream sim refuses silent gaps (SIM-6) — that is correct behavior.

## Escalation
Persistent gaps on one venue ⇒ mark it degraded in config; strategies relying
on it should be shadow-only until stable.
