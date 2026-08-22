#!/bin/bash
# vps_drain_list.sh - List the CLOSED day-files under /opt/money-printer/data/raw
# (or $1) for the nightly drain (ops/scripts/vps_drain.ps1). A day-file
# {YYYYMMDD}_{venue}_{SYMBOL}.log is CLOSED when its date prefix is strictly
# before today (UTC): the collectors rotate at UTC midnight, so a closed day's
# file is never written again. READ-ONLY on the corpus (W-6).
#
# Output: one line per file:  file <relpath> <size_bytes> <sha256>
# plus a final line:          count <N>
#
# Keeps in sync with vps_drain_release.sh (same base dir resolution).
set -euo pipefail
BASE="${1:-/opt/money-printer/data}"
TODAY="$(TZ=UTC date +%Y%m%d)"

cd "$BASE"
count=0
# Day-files only: {YYYYMMDD}_*.log — excludes .lock_*, *.pid, *.heartbeat,
# trace_*/watchdog_* diagnostics, and anything not date-prefixed.
while IFS= read -r f; do
    name="$(basename "$f")"
    day="${name:0:8}"
    if [[ "$day" =~ ^[0-9]{8}$ ]] && [[ "$day" -lt "$TODAY" ]]; then
        size="$(stat -c %s "$f")"
        hash="$(sha256sum "$f" | awk '{print $1}')"
        printf 'file %s %s %s\n' "$f" "$size" "$hash"
        count=$((count + 1))
    fi
done < <(find raw -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]_*.log' | sort)
printf 'count %s\n' "$count"
