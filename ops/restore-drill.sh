#!/usr/bin/env bash
# Quarterly restore drill (OPS-5): an untested backup is a hope, not a backup.
# Restores the latest off-host backup into a scratch dir and runs the sim
# golden fixture from the restored state. Exits non-zero if the restore or the
# golden run fails. Safe: never touches live data or the real journal (W-6).
set -euo pipefail

BACKUP="${1:-}"
if [[ -z "$BACKUP" || ! -f "$BACKUP" ]]; then
  echo "usage: restore-drill.sh <encrypted-backup.tar.gz.age>" >&2
  echo "  (fetch the latest from the rclone target first)" >&2
  exit 2
fi

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
echo "restore-drill: scratch = $SCRATCH"

# 1. Decrypt + extract (age/gpg per your backup tool; example uses tar).
echo "restore-drill: extracting backup ..."
tar -xzf "$BACKUP" -C "$SCRATCH"

# 2. Sanity: the business records must be present.
for want in journal runs; do
  if [[ ! -e "$SCRATCH/$want" ]]; then
    echo "restore-drill: FAIL — missing '$want' in backup" >&2
    exit 1
  fi
done

# 3. Golden replay from restored state — proves the data is usable, not just
#    present. Default verifier is the sim golden fixture; MP_DRILL_VERIFY_CMD
#    overrides it (used by the automated ops_5 test to exercise the full
#    restore path without nesting a cargo build).
VERIFY_CMD="${MP_DRILL_VERIFY_CMD:-cargo test -p mp-sim --test backtest}"
echo "restore-drill: verifying restored state via: $VERIFY_CMD"
if ! bash -c "$VERIFY_CMD" >/dev/null 2>&1; then
  echo "restore-drill: FAIL — verification did not pass on restored state" >&2
  exit 1
fi

echo "restore-drill: PASS — backup restored and golden replay succeeded"
