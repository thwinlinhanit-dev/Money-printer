#!/bin/bash
# Deploy the bybit BTCUSDT collector leg + gate wiring (2026-08-13, COL-29).
# Run on the VPS with root privileges:
#   bash /home/mp-egress/deploy_bybit.sh
# Builds happened in /home/mp-egress/mp-build (mp-egress-owned); the install
# lands in printer-owned /opt/money-printer and root-owned /etc/systemd/system.
set -euo pipefail

BIN_DIR=/opt/money-printer/bin
BUILD_BIN=/home/mp-egress/mp-build/target/release/mp-collector
BUILD_OPS=/home/mp-egress/mp-build/target/release/mp-ops
UNIT_SRC=/home/mp-egress/mp-build/ops/systemd/mp-collector@.service
GATE_SRC=/home/mp-egress/mp-build/ops/scripts/daily_maintenance.sh
GATE_DST=/opt/money-printer/ops/scripts/daily_maintenance.sh

echo "[1/7] Install rebuilt mp-collector (orderbook fix, COL-29)"
install -m 0755 "$BUILD_BIN" "$BIN_DIR/mp-collector"
"$BIN_DIR/mp-collector" --venue bybit --symbol BTCUSDT --check-config 2>&1 | grep -q "config OK" \
    && echo "  config OK" || { echo "  CONFIG CHECK FAILED"; exit 1; }

echo "[2/7] Install rebuilt mp-ops (venue-scoped --require-stream, COL-29)"
install -m 0755 "$BUILD_OPS" "$BIN_DIR/mp-ops"

echo "[3/7] Install mp-collector@.service unit"
install -m 0644 "$UNIT_SRC" /etc/systemd/system/mp-collector@.service
systemctl daemon-reload

echo "[4/7] Enable mp-collector@BTCUSDT"
systemctl enable --now mp-collector@BTCUSDT

echo "[5/7] Update daily gate script (bybit:BTCUSDT + venue-scoped liquidation requirement)"
install -m 0755 "$GATE_SRC" "$GATE_DST"

echo "[6/7] Verify unit is running"
systemctl status mp-collector@BTCUSDT --no-pager | head -8 || true

echo "[7/7] Done. Watch for data: ls /opt/money-printer/data/raw/ | grep bybit"
