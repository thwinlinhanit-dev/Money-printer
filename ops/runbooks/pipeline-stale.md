# pipeline-stale (P1)

The daily integrity gate did not land. By `--deadline-min` (default 15) minutes
after UTC midnight, the previous UTC day's scorecard
(`data/scorecards/YYYY-MM-DD.json`) must exist and parse. It does not — the
gate (cron `daily_maintenance.sh` on the VPS, `MoneyPrinterDailyPipeline` on
the Windows box) either never ran, crashed before archiving, or wrote a
corrupt file. The streak (spec 024) is going blind: a lost day is visible
within 24h *only if this check fires* (blueprint failure-mode #6).

## Symptoms
- P1 alert with detail "no scorecard for {date} ..." or "exists but is
  unparseable / cannot be read".

## Diagnosis
1. Is the scorecard actually there? `ls -la data/scorecards/ | tail` — and
   `mp-ops status` for the full picture.
2. Did the gate run? On the VPS: `journalctl -u cron` / the cron mail spool /
   `tail data/scorecards/pipeline.log`. On Windows: Task Scheduler →
   `MoneyPrinterDailyPipeline` → last run result.
3. A *present but unparseable* scorecard means the pipeline wrote a truncated
   file (disk full, killed mid-write) — inspect the file's tail bytes.

## Remediation
- If the gate merely ran late (still inside the day): wait — the next hourly
  `pipeline-stale` run clears once the scorecard lands (the check reads the
  file; dedupe prevents a re-alert storm inside 6h).
- If it never ran: fix and re-run the gate manually for yesterday:
  `mp-ops scorecard --date <yesterday> --required hyperliquid:BTC --required
  hyperliquid:ETH ...` (the daily pipeline script is the canonical wrapper).
- If the scorecard is corrupt: delete it and re-score yesterday — the streak
  recomputes from the files on disk.
- Root-cause the miss (cron disabled? VM rebooted? task deleted?) before the
  next midnight — this alert is the tripwire, not the fix.

## Escalation
A second consecutive `pipeline-stale` day means the gate itself is broken, not
just late — treat the data-integrity pipeline as down and fix before any
promotion verdict is trusted.
