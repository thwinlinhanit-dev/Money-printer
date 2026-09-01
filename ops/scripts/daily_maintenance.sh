#!/bin/bash
# Daily integrity gate -> compaction -> manifest pipeline (spec 024).
# Schedule via crontab at 00:05 UTC:
#   5 0 * * * /opt/money-printer/ops/scripts/daily_maintenance.sh
#
# Zero-Cost Mode (2026-08-31): set ZERO_COST=1 (or leave default) to run
# under $0 budget constraints — Hyperliquid-only, no book stream, relaxed
# gate. Set ZERO_COST=0 for full-mode (all venues, book required).
# See docs/ZERO_COST_MODE.md.
set -euo pipefail

export PATH="/usr/local/bin:/usr/bin:/bin"

YESTERDAY=$(date -u -d "yesterday" +%Y-%m-%d)
LOG_DIR="/opt/money-printer/data"
BIN_DIR="/opt/money-printer/bin"
SCRIPT_DIR="/opt/money-printer/ops/scripts"
VENV_DIR="/opt/money-printer/.venv"

# --- Zero-Cost Mode switch (docs/ZERO_COST_MODE.md) --------------------------
# ZERO_COST defaults to 1 (free-tier VPS). Override with ZERO_COST=0 for
# full-mode recording (requires paid VPS with enough disk for book streams).
ZERO_COST="${ZERO_COST:-1}"

if [ "$ZERO_COST" = "1" ]; then
    # Zero-Cost Mode: Hyperliquid only (permissionless, no geo-blocks),
    # BTC + ETH only (storage bounded). No book stream — trades +
    # activeAssetCtx (funding/mark/OI) are the minimal viable set.
    RECORDINGS="${RECORDINGS:-hyperliquid:BTC hyperliquid:ETH}"
    REQUIRED_STREAMS="${REQUIRED_STREAMS:-trade funding mark_price open_interest}"
    echo "[$(date -u)] Zero-Cost Mode: RECORDINGS=$RECORDINGS REQUIRED_STREAMS=$REQUIRED_STREAMS"
else
    # Full-mode: all venues, book stream required.
    # 2026-08-08: Phase-0 venue is hyperliquid (egress is geo-filtered by Binance
    # futures, spec 024). Symbols are bare coin names. Liquidation data comes from
    # the on-chain whale census (spec 028) and is not a required gate stream.
    # 2026-08-13 (COL-29): bybit:BTCUSDT joined the gate set — Bybit's public WS
    # liquidation topic is the real live source for the `Liquidation` event from
    # this egress, and its recording carries the full required stream set (trade
    # book funding mark_price open_interest + liquidation).
    RECORDINGS="${RECORDINGS:-hyperliquid:BTC hyperliquid:ETH bybit:BTCUSDT bybit:ETHUSDT bybit:SOLUSDT}"
    REQUIRED_STREAMS="${REQUIRED_STREAMS:-trade book funding mark_price open_interest}"
fi

cd "/opt/money-printer"

# --- 0. PD guardrails (audit 2026-08-17): mechanical rulebook enforcement
# (PD-1..4, W-7) before any number from the scorecard is trusted - same
# semantics as the PowerShell port (daily_pipeline.ps1). Fail-closed: a
# violation OR a missing guardrails script stops the pipeline.
echo "[$(date -u)] Running ops/ci/guardrails.sh ..."
if [ ! -f "/opt/money-printer/ops/ci/guardrails.sh" ]; then
    echo "[$(date -u)] guardrails script MISSING - cannot verify PD-1..4/W-7; refusing to trust today's scorecard" >&2
    exit 1
fi
if ! bash "/opt/money-printer/ops/ci/guardrails.sh"; then
    echo "[$(date -u)] guardrails failed - the tree violates the rulebook (PD-1..4/W-7); fix before trusting today's scorecard" >&2
    exit 1
fi
echo "[$(date -u)] guardrails: all checks passed"

