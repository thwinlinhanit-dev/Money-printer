# clock-skew (P2)

Host clock skew exceeded 100ms vs NTP (OPS-7).

## Symptoms
- Clock-skew alert; lead-lag research and venue-timestamp deltas become unsafe.

## Diagnosis
- `chronyc tracking` / `timedatectl`; check NTP reachability and drift.

## Remediation
- Restore NTP sync; confirm skew < 100ms. Timestamps are `exch_ts_ns` and
  `recv_ts_ns` (CONV) — a bad `recv` clock corrupts recv-based features.

## Escalation
Sustained skew ⇒ treat recv-time features as suspect for the window; flag the
manifest and hold time-sensitive strategies.
