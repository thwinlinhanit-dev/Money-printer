# daily-pipeline

The daily promotion-gate verdict alert (`mp-ops telegram-send --id daily-pipeline`,
sent by `ops/scripts/daily_pipeline.ps1` at the 00:05 UTC run). This is the
INT-4 gate telling the owner a recorded day failed to promote — the streak is
stalled, and the reason is in the message's per-recording DIRTY/blocking lines.

## What the alert means

- **severity p2** = the day is NOT promotable (`daily_pipeline.ps1` exits 1;
  Task Scheduler flags the run). Each recording line shows
  `venue/symbol: DIRTY (blocking=N)`.
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

1. **`stale_stream`** — the WS connection died and the collector emitted
   `Status::Stale` events (COL-2 watchdog, 15s threshold). Bursts every ~3h20m
   on both symbols usually trace to the network path (VPN tunnel re-key, Wi-Fi
   drop) — NOT the collector. Check the collector trace logs
   (`data/raw/trace_<date>_hyperliquid_*.log`) for
   `stream stale; reconnecting (COL-2)` / `os error 10054`.
2. **`coverage_gap`** — a real hole in the recv clock (e.g. machine sleep or a
   network outage). Same timestamp on both symbols = host-level event.

## Resolution

- Confirm the collector(s) ran the full UTC day continuously
  (heartbeats fresh, no sleep). Fix power settings / network path.
- A dirty day is never patched: the INT-4 gate only promotes days that audit
  clean. The streak restarts when a fully clean day is archived.
