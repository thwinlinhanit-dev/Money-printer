#!/bin/bash
# Daily compaction and archive run (SPEC-011).
# Schedule via crontab at 00:05 UTC:
#   5 0 * * * /opt/money-printer/ops/scripts/daily_maintenance.sh
set -euo pipefail

export PATH="/usr/local/bin:/usr/bin:/bin"

YESTERDAY=$(date -u -d "yesterday" +%Y-%m-%d)
LOG_DIR="/opt/money-printer/data"
BIN_DIR="/opt/money-printer/bin"
SCRIPT_DIR="/opt/money-printer/ops/scripts"
VENV_DIR="/opt/money-printer/.venv"

# --- 1. Compact yesterday's raw data to Parquet ---
for VENUE in bybit; do
    for SYMBOL in BTCUSDT ETHUSDT; do
        RAW_LOG="${LOG_DIR}/raw/${YESTERDAY}_${VENUE}_${SYMBOL}.log"
        if [ -f "$RAW_LOG" ]; then
            echo "[$(date -u)] Compacting $RAW_LOG"
            "${BIN_DIR}/mp-ops" compact \
                --date "$YESTERDAY" \
                --venue "$VENUE" \
                --symbol "$SYMBOL"
        else
            echo "[$(date -u)] No raw log for ${VENUE}/${SYMBOL} on ${YESTERDAY}"
        fi
    done
done

# --- 2. Archive and clean ---
if [ -d "${VENV_DIR}" ]; then
    # shellcheck disable=SC1091
    source "${VENV_DIR}/bin/activate"
fi

echo "[$(date -u)] Running archive script"
python3 "${SCRIPT_DIR}/../research/archive_data.py"

echo "[$(date -u)] Daily maintenance complete"