require_args=()
for stream in ${REQUIRED_STREAMS}; do
    require_args+=(--require-stream "$stream")
done
# COL-29 (spec 024): venues with a liquidation source must also show the
# `liquidation` stream every recorded day. Venue-scoped (--require-stream
# venue:stream) so hyperliquid — which has no native liq stream by design —
# is unaffected. Bybit delivers it via its public WS liquidation topic (the
# live leg). Binance's REST allForceOrders leg is USER_DATA — only delivers
# once MP_BINANCE_API_KEY/SECRET exist (dead-until-creds); the requirement
# is still correct either way: a Binance recording without liquidations is
# not promotable.
for recording in ${RECORDINGS}; do
    venue="${recording%%:*}"
    case "$venue" in
        binance|bybit) require_args+=(--require-stream "${venue}:liquidation") ;;
    esac
done

# --- 1. Audit the whole recording matrix before writing any cold data ---
scorecard_args=(scorecard --date "$YESTERDAY")
for recording in ${RECORDINGS}; do
    scorecard_args+=(--required "$recording")
done
scorecard_args+=("${require_args[@]}")
# Zero-Cost Mode (docs/ZERO_COST_MODE.md): pass --zero-cost to activate
# the relaxed gate (0.95 coverage, 14-day streak, stale bursts as warnings).
if [ "$ZERO_COST" = "1" ]; then
    scorecard_args+=(--zero-cost)
fi

SCORECARD_DIR="${LOG_DIR}/scorecards"
mkdir -p "$SCORECARD_DIR"
SCORECARD_PATH="${SCORECARD_DIR}/${YESTERDAY}.json"
"${BIN_DIR}/mp-ops" "${scorecard_args[@]}" > "$SCORECARD_PATH"

# --- 1.5 Decision determinism check (spec 018 MOD-9..11) --------------------
# Replay yesterday's recorded session through the PRODUCTION runtime (features
# -> strategy -> risk) and require the decision log to be byte-identical across
# two fresh runs. The promotion gate reads the artifact this writes
# (<date>.determinism.json): a window day without a PASSING artifact does not
# promote, and a divergence is determinism-diff (P2, runbook
# ops/runbooks/determinism-diff.md). Fail-closed: a missing binary means the
# gate cannot prove the day — the run exits 1 and no cold writes happen.
det_args=(--date "$YESTERDAY")
for recording in ${RECORDINGS}; do
    det_args+=(--required "$recording")
done
if [ -x "${BIN_DIR}/mp-determinism" ]; then
    # Pin the replay config (sim/determinism.toml) so the artifact's strategy
    # + seed are the reviewed values, not an implicit default (MOD-10).
    if [ -f "/opt/money-printer/sim/determinism.toml" ]; then
        det_args+=(--config /opt/money-printer/sim/determinism.toml)
    fi
    if "${BIN_DIR}/mp-determinism" "${det_args[@]}" --write >> "$SCORECARD_DIR/pipeline.log" 2>&1; then
        echo "[$(date -u)] Determinism check: passed for $YESTERDAY"
    else
        det_code=$?
        echo "[$(date -u)] Determinism check FAILED for $YESTERDAY (exit $det_code) - determinism-diff, promotion blocked" >&2
        exit 1
    fi
else
    echo "[$(date -u)] mp-determinism not installed - determinism gate inactive, day $YESTERDAY cannot prove itself" >&2
    exit 1
fi

# Promotion gate: a lost day is visible within 24h, never
# silently (ops/runbooks/vps-phase0-bringup.md sec 3).
# Under Zero-Cost Mode: 14-day streak (up from 7), 0.95 coverage (down
# from 0.995), stale bursts are warnings only (docs/ZERO_COST_MODE.md).
promote_args=(promote --scorecards-dir "$SCORECARD_DIR")
for recording in ${RECORDINGS}; do
    promote_args+=(--required "$recording")
done
if [ "$ZERO_COST" = "1" ]; then
    promote_args+=(--zero-cost)
