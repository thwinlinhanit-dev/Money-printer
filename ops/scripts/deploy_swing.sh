#!/bin/bash
# Deploy the SWING-ONLY collector set alongside the Phase-0 recorder
# (Option 1, docs/SWING_DATA_PLAN.md — owner decision 2026-08-22).
#
# What this does on the VPS:
#   - REPLACES the full-mode bybit legs (mp-collector@{BTCUSDT,ETHUSDT,SOLUSDT})
#     with swing_only units (mp-swing@bybit-*): trades + funding/mark/OI +
#     allLiquidation, NO L2 orderbook.50 stream (the dominant disk consumer —
#     frees roughly 2.5 GB/day on a disk that was at 87%).
#   - KEEPS mp-hyperliquid@BTC/@ETH (the Phase-0 promotion-gate recordings,
#     book stream included) and mp-whale untouched — data keeps compounding.
#   - ENABLES the FRED macro leg (mp-swing-macro) ONLY when FRED_API_KEY is in
#     venues.env (fail-closed, spec 030 MAC-2; same pattern as mp-netflow).
#
# Single-writer invariant: every {venue}:{symbol} day-file has exactly one
# writer host, so the nightly Windows drain never sees same-name/different-
# bytes collisions from this swap (the Aug 12–17 hyperliquid backlog lesson).
#
# Staged tree: /home/mp-egress/mp-build (mp-egress-owned). Self-elevates
# (google-sudoers NOPASSWD, COL-29 pattern). Idempotent: safe to re-run.
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
CARGO=/home/mp-egress/.cargo/bin/cargo
export CARGO_BUILD_JOBS=2

echo "[1/6] Install swing sources + configs into $TREE (data/ untouched)"
install -d -o printer -g printer "$TREE/collectors/swing"
for f in "$SRC"/collectors/swing/*.toml; do
    install -m 0644 -o printer -g printer "$f" "$TREE/collectors/swing/"
done
install -m 0644 -o printer -g printer "$SRC/collectors/src/bin/mp-collector.rs" \
    "$TREE/collectors/src/bin/mp-collector.rs"
echo "  configs + mp-collector.rs OK"

echo "[2/6] Build mp-collector + mp-macro (live-ws,live-http)"
su mp-egress -c "cd $SRC && PATH=/home/mp-egress/.cargo/bin:\$PATH $CARGO build --release --features live-ws,live-http --bin mp-collector --bin mp-macro"

echo "[3/6] Install binaries"
for b in mp-collector mp-macro; do
    install -m 0755 -o printer -g printer "$SRC/target/release/$b" "$BIN_DIR/$b"
done
echo "  binaries OK"

echo "[4/6] Install units"
install -m 0644 "$SRC/ops/systemd/mp-swing@.service" \
    "$SRC/ops/systemd/mp-swing-macro.service" /etc/systemd/system/
systemctl daemon-reload
echo "  units OK"

echo "[5/6] Unit swap (single writer per symbol)"
systemctl stop mp-collector@BTCUSDT mp-collector@ETHUSDT mp-collector@SOLUSDT 2>/dev/null || true
systemctl disable mp-collector@BTCUSDT mp-collector@ETHUSDT mp-collector@SOLUSDT 2>/dev/null || true
sleep 2
# Stale locks would block the swing units' first spawn.
rm -f "$TREE/data/raw/.lock_bybit_BTCUSDT" "$TREE/data/raw/.lock_bybit_ETHUSDT" \
      "$TREE/data/raw/.lock_bybit_SOLUSDT"
systemctl enable --now mp-swing@bybit-btcusdt mp-swing@bybit-ethusdt mp-swing@bybit-solusdt
if grep -q '^FRED_API_KEY=.\+' /etc/money-printer/venues.env 2>/dev/null; then
    systemctl enable --now mp-swing-macro
    echo "  mp-swing-macro enabled (FRED_API_KEY present)"
else
    echo "  mp-swing-macro NOT enabled: FRED_API_KEY missing from venues.env (spec 030 fail-closed; enable after provisioning)"
fi
# Phase-0 + whale must be UNTOUCHED and running.
systemctl enable --now mp-hyperliquid@BTC mp-hyperliquid@ETH mp-whale

echo "[6/6] Verify"
sleep 5
FAIL=0
for u in mp-hyperliquid@BTC mp-hyperliquid@ETH mp-whale \
         mp-swing@bybit-btcusdt mp-swing@bybit-ethusdt mp-swing@bybit-solusdt; do
    if systemctl is-active "$u" >/dev/null 2>&1; then
        echo "  active: $u"
    else
        echo "  UNIT NOT ACTIVE: $u"
        FAIL=1
    fi
done
[ "$FAIL" -eq 0 ] || exit 1
if systemctl is-active mp-swing-macro >/dev/null 2>&1; then
    echo "  active: mp-swing-macro"
fi
"$BIN_DIR/mp-collector" --version
echo "  Done. Watch: ls $TREE/data/raw/ | tail; journalctl -u 'mp-swing@*' -f"
