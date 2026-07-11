# disk-high (P2)

Disk usage crossed the warn threshold (> 85%, STO-7).

## Symptoms
- Disk alert with current usage; risk of write failures for the event log.

## Diagnosis
- `df -h`; identify the largest consumer (usually `cold/` parquet or logs).

## Remediation
- Run the compactor if behind; ship + verify cold partitions off-host, then
  the HUMAN deletes verified-migrated data (W-6 — agents never delete data).
- Rotate process logs (30-day retention, OPS-10). Never touch `journal/`.

## Escalation
< 5% free ⇒ P1-adjacent: stop collectors cleanly to protect the event log
before it fails a write; page the owner.
