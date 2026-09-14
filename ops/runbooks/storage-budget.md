# storage-budget (P2)

`data/raw` growth is trending toward the storage budget cap — the forward-
looking watch (OPS-15), not the current-usage `disk-high` alert (OPS-7). The
daily corpus sizes are summed from the `{YYYYMMDD}_*.log` file names
(read-only, W-6 — no state file), the trailing-window growth rate is a
trailing-window mean daily addition rate, and the alert fires when the projection puts the corpus at
the `--cap-bytes` budget within the alert horizon (default 14 days), or when
it is already at/over the cap. This is spec 001's appendix "disk budget"
revisit trigger, operationalized: when it fires, the raw corpus is
approaching the size where a wire-format change (the varint appendix) or an
off-host migration decision has to be made.

Since 2026-08-16 the watch also scans the drain manifest
(`--manifest data/vps_drain_manifest.jsonl` — the Windows pipeline hook only;
the VPS has no manifest): any file whose LATEST manifest entry is
`action=landed` with `release` not in {released, no_release} means the drain
landed the file into the master corpus but never released the VPS copy
(`ssh_failed` / `skipped`) — the relay is silently holding byte-verified
files, and the same P2 fires regardless of the growth projection.

## Symptoms
- Alert detail names the corpus size, growth rate, and projected days-to-cap,
  e.g. "data/raw 18.4 GB used, growing 1.15 GB/day; projected to hit the 100
  GB cap in 69.7 days (alert horizon 14 days)".
- Held-file variant (Windows only): the same P2 whose detail names the held
  files, e.g. "data/raw: 1 held VPS file(s) after drain (landed but release
  not confirmed): raw/20260815_bybit_SOLUSDT.log — the relay is still holding
  byte-verified files; check the vps_drain release leg". The verdict also
  carries `held_vps_files` / `held_vps_count`.
- `mp-ops storage-budget --cap-bytes N` prints a JSON verdict with a
  non-null `alert`.

## Deployed budgets (2026-08-14)
- Recording host (Windows): `MP_STORAGE_BUDGET_BYTES=40000000000` (40 GB) —
  the master corpus (22.4 GB, ~0.62 GB/day).
- VPS relay (`env MP_VPS_HOST` / `-VpsHost` at runtime, never committed — PD-2,
  audit M-1): `MP_STORAGE_BUDGET_BYTES=30000000000` (30 GB)
  in `/etc/money-printer/ops.env`, daily 06:00 UTC via cron
  (see `ops/runbooks/zero-cost-mode.md` for crontab entry). VPS verdicts
  land in journald and Telegram (`--telegram`). With `ZERO_COST=1`, the
  daily pipeline auto-prunes raw logs > 14 days.
- The binance-futures era (`data/raw` 2026-07-18..08-08, 19.98 GB) can
  never pass the INT-4 gate (`missing_provenance`, coverage < 0.995) and so
  can never be pruned by the automated path — it is an OPEN OWNER DECISION
  (`docs/OWNER-DECISION-binance-era.md`: accept / migrate / raise-cap). With
  the default (accept) the corpus floors at ~34–35 GB, so this P2 keeps
  firing on the forward projection most nights — expected, by design, until
  the owner signs.
- Re-run the projection by hand (the budget is explicit config):
  `mp-ops storage-budget --cap-bytes <N> --telegram` with
  `TELEGRAM_BOT_TOKEN`/`TELEGRAM_CHAT_ID` set (or `--cap-bytes` matching the
  host's `MP_STORAGE_BUDGET_BYTES`).
- Check the growth rate is real: `du -sb data/raw` per day, or list per-day
  sizes `ls -l data/raw/2026????_*.log | awk ...`. A negative or flat slope
  means the corpus is being cleaned — the alert self-clears.
- `df -h` for the volume: the budget cap is the operator's number, not the
  disk's free space — a small budget fires early, which is the point.

## Remediation
- The standing answer is compact + ship off-host (spec 003 / 024): run the
  compactor, verify the Parquet cold copies, then the HUMAN deletes
  verified-migrated raw data (W-6 — agents never delete data). The backup
  jobs (`backup_data.ps1`, `vps_backup.ps1`) already mirror the corpus.
- On the VPS relay specifically, the standing answer is the nightly DRAIN
  (`ops/scripts/vps_drain.ps1`, task `MoneyPrinterVpsDrain` 01:00 UTC):
  closed day-files move into the Windows master `data/raw` sha256-verified
  and the VPS copy is released (delete-only-when-verified — the W-6
  exception the owner approved 2026-08-14). The Windows hook passes
  `--manifest`, so a drain RELEASE failure (file landed but the VPS copy
  never released) now fires the same P2 automatically — previously it was
  visible only in `vps_drain.log`/the manifest. A VPS storage-budget fire
  that isn't explained by a drain failure (check `vps_drain.log`/manifest)
  means the relay is genuinely outgrowing its budget.
- If the corpus is genuinely at the cap and history must stay local, this is
  the trigger the spec 001 appendix exists for: evaluate the varint wire
  flip (measured ~29% payload reduction, `docs/DESIGN-frame-header-slimming.md`)
  as a bundling decision — a format change, not an ops fix.
- Do NOT silence the alert by lowering the budget or deleting data to make
  the number look better (W-6, PD-5).

## Escalation
A projection under ~7 days with no off-host capacity ready means writes will
fail within the week: stop collectors cleanly to protect the event log
(same posture as `disk-high`'s < 5% free) and page the owner.
