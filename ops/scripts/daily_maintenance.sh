#!/bin/bash
# Daily integrity gate → compaction → manifest pipeline (spec 024).
# Schedule via crontab at 00:05 UTC:
#   5 0 * * * /opt/money-printer/ops/scripts/daily_maintenance.sh
set -euo pipefail

export PATH="/usr/local/bin:/usr/bin:/bin"

YESTERDAY=$(date -u -d "yesterday" +%Y-%m-%d)
LOG_DIR="/opt/money-printer/data"
BIN_DIR="/opt/money-printer/bin"
SCRIPT_DIR="/opt/money-printer/ops/scripts"
VENV_DIR="/opt/money-printer/.venv"
RECORDINGS="${RECORDINGS:-binance:BTCUSDT binance:ETHUSDT}"
REQUIRED_STREAMS="${REQUIRED_STREAMS:-trade book funding mark_price liquidation open_interest}"

cd "/opt/money-printer"

require_args=()
for stream in ${REQUIRED_STREAMS}; do
    require_args+=(--require-stream "$stream")
done

# --- 1. Audit the whole recording matrix before writing any cold data ---
scorecard_args=(scorecard --date "$YESTERDAY")
for recording in ${RECORDINGS}; do
    scorecard_args+=(--required "$recording")
done
scorecard_args+=("${require_args[@]}")

SCORECARD_DIR="${LOG_DIR}/scorecards"
mkdir -p "$SCORECARD_DIR"
SCORECARD_PATH="${SCORECARD_DIR}/${YESTERDAY}.json"
"${BIN_DIR}/mp-ops" "${scorecard_args[@]}" > "$SCORECARD_PATH"
if ! grep -q '"promotable": true' "$SCORECARD_PATH"; then
    echo "[$(date -u)] Recording scorecard failed: $SCORECARD_PATH" >&2
    exit 1
fi

# --- 2. Compact only scorecard-approved recordings ---
for recording in ${RECORDINGS}; do
    VENUE="${recording%%:*}"
    SYMBOL="${recording#*:}"
    RAW_LOG="${LOG_DIR}/raw/${YESTERDAY//-/}_${VENUE}_${SYMBOL}.log"
    if [ ! -f "$RAW_LOG" ]; then
        echo "[$(date -u)] Required raw log missing: $RAW_LOG" >&2
        exit 1
    fi
    echo "[$(date -u)] Compacting $RAW_LOG"
    "${BIN_DIR}/mp-ops" compact \
        --date "$YESTERDAY" \
        --venue "$VENUE" \
        --symbol "$SYMBOL" \
        "${require_args[@]}"
done

# --- 3. Archive verified copies; source recordings remain append-only ---
if [ -d "${VENV_DIR}" ]; then
    # shellcheck disable=SC1091
    source "${VENV_DIR}/bin/activate"
fi

echo "[$(date -u)] Running archive script"
python3 "${SCRIPT_DIR}/../research/archive_data.py"

echo "[$(date -u)] Daily integrity pipeline complete: $SCORECARD_PATH"
