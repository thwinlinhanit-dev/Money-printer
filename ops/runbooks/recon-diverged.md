# recon-diverged (P1)

The reconciler found live venue state ≠ internal OMS state (EXE reconcile).

## Symptoms
- DIVERGED alert with the mismatching position/order.

## Diagnosis
- Compare venue truth (positions/open orders) vs OMS store. An UNKNOWN order
  (see unknown-order) is the usual cause.

## Remediation
- `/kill GLOBAL` from the phone (latch — works even if oms is wedged).
- Reconcile from venue truth: cancel unknown resting orders, square the
  position delta reduce-only. Do NOT add exposure to "fix" a mismatch (PD-1).

## Escalation
Cannot reconcile within minutes ⇒ `/flatten` (reduce-only) and page the owner;
a wrong internal position is unbounded risk.
