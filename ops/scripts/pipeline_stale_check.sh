#!/bin/bash
# Dead-man for the daily gate itself (OPS-17, runbook ops/runbooks/pipeline-stale.md).
# The 00:05 cron gate (daily_maintenance.sh) is one job; if THAT job silently
# dies, the streak goes blind (blueprint failure-mode #6). This check verifies
# the previous UTC day's scorecard exists and parses, raising a P1 when it
# does not — delivered via Telegram + the P1 webhook when credentials exist.
#
# Install in crontab (hourly, after the 00:15 deadline):
#   15 * * * * /opt/money-printer/ops/scripts/pipeline_stale_check.sh
#
# Safe on binaries that predate the subcommand: the probe skips silently
# rather than red the cron log with "unknown subcommand" until the tree is
# rebuilt with OPS-17 in it.
set -u

BIN="/opt/money-printer/bin/mp-ops"
LOG="/opt/money-printer/data/scorecards/pipeline.log"
# cron runs with cwd=/ — the scorecards dir MUST be absolute or the dead-man
# would look at /data/scorecards and false-alarm hourly (found 2026-08-13).
SCORE_DIR="/opt/money-printer/data/scorecards"

# Probe: `--ts-ns 1` runs the check with an injected clock; an older binary
# without the subcommand exits non-zero (unknown subcommand).
if ! "$BIN" pipeline-stale --scorecards-dir "$SCORE_DIR" --ts-ns 1 >/dev/null 2>&1; then
    echo "[$(date -u)] pipeline-stale: binary predates OPS-17 - skipping (rebuild to activate)" >> "$LOG"
    exit 0
fi

mkdir -p "$(dirname "$LOG")"
echo "[$(date -u)] pipeline-stale check:" >> "$LOG"
"$BIN" pipeline-stale --scorecards-dir "$SCORE_DIR" --telegram --webhook >> "$LOG" 2>&1
