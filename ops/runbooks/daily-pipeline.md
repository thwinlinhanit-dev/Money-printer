# daily-pipeline

The daily promotion-gate verdict alert (`mp-ops telegram-send --id daily-pipeline`,
sent by `ops/scripts/daily_pipeline.ps1` at the 00:05 UTC run). This is the
INT-4 gate telling the owner a recorded day failed to promote — the streak is
stalled, and the reason is in the message's per-recording DIRTY/blocking lines.

## What the alert means

- **severity p2** = the day is NOT promotable (`daily_pipeline.ps1` exits 1;
  Task Scheduler flags the run). Each recording line shows
  `venue/symbol: DIRTY (blocking=N coverage=… stale_bursts=… worst_gap_s=…)`
  (2026-08-12: coverage vs the 0.995 bar, stale-burst count, and the worst
  single gap are on every line so a dirty day is self-explaining).
- **severity p3** = the day IS promotable (informational; the streak advanced).

## First step — always

The full evidence lives in the scorecard and pipeline log:

- `data/scorecards/<date>.json` — per-recording `clean`, `event_count`,
  `coverage`, `findings`, `blocking_findings`
- `data/scorecards/pipeline.log` — the run's own log lines

Run the detail audit for each DIRTY recording to see the actual finding codes
(`coverage_gap`, `stale_stream`, etc.):

```bash
mp-ops audit --date <YYYYMMDD> --venue hyperliquid --symbol BTC
mp-ops audit --date <YYYYMMDD> --venue hyperliquid --symbol ETH
```

## Common causes (Phase-0 hyperliquid)

Under the 2026-08-12 gate semantics the verdict is **coverage ≥ 0.995** with
no identity/provenance/loss findings; `stale_stream` and `coverage_gap` are
warnings, not vetoes (their severity shows up in `coverage` / `worst_gap_s`).

1. **`low_coverage`** (blocking) — recv-clock coverage below 0.995, usually
   because of the ~3h20m WS burst: the connection stays half-alive while the
   venue stops delivering, the collector emits `Status::Stale` every ~15s
   (COL-2 watchdog), and the recv clock accumulates holes. Bursts on both
   symbols at the same times trace to the network path (VPN tunnel re-key,
   Wi-Fi drop) — NOT the collector. Check the collector trace logs
   (`data/raw/trace_<date>_hyperliquid_*.log`) for
   `stream stale; reconnecting (COL-2)` / `os error 10054`. Keepalive pings
   now guard against venue/NAT idle mechanisms (2026-08-12); the VPS A-B
   (`ops/runbooks/vps-phase0-bringup.md` §6) isolates the egress variable.
2. **`sequence_gap` / `backpressure_loss`** (blocking) — venue-side order
   loss or dropped frames that the coverage number cannot see. Same timestamp
   on both symbols = host-level event; a single symbol = venue/stream issue.
3. **Identity/provenance codes** (blocking) — the recording cannot be
   attributed; investigate before anything else.

## Resolution

- Confirm the collector(s) ran the full UTC day continuously
  (heartbeats fresh, no sleep). Fix power settings / network path.
- A dirty day is never patched: the INT-4 gate only promotes days that audit
  clean. The streak restarts when a fully clean day is archived.
