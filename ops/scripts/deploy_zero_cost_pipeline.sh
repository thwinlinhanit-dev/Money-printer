#!/bin/bash
# deploy_zero_cost_pipeline.sh - Deploy updated daily_maintenance.sh to the VPS
# and verify the Zero-Cost mode is active for the next cron run.
#
# Run from the Windows host (has SSH access to VPS) or from the VPS itself.
#
# Usage:
#   # From Windows (PowerShell):
#   bash ops/scripts/deploy_zero_cost_pipeline.sh $env:MP_VPS_HOST
#
#   # From VPS directly:
#   bash ops/scripts/deploy_zero_cost_pipeline.sh localhost
#
#   # Dry-run only (verify, no copy):
#   bash ops/scripts/deploy_zero_cost_pipeline.sh $env:MP_VPS_HOST --dry-run
set -euo pipefail

VPS_HOST="${1:-}"
SSH_USER="${SSH_USER:-printer}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
REMOTE_BASE="/opt/money-printer"
DRY_RUN=false

if [[ "${2:-}" == "--dry-run" ]]; then
    DRY_RUN=true
fi

if [[ -z "$VPS_HOST" ]]; then
    echo "[ERROR] Usage: $0 <vps-host> [--dry-run]"
    echo "  Set MP_VPS_HOST or pass the host as first argument."
    exit 1
fi

SSH_OPTS="-o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new"
if [[ -f "$SSH_KEY" ]]; then
    SSH_OPTS="$SSH_OPTS -i $SSH_KEY"
fi

ssh_cmd() {
    ssh $SSH_OPTS "$SSH_USER@$VPS_HOST" "$@"
}

scp_cmd() {
    scp $SSH_OPTS "$@"
}

echo "=== Zero-Cost Pipeline Deployment ==="
echo "VPS: $SSH_USER@$VPS_HOST"
echo "Dry run: $DRY_RUN"
echo ""

# ---- 1. Pre-flight: verify the VPS is reachable ----
echo "[1/6] Checking VPS connectivity..."
if ! ssh_cmd "echo ok" >/dev/null 2>&1; then
    echo "[ERROR] Cannot reach VPS at $SSH_USER@$VPS_HOST"
    echo "  Check: MP_VPS_HOST, SSH key, network."
    exit 1
fi
echo "  VPS reachable."

# ---- 2. Check current script (before copy) ----
echo "[2/6] Current script on VPS:"
CURRENT_ZC=$(ssh_cmd "grep -c 'ZERO_COST' '$REMOTE_BASE/ops/scripts/daily_maintenance.sh' 2>/dev/null || echo 0")
echo "  ZERO_COST references: $CURRENT_ZC"
if [[ "$CURRENT_ZC" -gt 0 ]]; then
    echo "  NOTE: Script already has Zero-Cost support."
    echo "  This deploy will overwrite with the latest version."
fi

# ---- 3. Backup the current script ----
echo "[3/6] Backing up current script..."
BACKUP_NAME="daily_maintenance.sh.bak.$(date +%Y%m%d%H%M%S)"
ssh_cmd "cp '$REMOTE_BASE/ops/scripts/daily_maintenance.sh' '$REMOTE_BASE/ops/scripts/$BACKUP_NAME' 2>/dev/null || true"
echo "  Backup: $BACKUP_NAME"

# ---- 4. Copy the updated script ----
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LOCAL_SCRIPT="$SCRIPT_DIR/daily_maintenance.sh"

if [[ ! -f "$LOCAL_SCRIPT" ]]; then
    echo "[ERROR] Local script not found: $LOCAL_SCRIPT"
    exit 1
fi

if [[ "$DRY_RUN" == "true" ]]; then
    echo "[4/6] DRY RUN - would copy: $LOCAL_SCRIPT -> $REMOTE_BASE/ops/scripts/daily_maintenance.sh"
else
    echo "[4/6] Copying updated script..."
    scp_cmd "$LOCAL_SCRIPT" "$SSH_USER@$VPS_HOST:$REMOTE_BASE/ops/scripts/daily_maintenance.sh"
    ssh_cmd "chmod 755 '$REMOTE_BASE/ops/scripts/daily_maintenance.sh'"
    echo "  Copied and chmod 755."
fi

# ---- 5. Verify the Zero-Cost branch is present ----
echo "[5/6] Verifying Zero-Cost support in deployed script..."
VERIFY=$(ssh_cmd "grep -c 'ZERO_COST' '$REMOTE_BASE/ops/scripts/daily_maintenance.sh' 2>/dev/null || echo 0")
echo "  ZERO_COST references: $VERIFY"
if [[ "$VERIFY" -lt 3 ]]; then
    echo "[ERROR] Deployed script does not have expected Zero-Cost support (found $VERIFY refs, expected >= 3)."
    exit 1
fi

# Check that --zero-cost is passed to scorecard and promote
HAS_SCORECARD_ZC=$(ssh_cmd "grep -c '\\-\\-zero-cost' '$REMOTE_BASE/ops/scripts/daily_maintenance.sh' 2>/dev/null || echo 0")
echo "  --zero-cost flag references: $HAS_SCORECARD_ZC"
if [[ "$HAS_SCORECARD_ZC" -lt 2 ]]; then
    echo "[ERROR] --zero-cost is not passed to both scorecard and promote."
    exit 1
fi

# Check hot-tier retention block exists
HAS_RETENTION=$(ssh_cmd "grep -c 'Hot-tier retention' '$REMOTE_BASE/ops/scripts/daily_maintenance.sh' 2>/dev/null || echo 0")
echo "  Hot-tier retention block: $HAS_RETENTION"
if [[ "$HAS_RETENTION" -lt 1 ]]; then
    echo "[ERROR] Hot-tier retention enforcement block not found."
    exit 1
fi

echo "  All checks passed."

# ---- 6. Verify cron schedule ----
echo "[6/6] Verifying cron schedule..."
CRON_LINE=$(ssh_cmd "crontab -l 2>/dev/null | grep 'daily_maintenance' || echo NOT_FOUND")
echo "  Cron entry: $CRON_LINE"
if [[ "$CRON_LINE" == "NOT_FOUND" ]]; then
    echo "[WARN] No cron entry for daily_maintenance.sh found. The script is deployed but will not run automatically."
    echo "  To add: crontab -e  # add: 5 0 * * * /opt/money-printer/ops/scripts/daily_maintenance.sh"
fi

echo ""
echo "=== Deployment Complete ==="
echo ""
echo "Next cron run will execute with:"
echo "  ZERO_COST=1 (default)"
echo "  RECORDINGS=hyperliquid:BTC hyperliquid:ETH"
echo "  REQUIRED_STREAMS=trade funding mark_price open_interest"
echo "  --zero-cost flag passed to mp-ops scorecard and promote"
echo "  Hot-tier retention: raw logs > 14 days deleted"
echo ""
echo "To verify manually before the next cron run:"
echo "  ssh $SSH_USER@$VPS_HOST 'cd $REMOTE_BASE && ZERO_COST=1 bash ops/scripts/daily_maintenance.sh 2>&1 | tail -20'"
echo ""
echo "To rollback:"
echo "  ssh $SSH_USER@$VPS_HOST 'cp $REMOTE_BASE/ops/scripts/$BACKUP_NAME $REMOTE_BASE/ops/scripts/daily_maintenance.sh'"
