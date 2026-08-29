#!/bin/bash
# Deploy the DeFiLlama regime collector (spec 046) and Coinalyze
# cross-exchange validation collector (spec 047) to the VPS.
#
# What this does on the VPS:
#   - Builds mp-defillama + mp-coinalyze (live-http feature)
#   - Installs binaries, systemd units, and config files
#   - Installs/updates collector source for the build tree
#   - Starts mp-defillama immediately (keyless, no env needed)
#   - Starts mp-coinalyze ONLY when COINALYZE_API_KEY is in venues.env
#     (fail-closed, COZ-1/PD-2 — same pattern as mp-netflow)
#
# Staged tree: /home/mp-egress/mp-build (mp-egress-owned). Self-elevates
# (google-sudoers NOPASSWD, established COL-29 pattern). Idempotent.
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

echo "[1/6] Install updated collector sources into $TREE"
for d in core collectors; do
    rm -rf "$TREE/$d"
    cp -a "$SRC/$d" "$TREE/$d"
    chown -R printer:printer "$TREE/$d"
done
for f in Cargo.toml Cargo.lock rust-toolchain.toml; do
    install -m 0644 -o printer -g printer "$SRC/$f" "$TREE/$f"
done
# Restore exec bits on ops scripts (Windows checkout loses them).
find "$TREE/ops" -name "*.sh" -exec chmod +x {} + 2>/dev/null || true
echo "  sources OK"

echo "[2/6] Build mp-defillama + mp-coinalyze (live-http)"
su mp-egress -c "cd $SRC && PATH=/home/mp-egress/.cargo/bin:$PATH $CARGO build --release --features live-http --bin mp-defillama --bin mp-coinalyze"

echo "[3/6] Install binaries"
for b in mp-defillama mp-coinalyze; do
    install -m 0755 -o printer -g printer "$SRC/target/release/$b" "$BIN_DIR/$b"
done
echo "  binaries OK"

echo "[4/6] Install units + configs"
install -m 0644 "$SRC/ops/systemd/mp-defillama.service" \
    "$SRC/ops/systemd/mp-coinalyze.service" /etc/systemd/system/
# DeFiLlama config (keyless — no secrets)
install -m 0644 -o printer -g printer \
    "$SRC/collectors/defillama.toml.example" "$TREE/collectors/defillama.toml"
# Coinalyze config (API key comes from env only — never in this file)
install -m 0644 -o printer -g printer \
    "$SRC/collectors/coinalyze.toml.example" "$TREE/collectors/coinalyze.toml"
systemctl daemon-reload
echo "  units + configs OK"

echo "[5/6] Start/restart units"
# DeFiLlama: keyless, always safe to enable.
systemctl enable --now mp-defillama
echo "  mp-defillama enabled (keyless)"

# Coinalyze: fail-closed on missing key (COZ-1, PD-2, same as mp-netflow).
if grep -q '^COINALYZE_API_KEY=.\\+' /etc/money-printer/venues.env 2>/dev/null; then
    systemctl enable --now mp-coinalyze
    echo "  mp-coinalyze enabled (COINALYZE_API_KEY present)"
else
    systemctl disable mp-coinalyze 2>/dev/null || true
    echo "  mp-coinalyze NOT enabled: COINALYZE_API_KEY missing from venues.env (spec 047 fail-closed; enable after provisioning)"
fi

echo "[6/6] Verify"
sleep 5
FAIL=0

if systemctl is-active mp-defillama >/dev/null 2>&1; then
    echo "  active: mp-defillama"
else
    echo "  UNIT NOT ACTIVE: mp-defillama"
    FAIL=1
fi

if systemctl is-active mp-coinalyze >/dev/null 2>&1; then
    echo "  active: mp-coinalyze"
else
    echo "  mp-coinalyze not active (expected if COINALYZE_API_KEY is not yet provisioned)"
fi

[ "$FAIL" -eq 0 ] || exit 1

"$BIN_DIR/mp-defillama" --version
"$BIN_DIR/mp-coinalyze" --version
echo "  Done. Watch:"
echo "    ls $TREE/data/raw/ | grep -E 'defillama|coinalyze'"
echo "    journalctl -u mp-defillama -f"
echo "    journalctl -u mp-coinalyze -f"
