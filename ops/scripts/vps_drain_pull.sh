#!/bin/bash
# vps_drain_pull.sh - Stream the CLOSED day-files under /opt/money-printer/data/raw
# (or $1) as a tar archive on stdout, for the nightly drain's transfer phase
# (ops/scripts/vps_drain.ps1). READ-ONLY on the corpus (W-6). Uses the SAME
# closed-day filter as vps_drain_list.sh so list and pull always agree: a
# day-file is closed when its {YYYYMMDD} prefix is strictly before today (UTC).
# The caller extracts into a staging dir and sha256-verifies per file before
# anything lands — a partial stream simply fails verification and nothing is
# released (the guard against partial transfers).
#
# Optional include list (2026-08-16): extra args after $1 are RELATIVE paths
# (e.g. "raw/20260816_bybit_BTCUSDT.log") to pull; when given, ONLY those
# files stream. vps_drain.ps1 pre-classifies candidates against the master
# corpus + manifest and passes only the genuinely-new day-files, so known A-B
# collisions and byte-identical release re-attempts never move over the slow
# link. No args = all closed day-files (unchanged behavior).
set -euo pipefail
BASE="${1:-/opt/money-printer/data}"
shift || true
TODAY="$(TZ=UTC date +%Y%m%d)"

cd "$BASE"
args=()
if [ "$#" -gt 0 ]; then
    for rel in "$@"; do
        name="$(basename "$rel")"
        day="${name:0:8}"
        if [[ "$day" =~ ^[0-9]{8}$ ]] && [[ "$day" -lt "$TODAY" ]] && [ -f "$rel" ]; then
            args+=("$rel")
        fi
    done
else
    while IFS= read -r f; do
        name="$(basename "$f")"
        day="${name:0:8}"
        if [[ "$day" =~ ^[0-9]{8}$ ]] && [[ "$day" -lt "$TODAY" ]]; then
            args+=("$f")
        fi
    done < <(find raw -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]_*.log' | sort)
fi

if [ "${#args[@]}" -eq 0 ]; then
    exit 0
fi
TZ=UTC tar cf - "${args[@]}"
