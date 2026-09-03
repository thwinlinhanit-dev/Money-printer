#!/bin/bash
# repair_vps_20260902.sh — Re-compact the hollow 2026-09-02 hyperliquid cold
# tier on the VPS with the deployed fixed binary (schema-4 TradeWithAddr
# compactor fix, 5f8fd7d). The 09-02 raw logs were drained to the desktop on
# 2026-09-03 18:41Z (released); this script restores byte-identical copies
# staged at /home/mp-egress/ (desktop sha256 == drain manifest sha256),
# quarantines the hollow parquet/manifest (1,126-byte zero-row shells), and
# re-compacts. Market units are not touched. After verification the restored
# raws are removed again (returning to the released state — the desktop
# archive and the VPS cold tier now both hold the day).
#
# Self-elevates (google-sudoers NOPASSWD, COL-29). Idempotent: re-running
# after success is a no-op (compact skips on matching source hash).
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        exec sudo -n bash "$0" "$@"
    fi
    echo "ERROR: this repair needs root." >&2
    exit 1
fi

TREE=/opt/money-printer
BIN=$TREE/bin/mp-ops
STAGE=/home/mp-egress
Q=$TREE/data/cold/.quarantine-hollow-20260904
DATE_DASHED=2026-09-02
DATE_FLAT=20260902

echo "[1/6] Restore 09-02 hyperliquid raws from stage (byte-identical drain copies)"
for s in BTC ETH; do
    src="$STAGE/${DATE_FLAT}_hyperliquid_${s}.log"
    if [ ! -f "$src" ]; then
        echo "ERROR: staged raw missing: $src (scp it from the desktop first)"
        exit 1
    fi
    install -o printer -g printer -m 0644 "$src" "$TREE/data/raw/${DATE_FLAT}_hyperliquid_${s}.log"
    echo "  restored $s ($(stat -c%s "$src") bytes)"
done

echo "[2/6] Snapshot pre-repair manifest claims"
MP="$TREE/data/cold/manifests/venue=hyperliquid/date=$DATE_DASHED.json"
if [ -f "$MP" ]; then
    python3 - "$MP" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
for k, v in sorted(m["streams"].items()):
    if k.startswith("trades"):
        print(f"  {k}: events={v['events']}")
PY
else
    echo "  manifest not present (already quarantined on a prior run)"
fi

echo "[3/6] Quarantine hollow artifacts (non-destructive)"
mkdir -p "$Q/manifests" "$Q/trades"
if [ -f "$TREE/data/cold/manifests/venue=hyperliquid/date=$DATE_DASHED.json" ]; then
    mv "$TREE/data/cold/manifests/venue=hyperliquid/date=$DATE_DASHED.json" "$Q/manifests/"
fi
for s in BTC ETH; do
    p="$TREE/data/cold/trades/venue=hyperliquid/symbol=$s/date=$DATE_DASHED/part-000.parquet"
    if [ -f "$p" ]; then
        mv "$p" "$Q/trades/venue=hyperliquid_symbol=${s}_date=${DATE_DASHED}_part-000.parquet"
    fi
done
echo "  quarantined -> $Q"
ls -la "$Q/manifests" "$Q/trades"

echo "[4/6] Re-compact with deployed fixed binary (as mp-egress: the crontab user that owns data/cold and runs maintenance)"
su mp-egress -c "cd $TREE && $BIN compact --date $DATE_DASHED --venue hyperliquid --symbol BTC"
su mp-egress -c "cd $TREE && $BIN compact --date $DATE_DASHED --venue hyperliquid --symbol ETH"

echo "[5/6] Verify"
find "$TREE/data/cold/trades" -path "*$DATE_DASHED*" -name "*.parquet" -printf "  %s %p\n"
python3 - "$TREE/data/cold/manifests/venue=hyperliquid/date=$DATE_DASHED.json" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
for k, v in sorted(m["streams"].items()):
    if k.startswith("trades"):
        print(f"  manifest {k}: events={v['events']} coverage={v['coverage']}")
PY
su mp-egress -c "cd $TREE && $BIN prune --date $DATE_DASHED --venue hyperliquid --symbol BTC --dry-run"
su mp-egress -c "cd $TREE && $BIN prune --date $DATE_DASHED --venue hyperliquid --symbol ETH --dry-run"

echo "[6/6] Cleanup: remove restored raws (return to released state; desktop holds the archive)"
rm -f "$TREE/data/raw/${DATE_FLAT}_hyperliquid_BTC.log" "$TREE/data/raw/${DATE_FLAT}_hyperliquid_ETH.log"
rm -f "$STAGE/${DATE_FLAT}_hyperliquid_BTC.log" "$STAGE/${DATE_FLAT}_hyperliquid_ETH.log"
echo "  restored raws removed from VPS raw dir + stage"
echo "  Done."
