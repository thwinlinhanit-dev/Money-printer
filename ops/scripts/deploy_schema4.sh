#!/bin/bash
# Deploy the schema-4 release + netflow + whale census to the VPS recorder
# (2026-08-18): TradeWithAddr + NetflowSnapshot event variants (spec 033/034,
# SCHEMA_VER 4), the Etherscan netflow poller (mp-netflow), the microstructure
# features catalog (mp-materialize), and the mp-whale census restore (spec 028
# — the whale edge has been dark since the Windows watchdog retirement).
#
# Staged tree: /home/mp-egress/mp-build (mp-egress-owned). This script
# self-elevates (google-sudoers NOPASSWD, established COL-29 pattern) and is
# idempotent: safe to re-run.
#
# MP_ETHERSCAN_KEY must live in /etc/money-printer/venues.env (PD-2). The
# mp-netflow unit is enabled ONLY when the key is present (fail-closed
# runtime otherwise, spec 034).
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        exec sudo -n bash "$0" "$@"
    fi
    echo "ERROR: this deploy needs root; run it from an elevated shell on the VPS." >&2
    exit 1
fi

SRC=/home/mp-egress/mp-build
TREE=/opt/money-printer
BIN_DIR=$TREE/bin
BUILD_DIRS="core collectors features storage sim ops strategies oms risk llm"
BUILD_FILES="Cargo.toml Cargo.lock rust-toolchain.toml"
CARGO=/home/mp-egress/.cargo/bin/cargo
export CARGO_BUILD_JOBS=2

echo "[1/7] Install updated sources into $TREE (data/ untouched)"
for d in $BUILD_DIRS; do
    rm -rf "$TREE/$d"
    cp -a "$SRC/$d" "$TREE/$d"
    chown -R printer:printer "$TREE/$d"
done
# The staged tree comes from a Windows-originated git checkout (no exec
# bits); cron invokes ops scripts directly, so restore +x or the daily
# maintenance and hourly stale checks die silently (2026-08-19 incident:
# 00:05 run failed, no 08-18 scorecard, stale alerting dark for 18h).
find "$TREE/ops" -name "*.sh" -exec chmod +x {} +
for f in $BUILD_FILES; do
    install -m 0644 -o printer -g printer "$SRC/$f" "$TREE/$f"
done
echo "  sources OK (schema-4: core/src/{event,lib,log}.rs, collectors etherscan/netflow)"

echo "[2/7] Build collectors (live-ws,live-http): mp-collector + mp-whale + mp-netflow"
su mp-egress -c "cd $SRC && PATH=/home/mp-egress/.cargo/bin:\$PATH $CARGO build --release --features live-ws,live-http --bin mp-collector --bin mp-whale --bin mp-netflow"

echo "[3/7] Build gate + tooling: mp-ops, storage bins, mp-determinism"
su mp-egress -c "cd $SRC && PATH=/home/mp-egress/.cargo/bin:\$PATH $CARGO build --release -p mp-ops -p mp-storage --bin mp-materialize --bin mp-cross-venue --bin mp-audit --bin mp-query -p mp-sim --bin mp-determinism"

echo "[4/7] Install binaries"
for b in mp-collector mp-whale mp-netflow mp-ops mp-materialize mp-cross-venue mp-audit mp-query mp-determinism; do
    install -m 0755 -o printer -g printer "$SRC/target/release/$b" "$BIN_DIR/$b"
done
echo "  binaries OK"

echo "[5/7] Install units + netflow config"
install -m 0644 "$SRC/ops/systemd/mp-whale.service" "$SRC/ops/systemd/mp-netflow.service" "$SRC/ops/systemd/mp-hyperliquid@.service" "$SRC/ops/systemd/mp-collector@.service" /etc/systemd/system/
install -m 0644 -o printer -g printer "$SRC/collectors/netflow.toml.example" "$TREE/collectors/netflow.toml"
systemctl daemon-reload
echo "  units OK"

echo "[6/7] Start/restart units (market units get the schema-4 binary)"
systemctl enable --now mp-whale
if grep -q '^MP_ETHERSCAN_KEY=.\+' /etc/money-printer/venues.env 2>/dev/null; then
    systemctl enable --now mp-netflow
    echo "  mp-netflow enabled (MP_ETHERSCAN_KEY present)"
else
    echo "  mp-netflow NOT enabled: MP_ETHERSCAN_KEY missing from venues.env (spec 034 fail-closed; enable after provisioning)"
fi
systemctl restart mp-hyperliquid@BTC mp-hyperliquid@ETH mp-collector@BTCUSDT mp-collector@ETHUSDT mp-collector@SOLUSDT

echo "[7/7] Verify"
sleep 5
for u in mp-hyperliquid@BTC mp-hyperliquid@ETH mp-collector@BTCUSDT mp-collector@ETHUSDT mp-collector@SOLUSDT mp-whale; do
    systemctl is-active "$u" >/dev/null || { echo "  UNIT $u NOT ACTIVE"; exit 1; }
done
echo "  all market units + mp-whale active"
if systemctl is-active mp-netflow >/dev/null 2>&1; then
    echo "  mp-netflow active"
fi
"$BIN_DIR/mp-collector" --version
"$BIN_DIR/mp-whale" --version
"$BIN_DIR/mp-netflow" --version
echo "  Done. Logs: ls $TREE/data/raw/ | tail; watch $TREE/data/raw/ for *_netflow_ethereum.log and *_hyperliquid_positions.log"