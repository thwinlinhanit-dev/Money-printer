# process-deadman (P2 / P1 in live)

A registered process stopped sending heartbeats (missed 3 beats).

## Symptoms
- Dead-man alert names the silent process and how long it has been quiet.
- `/status` shows the process stale or absent.

## Diagnosis
- `systemctl status <unit>` — crashed, OOM-killed, or hung?
- Check its logs (last 30 min) for a panic or a blocking call.
- Confirm the host itself is up (the external watcher covers total host loss).

## Remediation
- If crashed: `systemctl restart <unit>`; confirm heartbeats resume in `/status`.
- If hung: capture a stack/backtrace first (evidence), then restart.
- If it is `oms` in live mode (P1): `/kill GLOBAL` from the phone first — the
  latch protects the account independent of oms — then restart.

## Escalation
Repeated crashes (>3/hour) ⇒ stop the affected strategy via `/kill`, open an
incident, do not paper over with a restart loop.
