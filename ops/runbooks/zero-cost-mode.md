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

## Disk Monitoring

### Check current usage

```bash
df -h /opt/money-printer/data
```

### Automatic cleanup

The daily pipeline enforces retention:
- Hot tier (last 7-14 days): oldest tick data deleted when budget exceeded
- Warm tier (last 30-60 days): oldest bars downsampled when budget exceeded
- Cold tier (PC only): manual cleanup only (W-6)

### Manual cleanup (emergency)

```bash
# Find and report old raw files
find data/raw -name "*.log" -mtime +14 -ls

# Check Parquet size
du -sh data/features/ data/cold/
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

| Check | Command | Frequency |
|---|---|---|
| Collector health | `systemctl status mp-hyperliquid@*` | Daily |
| Disk usage | `df -h` | Daily |
| Daily scorecard | `mp-ops scorecard --date $(date -d yesterday +%Y%m%d)` | Daily |
| Storage budget | `mp-ops storage-budget` | Weekly |
| Promotion streak | `mp-ops promote --dry-run` | Weekly |

## References

- `docs/ZERO_COST_MODE.md` — Zero-Cost Mode overview
- `docs/RETENTION_POLICY.md` — retention policy
- `docs/SWING_DATA_PLAN.md` — swing collector topology
- `ops/runbooks/disk-high.md` — disk high runbook
- `ops/runbooks/storage-budget.md` — storage budget runbook
- `ops/runbooks/collector-down.md` — collector down runbook