fi
SCORE="$("${BIN_DIR}/mp-ops" "${promote_args[@]}")"
echo "[$(date -u)] Promotion gate: $SCORE"
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

# --- 2.5 Materialize features for the approved day (spec 016 Phase 2) -------
# The feature store is the research substrate: written only for days that
# passed the INT-4 gate (dirty days would bake gaps/staleness into features).
# Log set = every required recording + the whale-positions census for venues
# that have one (spec 028 feeds whale.net/delta). Exact duplicate --log paths
# are dropped by the CLI (MAT-5 canonical input set).
MAT_BIN="${BIN_DIR}/mp-materialize"
FEATURES_TOML="/opt/money-printer/features/features.toml"
GIT_SHA="$(git -C /opt/money-printer rev-parse HEAD 2>/dev/null || echo unknown)"
mat_args=()
for recording in ${RECORDINGS}; do
    VENUE="${recording%%:*}"; SYMBOL="${recording#*:}"
    RAW_LOG="${LOG_DIR}/raw/${YESTERDAY//-/}_${VENUE}_${SYMBOL}.log"
    if [ -f "$RAW_LOG" ]; then mat_args+=(--log "$RAW_LOG"); fi
    POS_LOG="${LOG_DIR}/raw/${YESTERDAY//-/}_${VENUE}_positions.log"
    if [ -f "$POS_LOG" ]; then mat_args+=(--log "$POS_LOG"); fi
done
if [ "${#mat_args[@]}" -eq 0 ]; then
    echo "[$(date -u)] Materialize: no raw logs found for $YESTERDAY - skipping" >&2
else
    echo "[$(date -u)] Materializing features for $YESTERDAY"
    "$MAT_BIN" "${mat_args[@]}" --config "$FEATURES_TOML" --out "${LOG_DIR}/features" --git-sha "$GIT_SHA"
fi

# --- 2.7 Hot-tier retention enforcement (docs/RETENTION_POLICY.md) -----------
# Under Zero-Cost Mode, raw tick data older than 14 days is deleted to
# keep the VPS under the 30 GB free-tier cap. Only raw logs are pruned;
# compacted Parquet in data/parquet/ and features in data/features/ are
# kept (they are orders of magnitude smaller). Human-deletable: W-6
# (never auto-delete without hash-verified compact proof).
if [ "$ZERO_COST" = "1" ]; then
    RETENTION_DAYS="${RETENTION_DAYS:-14}"
    RAW_DIR="${LOG_DIR}/raw"
    echo "[$(date -u)] Hot-tier retention: deleting raw logs older than ${RETENTION_DAYS} days"
    DELETED=0
    while IFS= read -r -d '' old_file; do
        echo "[$(date -u)] Retention: deleting $(basename "$old_file")"
        rm -f "$old_file"
        DELETED=$((DELETED + 1))
    done < <(find "$RAW_DIR" -maxdepth 1 -name '*.log' -type f -mtime +"$RETENTION_DAYS" -print0 2>/dev/null || true)
    echo "[$(date -u)] Hot-tier retention: deleted $DELETED old raw log(s)"
fi

# --- 3. Archive verified copies; source recordings remain append-only ---
# S3 archive is optional (runbook sec 4: the minimal bar is an off-host copy
# via rclone/rsync). Only run when a bucket is actually configured, else the
# script would exit 1 on missing env vars and red the daily log for nothing.
if [ -n "${AWS_BUCKET_NAME:-}" ]; then
    if [ -d "${VENV_DIR}" ]; then
        # shellcheck disable=SC1091
        source "${VENV_DIR}/bin/activate"
    fi

    echo "[$(date -u)] Running archive script"
    python3 "${SCRIPT_DIR}/../../research/archive_data.py"
else
    echo "[$(date -u)] No S3 archive configured (AWS_BUCKET_NAME unset) - skipping"
fi

echo "[$(date -u)] Daily integrity pipeline complete: $SCORECARD_PATH"
