# killswitch-tripped (P1)

A kill switch latched (manual `/kill`, or an RG-8/9 breach auto-tripped it).

## Symptoms
- Killswitch alert with the tripped scope (GLOBAL / venue / strategy).

## Diagnosis
- Why did it trip? Manual action, drawdown governor, or a gate breach? Check
  the journal for the triggering event.

## Remediation
- Latches are one-way by design (EXE-7): risk-off is free, only a HUMAN resets.
- Confirm the underlying cause is resolved, then the owner clears the latch
  file deliberately. Agents cannot reset (the API requires `human = true`).

## Escalation
If tripped by an automated breach, do NOT reset until the breach is understood
and, if needed, a spec/limit change is signed off by the owner (PD-1).
