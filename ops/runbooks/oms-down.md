# oms-down (P1 in live)

The OMS process is down while in live mode.

## Symptoms
- oms-down / dead-man alert; no order lifecycle progress.

## Diagnosis
- `systemctl status oms`; crash vs hang. Any in-flight orders become UNKNOWN.

## Remediation
- `/kill GLOBAL` first — the latch is a file the gate reads, so it protects the
  account without oms (RG-10).
- Restart oms; on startup it MUST reconcile from venue truth before accepting
  new intents. Resolve any UNKNOWN orders (see unknown-order).

## Escalation
oms won't come up ⇒ `/flatten` from the phone and page the owner; do not run
live without a healthy OMS.
