#!/bin/bash
# deploy_retention_fix.sh — Deploy the 2026-09-03 gate-tooling delta to the
# VPS recorder and repair the featureless mp-whale/mp-macro install from the
# 2026-09-01 08:29 deploy (mp-whale was byte-identical to mp-macro; both crash
# with "'live-http' feature required (REST poller)" — restart counter ~97k).
#
# Ships local HEAD 5f8fd7d (docs/AUDIT-2026-09-02 findings A-1/A-12 + the
# schema-4 TradeWithAddr compactor fix):
#   - ops: hot-tier retention routes through `mp-ops prune` (hash-verified
#     compact proof, RETENTION_POLICY.md) — no more mtime-only deletion
#   - ops: `verify_prunable` COLD-root fix (d78fb85) so proof is actually found
#   - storage: TradeWithAddr rows no longer dropped by the trades compactor
#     (schema-4 hollow-parquet corruption, 0 rows / 1,126-byte shells)
#   - features: corr + oi_regime consumers see TradeWithAddr (same blind spot)
#   - mp-whale/mp-macro rebuilt with live-ws,live-http (heals the crash loop)
#
# Market units (mp-hyperliquid@BTC/ETH, mp-swing@bybit-*) are NOT restarted:
# binaries are swapped on disk and running recorders keep their inode, so no
# mid-day day-file split (single-writer invariant). mp-whale/mp-swing-macro
# heal via Restart=always once the fixed binary lands (no manual restart).
#
# Staged tree: /home/mp-egress/mp-build (mp-egress-owned, synced from the
# Windows-originated git checkout via `git archive`). Self-elevates
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
BUILD_DIRS="core collectors features storage sim ops strategies oms risk llm"
BUILD_FILES="Cargo.toml Cargo.lock rust-toolchain.toml"
CARGO=/home/mp-egress/.cargo/bin/cargo
export CARGO_BUILD_JOBS=2

echo "[1/6] Install updated sources into $TREE (data/ untouched)"
for d in $BUILD_DIRS; do
    rm -rf "$TREE/$d"
    cp -a "$SRC/$d" "$TREE/$d"
    chown -R printer:printer "$TREE/$d"
done
# The staged tree comes from a Windows-originated git checkout (no exec bits);
# cron invokes ops scripts directly, so restore +x (2026-08-19 incident).
find "$TREE/ops" -name "*.sh" -exec chmod +x {} +
for f in $BUILD_FILES; do
    install -m 0644 -o printer -g printer "$SRC/$f" "$TREE/$f"
done
echo "  sources OK (storage parquet_trades.rs, features corr/oi_regime, ops mp-ops.rs + daily_maintenance.sh)"

echo "[2/6] Build gate + tooling (mp-ops, mp-materialize, mp-determinism, ...)"
su mp-egress -c "cd $SRC && PATH=/home/mp-egress/.cargo/bin:\$PATH $CARGO build --release -p mp-ops -p mp-storage -p mp-sim --bin mp-ops --bin mp-materialize --bin mp-cross-venue --bin mp-audit --bin mp-query --bin mp-determinism"

echo "[3/6] Build collectors with live features (mp-collector, mp-whale, mp-macro, mp-netflow)"
su mp-egress -c "cd $SRC && PATH=/home/mp-egress/.cargo/bin:\$PATH $CARGO build --release --features live-ws,live-http --bin mp-collector --bin mp-whale --bin mp-macro --bin mp-netflow"

echo "[4/6] Install binaries"
for b in mp-ops mp-materialize mp-cross-venue mp-audit mp-query mp-determinism mp-collector mp-whale mp-macro mp-netflow; do
    install -m 0755 -o printer -g printer "$SRC/target/release/$b" "$BIN_DIR/$b"
done
echo "  binaries OK"

echo "[5/6] Units: no swap (market units untouched; whale/macro self-heal)"
# No unit files changed in this delta. Explicitly do NOT restart the five
# market recorders (single-writer / day-file-split hazard).

echo "[6/6] Verify"
sleep 3
FAIL=0
for u in mp-hyperliquid@BTC mp-hyperliquid@ETH \
         mp-swing@bybit-btcusdt mp-swing@bybit-ethusdt mp-swing@bybit-solusdt; do
    if systemctl is-active "$u" >/dev/null 2>&1; then
        echo "  active (untouched): $u"
    else
        echo "  UNIT NOT ACTIVE: $u"
        FAIL=1
    fi
done
# Whale/macro come up asynchronously via Restart=always; give them up to ~45s.
for i in $(seq 1 15); do
    if systemctl is-active mp-whale >/dev/null 2>&1; then break; fi
    sleep 3
done
if systemctl is-active mp-whale >/dev/null 2>&1; then
    echo "  active (healed): mp-whale"
else
    echo "  STILL NOT ACTIVE: mp-whale — check: journalctl -u mp-whale -n 20"
    FAIL=1
fi
if systemctl is-active mp-swing-macro >/dev/null 2>&1; then
    echo "  active (healed): mp-swing-macro"
else
    echo "  WARN: mp-swing-macro not active (FRED fail-closed if no key; check: journalctl -u mp-swing-macro -n 10)"
fi
[ "$FAIL" -eq 0 ] || { echo "DEPLOY VERIFY FAILED"; exit 1; }
systemctl reset-failed mp-whale mp-swing-macro 2>/dev/null || true

echo "=== retention gate references in deployed daily_maintenance.sh ==="
grep -c "mp-ops prune" "$TREE/ops/scripts/daily_maintenance.sh" || true
if grep -q "find .* -mtime.* -delete" "$TREE/ops/scripts/daily_maintenance.sh"; then
    echo "ERROR: mtime-only deletion still present in deployed daily_maintenance.sh"
    exit 1
fi
echo "  no mtime-only deletion present (gate enforced)"
echo "=== prune subcommand present in deployed mp-ops? ==="
if "$BIN_DIR/mp-ops" 2>&1 | grep -q "prune"; then
    echo "  prune subcommand present"
else
    echo "ERROR: mp-ops has no prune subcommand"
    exit 1
fi
echo "  Done. Next cron run (00:05 UTC) uses the gated retention."
