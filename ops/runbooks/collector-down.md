# collector-down (P2)

A venue collector process is down (no `/health`, no heartbeats).

## Symptoms
- `/status` shows the collector missing; no new events for that venue.

## Diagnosis
- `systemctl status collector-<venue>`; check for crash / auth / geo-block
  (Binance blocks US IPs — confirm egress region).

## Remediation
- Restart the unit; confirm events flow and the manifest coverage resumes.
- Recorded data during the outage stays a documented gap (W-6).

## Escalation
Collector won't stay up ⇒ disable dependent strategies via `/kill <strategy>`
until data is healthy again.
