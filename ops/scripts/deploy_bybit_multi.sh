#!/bin/bash
# Deploy the bybit ETHUSDT + SOLUSDT collector legs + gate wiring (2026-08-15,
# spec 032 multi-symbol direction: same template unit, per-symbol instances).
# Run on the VPS with root privileges:
#   bash /home/mp-egress/deploy_bybit_multi.sh
# The staged edits live in /home/mp-egress/mp-build (mp-egress-owned); this
# script installs them into printer/root-owned locations.
#
# 2026-08-15 (OOM fix, same deploy): the nightly determinism replay
# (mp-determinism, spec 018) loads every required day-log into memory and
# replays twice — the 08-14 gate OOM-killed (exit 137) on this 952 MiB box,
# and the gate gains two more bybit logs tonight. This script therefore also
# grows swap idempotently (swapfile2, 8 GiB; 21 GB free on /) so the replay
# and the subsequent 5-log compaction fit. Idempotent: safe to re-run.
set -euo pipefail

# Self-elevation guard (established COL-29 pattern, cf. deploy_bybit_fix.sh):
# mp-egress is in google-sudoers (GCP NOPASSWD), so one ssh command as
# mp-egress can run the whole deploy. Fail fast (-n) rather than hang if a
# password were ever required.
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        exec sudo -n bash "$0" "$@"
    fi
    echo "ERROR: this deploy needs root; run it from an elevated shell on the VPS." >&2
    exit 1
fi

BIN_DIR=/opt/money-printer/bin
GATE_SRC=/home/mp-egress/mp-build/ops/scripts/daily_maintenance.sh
GATE_DST=/opt/money-printer/ops/scripts/daily_maintenance.sh

echo "[1/5] Ensure swap headroom for the nightly determinism replay (OOM fix)"
if [ ! -f /swapfile2 ]; then
    fallocate -l 8G /swapfile2
    chmod 600 /swapfile2
    mkswap /swapfile2 >/dev/null
    echo '/swapfile2 none swap sw 0 0' >> /etc/fstab
    swapon /swapfile2
    echo "  created + enabled /swapfile2 (8 GiB)"
else
    if ! grep -q '^/swapfile2 ' /proc/swaps; then
        swapon /swapfile2
        echo "  enabled existing /swapfile2"
    else
        echo "  /swapfile2 already active (idempotent skip)"
    fi
fi
free -h | sed -n '1,3p'

echo "[2/5] Install updated daily gate script (bybit:ETHUSDT + bybit:SOLUSDT in RECORDINGS)"
install -m 0755 "$GATE_SRC" "$GATE_DST"
grep -q "bybit:ETHUSDT" "$GATE_DST" && grep -q "bybit:SOLUSDT" "$GATE_DST" \
    && echo "  gate RECORDINGS OK" || { echo "  GATE UPDATE FAILED"; exit 1; }

echo "[3/5] Config-check both symbols with the deployed binary"
"$BIN_DIR/mp-collector" --venue bybit --symbol ETHUSDT --check-config 2>&1 | grep -q "config OK" \
    && echo "  ETHUSDT config OK" || { echo "  ETHUSDT CONFIG FAILED"; exit 1; }
"$BIN_DIR/mp-collector" --venue bybit --symbol SOLUSDT --check-config 2>&1 | grep -q "config OK" \
    && echo "  SOLUSDT config OK" || { echo "  SOLUSDT CONFIG FAILED"; exit 1; }

echo "[4/5] Enable + start both units (template mp-collector@%I, venue hardcoded bybit)"
systemctl enable --now mp-collector@ETHUSDT
systemctl enable --now mp-collector@SOLUSDT

echo "[5/5] Verify units are running"
systemctl status mp-collector@ETHUSDT --no-pager | head -4 || true
systemctl status mp-collector@SOLUSDT --no-pager | head -4 || true
echo "  Done. Watch for data: ls /opt/money-printer/data/raw/ | grep bybit"
