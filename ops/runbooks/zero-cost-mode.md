# Zero-Cost Mode Runbook
#
# How to operate the system under $0 budget constraints.
# Applies to: Google Cloud e2-micro (1 GB RAM / 30 GB disk)
# or Oracle Always Free (2 OCPU / 12 GB / 200 GB).

## Starting Zero-Cost Collectors

### On VPS (recommended)

The swing collectors are the recommended path under Zero-Cost Mode:

```bash
# Deploy swing set (includes Hyperliquid BTC + ETH)
bash ~/mp-build/ops/scripts/deploy_swing.sh

# Verify all units are active
systemctl status mp-hyperliquid@BTC mp-hyperliquid@ETH mp-whale
```

### On Windows (isolated host fallback)

```powershell
.\swing_collectors.ps1                 # build + supervise (foreground)
.\swing_collectors.ps1 -RegisterTask   # auto-start at logon/startup
```

### Manual start

```bash
cargo run -p mp-collectors --features live-ws --bin mp-collector -- \
  --config collectors/zero_cost/hyperliquid-btc.toml

cargo run -p mp-collectors --features live-ws --bin mp-collector -- \
  --config collectors/zero_cost/hyperliquid-eth.toml
```

## Disk Monitoring & Alerts

The 30 GB free-tier cap requires proactive monitoring. Two layers:
1. **`df -h`** — immediate filesystem-level free space (P2 when > 85% used)
2. **`mp-ops storage-budget`** — forward-looking growth projection (P2 when
   projected to hit cap within 14 days)

Both deliver alerts via Telegram so failures reach your phone, not just
journald.

### 1. Filesystem check (`df -h`)

```bash
df -h /opt/money-printer/data
```

| Free space | Action |
|---|---|
| > 15% (4.5 GB) | Normal — no action needed |
| 5–15% (1.5–4.5 GB) | WARN — check growth rate, run compactor |
| < 5% (1.5 GB) | **P1-adjacent** — stop collectors to protect event log |

Triggered by: `disk-high` alert (OPS-7, P2). See `ops/runbooks/disk-high.md`.

### 2. Storage budget projection (`mp-ops storage-budget`)

The forward-looking watch (OPS-15) sums daily corpus sizes from raw file
names and projects when the growth rate will hit the cap.

```bash
# Zero-Cost budget: 30 GB (full free-tier disk)
MP_STORAGE_BUDGET_BYTES=30000000000 mp-ops storage-budget \
  --dir /opt/money-printer/data/raw \
  --cap-bytes 30000000000 \
  --telegram
```

**What it reports:**
- Current corpus size (e.g. "8.2 GB used")
- Trailing growth rate (e.g. "growing 0.58 GB/day")
- Projected days to cap (e.g. "hits 30 GB cap in 37.6 days")
- Alert horizon: fires P2 when projection < 14 days to cap

**Set the env var for automation:**
```bash
# In /etc/money-printer/ops.env (mode 0600)
echo 'MP_STORAGE_BUDGET_BYTES=30000000000' >> /etc/money-printer/ops.env
```

The daily pipeline (`daily_maintenance.sh`) reads `MP_STORAGE_BUDGET_BYTES`
from the environment and runs `storage-budget --telegram` automatically.

See `ops/runbooks/storage-budget.md` for the full projection math and
remediation steps.

### 3. Automated daily check (cron)

Add to crontab (`crontab -e`) for a standalone daily disk check that runs
independently of the pipeline:

```cron
# Disk budget watch — fires Telegram P2 when growth projection hits 30 GB
# cap within 14 days (runs at 06:00 UTC, before the 07:30 pipeline gate)
0 6 * * * cd /opt/money-printer && MP_STORAGE_BUDGET_BYTES=30000000000 bin/mp-ops storage-budget --dir data/raw --cap-bytes 30000000000 --telegram >> data/scorecards/disk_budget.log 2>&1
```

### 4. Manual checks

```bash
# Immediate disk state
df -h /opt/money-printer/data

# Current corpus size
du -sh /opt/money-printer/data/raw/

# Per-venue breakdown
du -sh /opt/money-printer/data/raw/*_hyperliquid_*.log 2>/dev/null | tail -5

# Full budget projection (no Telegram)
MP_STORAGE_BUDGET_BYTES=30000000000 bin/mp-ops storage-budget --dir data/raw --cap-bytes 30000000000
```

### Automatic cleanup

The daily pipeline enforces retention when `ZERO_COST=1`:
- Hot tier (last 14 days): oldest raw `.log` files deleted automatically
- Compacted Parquet in `data/parquet/` and features in `data/features/`
  are kept (orders of magnitude smaller)
- Cold tier (PC only): manual cleanup only (W-6)

### Manual cleanup (emergency)

```bash
# Find and report old raw files
find data/raw -name "*.log" -mtime +14 -ls

# Check Parquet/feature size
du -sh data/features/ data/parquet/ data/cold/

# Run compactor if behind
bin/mp-ops compact --data-dir data/raw
```

## When Free VPS Is Reclaimed

1. **Data is safe on PC** — VPS drain runs nightly, master corpus is on Windows
2. **Re-provision** a new free-tier VPS (Google or Oracle)
3. **Restore** from off-host backup:
   ```bash
   # Pull from off-host
   rclone copy offhost:offhost/ /tmp/restore/
   age -d -i ops/keys/offhost.agekey /tmp/restore/*.age > /tmp/restore/raw.tar
   tar xf /tmp/restore/raw.tar -C /opt/money-printer/data/
   ```
4. **Re-deploy** collectors: `bash ~/mp-build/ops/scripts/deploy_swing.sh`
5. **Resume** daily pipeline

## When Disk Fills

1. Run `mp-ops storage-budget` to check projection
2. Run compactor: `mp-ops compact --data-dir data/raw`
3. Ship to off-host: `.\offhost_backup.ps1`
4. **Human deletes** verified-migrated data (W-6: never auto-delete)

## What to Monitor

| Check | Command | Frequency | Alert |
|---|---|---|---|
| Collector health | `systemctl status mp-hyperliquid@*` | Daily | Journald |
| Disk free space | `df -h /opt/money-printer/data` | Daily (cron) | Telegram P2 (> 85% used) |
| Storage budget | `mp-ops storage-budget --cap-bytes 30000000000 --telegram` | Daily (cron) | Telegram P2 (< 14 days to cap) |
| Hot-tier retention | `daily_maintenance.sh` (ZERO_COST=1) | Daily (pipeline) | Auto-prune > 14 days |
| Daily scorecard | `mp-ops scorecard --date $(date -d yesterday +%Y%m%d)` | Daily (pipeline) | Telegram P2 |
| Promotion streak | `mp-ops promote --dry-run` | Weekly | Telegram P3 |

## References

- `docs/ZERO_COST_MODE.md` — Zero-Cost Mode overview
- `docs/RETENTION_POLICY.md` — retention policy
- `docs/SWING_DATA_PLAN.md` — swing collector topology
- `ops/runbooks/disk-high.md` — disk high runbook
- `ops/runbooks/storage-budget.md` — storage budget runbook
- `ops/runbooks/collector-down.md` — collector down runbook
