#!/bin/bash
# Weekly RES-4 whale band-accuracy study (spec 029 LIQ-6). Fired by
# ops/systemd/whale-study.timer (Mon 06:30 UTC): gathers the week that just
# ended's Hyperliquid raw logs (mp-whale positions + market) and runs
# research/run_band_accuracy.py, which shells out to `whale_study --json`
# (journaling the SIM-10 run record to <out-dir>/runs/index.jsonl) and writes
# the idempotent weekly ledger <out-dir>/{week}.json (W-6, RES-2 pattern).
#
# Until mp-whale logs exist the week has no positions log and the job fails
# loudly (non-zero exit, visible in journald) — never an optimistic n=0 row.
set -euo pipefail

export PATH="/usr/local/bin:/usr/bin:/bin"

ROOT="/opt/money-printer"
RAW_DIR="${RAW_DIR:-${ROOT}/data/raw}"
OUT_DIR="${OUT_DIR:-${ROOT}/research/band_accuracy}"
WHALE_BIN="${WHALE_BIN:-${ROOT}/target/release/whale_study}"
GIT_SHA="${GIT_SHA:-$(git -C "${ROOT}" rev-parse --short HEAD 2>/dev/null || echo dev)}"

# ISO week that just ended (Mon 00:00 UTC .. Sun 24:00 UTC): today minus 7 days.
WEEK=$(date -u -d "-7 days" +%G-W%V)

args=()
n_positions=0
for i in $(seq 0 6); do
    day=$(date -u -d "-$((7 - i)) days" +%Y%m%d)
    pos_log="${RAW_DIR}/${day}_hyperliquid_positions.log"
    if [ -f "${pos_log}" ]; then
        args+=(--log "${pos_log}")
        n_positions=$((n_positions + 1))
    fi
    for log in "${RAW_DIR}"/"${day}"_hyperliquid_*.log; do
        case "${log}" in
            *_positions.log) ;;  # the positions log is already passed above
            *) if [ -f "${log}" ]; then args+=(--log "${log}"); fi ;;
        esac
    done
done

if [ "${n_positions}" -eq 0 ]; then
    echo "[$(date -u)] no mp-whale positions logs for ${WEEK} under ${RAW_DIR} (mp-whale not recording yet)" >&2
    exit 1
fi

echo "[$(date -u)] Running weekly band-accuracy study for ${WEEK} (logs: ${#args[@]})"
python3 "${ROOT}/research/run_band_accuracy.py" \
    --week "${WEEK}" \
    --out-dir "${OUT_DIR}" \
    --git-sha "${GIT_SHA}" \
    --whale-study "${WHALE_BIN}" \
    "${args[@]}"
