#!/usr/bin/env bash
# Weekly RES-4 band-accuracy study (spec 029 LIQ-6/LIQ-10): replay the last
# WEEK_DAYS of recorded Hyperliquid logs — the spec 028 mp-whale positions
# census (`*_hyperliquid_positions.log`, WHL-6) merged with the hyperliquid
# market logs (mark/OI/funding) — through `run_band_accuracy.py`, which shells
# out to the `whale_study` binary and journals the SIM-10 run record to
# `<runs-dir>/index.jsonl` (RES-4 tracker) plus the idempotent weekly ledger
# and trend (W-6, RES-2 pattern).
#
# Skip-not-fail: with no mp-whale positions log in the window the study has
# nothing to validate and exits 0 without journaling — evidence production
# starts once spec 028 census data exists (the systemd unit's
# ConditionPathExists is the same gate at the scheduler level). A dead
# collector is the dead-man's job to flag (OPS-2), not this job's.
#
# After the study appends the new week to the trend journal, the OPS-13
# drift/decay watch runs `mp-ops band-accuracy-decay` over it — a P3 FYI when
# coverage halved / MRE doubled vs the 12-week baseline. Best-effort: a
# missing mp-ops binary or an unreadable journal warns (visible in journald),
# never fails the study that just succeeded; the check itself never
# fabricates a verdict (fail-closed, CONV-8).
#
# All paths are env-overridable (MP_DATA_DIR, MP_OUT_DIR, MP_RUNS_DIR,
# MP_WHALE_STUDY_BIN, MP_PYTHON, MP_CONFIG, MP_GIT_SHA, MP_REPO_DIR,
# MP_WEEK_DAYS, MP_OPS_CMD) so the script is testable without a live host.
set -euo pipefail

DATA_DIR="${MP_DATA_DIR:-/opt/money-printer/data/raw}"
OUT_DIR="${MP_OUT_DIR:-/opt/money-printer/research/band_accuracy}"
RUNS_DIR="${MP_RUNS_DIR:-/opt/money-printer/runs}"
BINARY="${MP_WHALE_STUDY_BIN:-/opt/money-printer/bin/whale_study}"
PYTHON="${MP_PYTHON:-/usr/bin/python3}"
CONFIG="${MP_CONFIG:-}"
REPO_DIR="${MP_REPO_DIR:-/opt/money-printer}"
WEEK_DAYS="${MP_WEEK_DAYS:-7}"
OPS_CMD="${MP_OPS_CMD:-/opt/money-printer/bin/mp-ops}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JOB="${SCRIPT_DIR}/../research/run_band_accuracy.py"

if [ ! -d "$DATA_DIR" ]; then
  echo "whale-study-weekly: data dir $DATA_DIR missing — skipping (no spec 028 census yet)"
  exit 0
fi

# GNU date, with a BSD fallback (macOS).
SINCE="$(date -u -d "${WEEK_DAYS} days ago" +%F 2>/dev/null || date -u -v-"${WEEK_DAYS}"d +%F)"
# The week bucket is the window's EARLIEST event (data_from_ns), so two
# overlapping windows can derive the same week — the job's idempotent weekly
# ledger absorbs the overlap (RES-2).
mapfile -t logs < <(find "$DATA_DIR" -maxdepth 1 -type f -name '*_hyperliquid*.log' -newermt "$SINCE" | sort)

# The mp-whale positions log is the gating input (WHL-6): no spec 028 census
# in the window ⇒ nothing to grade. Quiet skip, not a failure.
has_positions=0
for f in "${logs[@]}"; do
  case "$f" in
    *_hyperliquid_positions.log) has_positions=1 ;;
  esac
done
if [ "$has_positions" -eq 0 ]; then
  echo "whale-study-weekly: no mp-whale positions log in the last ${WEEK_DAYS} days — skipping (study starts once spec 028 data exists)"
  exit 0
fi

# The commit under which the recorded logs were produced, so the run record
# is reproducible from itself (RES-4/SIM-10). "unknown" without a repo/sha.
GIT_SHA="${MP_GIT_SHA:-}"
if [ -z "$GIT_SHA" ] && [ -d "$REPO_DIR/.git" ]; then
  GIT_SHA="$(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null || true)"
fi

cmd=("$PYTHON" "$JOB" --out-dir "$OUT_DIR" --runs-dir "$RUNS_DIR" --whale-study "$BINARY")
cmd+=(--git-sha "${GIT_SHA:-unknown}")
if [ -n "$CONFIG" ] && [ -f "$CONFIG" ]; then
  cmd+=(--config "$CONFIG")
fi
for f in "${logs[@]}"; do
  cmd+=(--log "$f")
done

echo "whale-study-weekly: grading $((${#logs[@]})) hyperliquid log(s) into $RUNS_DIR/index.jsonl"
"${cmd[@]}"

# OPS-13 drift/decay watch on the freshly-updated trend journal. Advisory P3:
# a missing binary or corrupt journal warns and keeps going (the monthly
# report surfaces the same journal; the alert send is a deployment artifact).
TREND="${OUT_DIR}/band_accuracy.jsonl"
if command -v "$OPS_CMD" >/dev/null 2>&1; then
  "$OPS_CMD" band-accuracy-decay --trend "$TREND" \
    || echo "whale-study-weekly: band-accuracy-decay check failed (exit $?) — see journald" >&2
else
  echo "whale-study-weekly: mp-ops ($OPS_CMD) not found — skipping the band-accuracy decay check (OPS-13)"
fi
