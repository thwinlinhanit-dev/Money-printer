#!/usr/bin/env bash
# Cross-tape kill panel (spec 054): swing_hyperliquid_BTC vs merged_hyperliquid_BTC_4d
# Round: 2026-09-09, params-hash "rel32-swing-baseline" (fresh identities, no W-6 collision)
set -uo pipefail
cd "Money-printer-claude-trading-research-intelligence-4tzim4" || exit 97
LOG=data/swing_hyperliquid_BTC.log
D4=data/merged_hyperliquid_BTC_4d.log
OD=data/observations
RD=data/runs
PARAMS=rel32-swing-baseline
OUT=data/panel-swing-r2.log

echo "### PANEL START $(date -u +%Y-%m-%dT%H:%M:%SZ)" | tee -a "$OUT"
cargo build --release -p mp-sim --bin sim 2>&1 | tail -2 | tee -a "$OUT"
rc=${PIPESTATUS[0]}
[ "$rc" -ne 0 ] && { echo "### BUILD FAILED rc=$rc" | tee -a "$OUT"; exit 98; }
echo "### BUILD OK $(date -u +%Y-%m-%dT%H:%M:%SZ)" | tee -a "$OUT"

run() {
  local s="$1" rid="$2" lg="$3"
  echo "==================================================" | tee -a "$OUT"
  echo "### RUN strategy=$s run_id=$rid log=$(basename "$lg") START $(date -u +%Y-%m-%dT%H:%M:%SZ)" | tee -a "$OUT"
  ./target/release/sim backtest --log "$lg" --strategy "$s" --seed 1 \
    --run-id "$rid" --runs-dir "$RD" \
    --params-hash "$PARAMS" --obs-dir "$OD" \
    --horizons "15m,1h,4h,1d" >> "$OUT" 2>&1
  echo "### RUN $rid EXIT=$? END $(date -u +%Y-%m-%dT%H:%M:%SZ)" | tee -a "$OUT"
}

run coinflip-any          eval-20260909-control-swing    "$LOG"
run carry-v1              eval-20260909-carry-swing      "$LOG"
run orderflow-v1          eval-20260909-orderflow-swing  "$LOG"
run liq-fade-v1           eval-20260909-liqfade-swing    "$LOG"
run swing-range-reclaim-v1 eval-20260909-swingreclaim-swing "$LOG"
run coinflip-any          eval-20260909-control-4d-rerun "$D4"
echo "### PANEL DONE $(date -u +%Y-%m-%dT%H:%M:%SZ)" | tee -a "$OUT"
