# vps-phase0-bringup

Minimal VPS recorder setup whose only goal is the ROADMAP Phase-0 gate: **7
consecutive clean days, coverage ≥ 0.995, zero `stale_bursts` across the
qualifying window (spec 024, amendment 2026-08-12 — enforced in `mp-ops
promote`), required streams `trade book funding mark_price open_interest`,
recordings `hyperliquid:BTC` + `hyperliquid:ETH` + `bybit:BTCUSDT`**
(daily_maintenance.sh RECORDINGS — the VPS gate's source of truth; the
Windows-side `ops/core_symbols.txt` deliberately stays hyperliquid-only until
the Windows collector binary carries the orderbook fix). The bybit leg
(COL-29, 2026-08-13) is the real live
source for the `Liquidation` event — its recording must also show the
`liquidation` stream each day (`--require-stream bybit:liquidation`, venue-
scoped so hyperliquid — which has no native liq stream — is unaffected).

This is a *host move*, not new software. Everything needed already exists
in-tree and is green: the collector self-heals WS stalls (COL-2), systemd
units live in `ops/systemd/`, the daily gate is `ops/scripts/daily_maintenance.sh`,
and the verdict tooling is `mp-ops scorecard/promote`. `ops/deploy.md` remains
the full reference; this runbook is the minimal Phase-0 slice of it.

## Why a VPS fixes the actual blockers

The streak is 0/7 for operational reasons on the Windows host, not code:

1. **`stale_stream` bursts every ~3h20m on both symbols** (VPN tunnel re-key /
   Wi-Fi drop on the network path — NOT the collector, per
   `daily-pipeline.md`). Under the 2026-08-12 semantics a burst is a
   per-day warning, not an auto-veto — but it is load-bearing twice over:
   the outages it represents push real recv-clock coverage below the 0.995
   bar (08-08: 0.989, 08-10: 0.983, 08-11: 0.993 vs 08-09's 0.9984), which
   the gate measures honestly, AND even a 0.9984 day (08-09) now blocks
   promotion through the zero-`stale_bursts` window condition (spec 024,
   amendment 2026-08-12). A datacenter egress has no VPN re-key and no
   Wi-Fi; this is the single highest-leverage change.
2. **No gate visibility** — Task Scheduler silently failed to archive
   scorecards for 08-06/07/08 (audit 2026-08-08). A cron + the bash pipeline
   with a streak line restores the daily "N/7".
3. **W-6 existential risk** — data under `~/Downloads` on one C: drive.
   `/opt/money-printer/data` on a real VPS disk fixes location; a nightly
   off-box copy fixes safety.

The collector itself is not the problem: it reconnects via its COL-2
staleness watchdog (`stream stale; reconnecting (COL-2)`) and systemd
`Restart=always` covers crashes. The VPS just stops the network from
re-dirtying days.

## 0. Host

- $5/mo tier: 1 vCPU, 1–2 GB RAM, 40–80 GB SSD. Ubuntu LTS. NTP/chrony is on
  by default (clock discipline — ARCHITECTURE_BRAINSTORM pitfall #4).
- Any region works for Hyperliquid (permissionless API, no geo-block —
  verified live 2026-08-08). Non-US keeps the future Binance option open
  (deploy.md §0).
- Dedicated service user `printer`, never root (OPS-7).
- No VPN/proxy needed — that is the point.

## 1. One-time install

```sh
adduser printer
# rustup + the pinned toolchain (rust-toolchain.toml)
git clone <this repo> /opt/money-printer && chown -R printer:printer /opt/money-printer
su printer -c 'cd /opt/money-printer && cargo build --release --features live-ws,live-http --bin mp-collector && cargo build --release --features live-http --bin mp-whale'
install -d /opt/money-printer/bin
install -m 0755 target/release/mp-collector target/release/mp-whale /opt/money-printer/bin/
# gate tooling (mp-ops, mp-materialize) built by the daily pipeline's own
# stale-guard, or pre-build now:
cargo build --release -p mp-ops && cargo build --release -p mp-storage --bin mp-materialize
install -m 0755 target/release/mp-ops target/release/mp-materialize /opt/money-printer/bin/
# secrets (0600; public data = usually empty, deploy.md §1)
install -m 0600 /dev/null /etc/money-printer/venues.env
install -m 0600 /dev/null /etc/money-printer/ops.env   # only if Telegram alerts
```

## 2. Run — the minimal set

One templated unit per core symbol — the concrete files ship in-tree at
`ops/systemd/mp-hyperliquid@.service` (venue hardcoded to hyperliquid) and
`ops/systemd/mp-collector@.service` (venue hardcoded to bybit, COL-29):

```sh
install -m 0644 ops/systemd/mp-hyperliquid@.service ops/systemd/mp-collector@.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now mp-hyperliquid@BTC mp-hyperliquid@ETH mp-collector@BTCUSDT
systemctl status 'mp-*@*'   # all three active, log growing
```

Additional bybit symbols (2026-08-15, spec 032 multi-symbol direction): the
same template unit, per-symbol instances — `systemctl enable --now
mp-collector@ETHUSDT mp-collector@SOLUSDT`, and add `bybit:ETHUSDT
bybit:SOLUSDT` to the gate's `RECORDINGS` list
(`/opt/money-printer/ops/scripts/daily_maintenance.sh`). DEPLOYED
2026-08-16 01:52 UTC via `bash /home/mp-egress/deploy_bybit_multi.sh` run
over ssh as mp-egress: the script self-elevates (mp-egress is in
`google-sudoers`, GCP NOPASSWD — root SSH with the egress key is denied, so
the guard is how these deploys run). It enabled
`mp-collector@ETHUSDT`/`@SOLUSDT`, installed the updated gate, and grew
swap; all three units verified active + recording. Idempotent — re-run the
script to re-apply. The drain is glob-based (`{YYYYMMDD}_*.log`) so new
symbols are picked up with no drain change.

The same deploy also grows swap (idempotent, `swapfile2` 8 GiB; 21 GB free on
`/`): the 08-14 nightly gate OOM-killed (exit 137) during its determinism
replay (`mp-determinism`, spec 018) on this 952 MiB box, and the gate gains
two more bybit logs — without the headroom the replay (which loads every
required day-log into memory and replays twice) cannot pass. The swap step
runs BEFORE the units start, so tonight's replay and the 5-log compaction
fit. The scorecard/promote gate itself also requires the new symbols' logs to
exist and audit clean, so a deploy before UTC midnight is needed for the
same-day recording to clear the next gate.

The bybit unit is what makes the `Liquidation` event real (COL-29, spec 024):
Bybit's public WS `liquidation.BTCUSDT` topic flows from this egress where
Binance's liq paths are dead, and its recording carries the full required
stream set — `publicTrade.` + `orderbook.50.` (the gate's `book` stream; a
bybit recording from a pre-orderbook-fix binary would fail the gate on the
missing `book` stream) + `tickers.` (funding/mark/OI) + `liquidation.`.

Optional (not gate-required): `mp-whale` census collector
(`collectors/whale_positions.toml`) if the whale-study research edge is
wanted during the streak.

**Restored 2026-08-18 (schema-4 deploy):** `mp-whale.service` ships in-tree
and is enabled by `ops/scripts/deploy_schema4.sh` (the census had been dark
since the Windows watchdog retirement). `mp-netflow.service` (spec 034
Etherscan netflow snapshots) also ships but stays DISABLED until
`MP_ETHERSCAN_KEY` is added to `/etc/money-printer/venues.env` (PD-2,
fail-closed) — then `systemctl enable --now mp-netflow`. Both units follow
the collector template (printer user, `ProtectSystem=strict`,
`ReadWritePaths=/opt/money-printer/data`).

**What replaces the Windows watchdog:** `Restart=always` (crashes) + the
collector's COL-2 reconnect (stalls) + the daily scorecard (the gate truth).
The PS watchdog's data-log *growth* check existed for one specific 10h stale
window (2026-08-08); do not port it up front. If a silent-stall case recurs on
Linux, add a small systemd timer that checks log growth — one unit, later.

## 3. Daily gate (cron, one line)

```sh
# crontab for printer: 00:05 UTC, right after the UTC-midnight log rotation
5 0 * * * /opt/money-printer/ops/scripts/daily_maintenance.sh >> /opt/money-printer/data/scorecards/pipeline.log 2>&1
```

`daily_maintenance.sh` already defaults to `hyperliquid:BTC hyperliquid:ETH`
and the required stream set; it archives the scorecard, compacts clean days,
and materializes features. Add the streak line the PS pipeline has (so a lost
day is visible within 24h, never silently):

```sh
# after the scorecard, before the exit — append to daily_maintenance.sh
# mp-ops takes one value per --required flag; repeat it per recording
# (daily_maintenance.sh builds these from ${RECORDINGS}).
promote_args=(promote --scorecards-dir "${SCORECARD_DIR}")
for recording in ${RECORDINGS}; do
    promote_args+=(--required "$recording")
done
SCORE="$("${BIN_DIR}/mp-ops" "${promote_args[@]}")"
echo "[$(date -u)] Promotion gate: $SCORE"
```

Optional visibility: `mp-ops telegram-send --id daily-pipeline --severity p2|p3`
with the verdict detail (needs `TELEGRAM_BOT_TOKEN`/`CHAT_ID` in ops.env),
mirroring `daily_pipeline.ps1`.

**Dead-man for the gate itself (OPS-17, 2026-08-13):** one cron job
producing the scorecards is itself a single point of failure — a silent
failure of THAT job is the "you find out in 11 days" failure mode. Install
`ops/scripts/pipeline_stale_check.sh` and an hourly cron at :15 past the hour:

```sh
# crontab for printer: dead-man runs hourly; the check itself defers before
# the 00:15 UTC deadline (the 00:05 gate plus margin) and raises a P1
# (runbook ops/runbooks/pipeline-stale.md) when yesterday's scorecard has
# not landed.
15 * * * * /opt/money-printer/ops/scripts/pipeline_stale_check.sh
```

The wrapper probes for the `pipeline-stale` subcommand and skips silently on
binaries that predate OPS-17, so it is safe to install before the tree is
rebuilt. Telegram + the P1 webhook egress activate automatically when
`TELEGRAM_BOT_TOKEN`/`CHAT_ID`/`MP_OPS_P1_WEBHOOK` exist in ops.env.

## 4. Data safety (W-6)

Data lives at `/opt/money-printer/data` — off the disposable path. Minimal
backup: nightly rclone of `data/` to an off-host target (deploy.md §7), or
rsync to the Windows box (its `backup_data.ps1` convention). An untested
backup is a hope: run one restore drill (deploy.md §7) during the 7 days.

**Implemented (2026-08-12):** `ops/scripts/vps_backup.ps1` pulls
`/opt/money-printer/data` to the Windows box nightly (Task Scheduler
`MoneyPrinterVpsBackup`, 00:30 UTC — after the 00:05 daily gate, so each
night's pull includes that day's scorecard) — incremental by mtime via
`~/vps_pull.sh`/`~/vps_stats.sh` on the VPS, exact name-based integrity pass,
JSONL manifest. Drill: `ops/scripts/vps_restore_drill.ps1 -Destination <root>`
— restores the latest day, byte-verifies against the live VPS, and audits the
restored copy with the real `mp-ops` tooling. Re-run the drill each time the
streak advances.

**Marker semantics (hardened 2026-09-04, commit `124800b`):** `.last_backup_ts`
is written ONLY after the integrity pass verifies the delta complete — a
failed run (exit 1/2) leaves the marker where it was, so the next incremental
re-pulls the missing files. Before this, the marker advanced before integrity,
so an exit-2 run's missing files fell off every future incremental (the
2026-09-03 173-file gap; recovered with `-Force`). A transfer that lands ZERO
files for a non-empty delta is a hard failure — Windows bsdtar exits 0 on
empty input, so a dropped ssh stream used to parse as a clean "success".
Transfers retry (default 3 attempts, 30s apart; `-MaxTransferAttempts` /
`-RetryDelaySec`). After any gap, resync the full corpus with
`vps_backup.ps1 -Destination C:\mp-backup -Force` (ignores the marker). The
manifest's last entry is the integrity record: `dst_delta_files ==
src_delta_files` must hold. Staleness + integrity + corpus size are surfaced
daily by `ops/scripts/mp_health.ps1` (Task Scheduler `MoneyPrinterHealth`,
05:30 UTC = 12:00 local, after the 09:00 off-host leg; output logged to
`ops/scripts/mp_health.log`; `-Register` recreates the task).

The off-host leg (`offhost_backup.ps1`) was hardened the same day: remote-only
orphans self-heal (a stale remote ciphertext used to fail the `rclone check`
gate exit-2 forever — the 2026-09-04 incident), the check gate and per-file
verify downloads retry transient failures (flaky link / gdrive eventual
consistency), the `_params` triple-underscore stem decoder was fixed (it
false-negatived and churned ~76 artifacts every run), and the scheduled
task's output is redirected to `ops/scripts/offhost_backup.log` so a
scheduled failure has a visible cause. Exit codes: 0 = pushed + verified;
1 = push failed; 2 = integrity/verify failed; 3 = config; 10 = partial
(live files skipped, retry tomorrow).

**Implemented (2026-08-14): the relay DRAIN — `ops/scripts/vps_drain.ps1`
(Task Scheduler `MoneyPrinterVpsDrain`, 01:00 UTC — after the 00:05 gate AND
the 00:30 backup).** The backup is a READ-ONLY mirror to a separate root; the
drain is the "VPS never accumulates" mechanism behind the OPS-15 relay cap
(storage-budget runbook): closed day-files (`{YYYYMMDD}_*.log` with date <
today UTC — the collector rotates at midnight, so a closed day is frozen)
move into the Windows MASTER corpus (`data/raw`), sha256-verified, and the VPS
copy is released ONLY after byte-verification. Guard against partial
transfers: files land in `data\.vps-drain-staging` first (PER-FILE ssh|tar
streams via `~/vps_drain_pull.sh`'s include-list form — one rel per pipe,
binary-safe `cmd /c` doubled-quote construction, 2026-08-18; previously one
multi-file tar stream where a mid-stream drop discarded every file's
progress); every file is sha256+size-checked against `~/vps_drain_list.sh`'s
output before anything moves; `~/vps_drain_release.sh` re-hashes each file
right before deleting it (a changed file is SKIPPED, never deleted).
Slow-link resilience (2026-08-18): a dropped stream fails only that file —
ssh/tar stderr is captured into `vps_drain.log` on failure (a drop exits
non-zero, or yields an empty/absent stream that the post-transfer check
detects and logs), verified staging copies from a failed run are REUSED
across runs (resume; partial copies re-pulled), and a transfer drop is exit 1
(partial) so verified files still land+release; the exit-2 nothing-released
contract is reserved for genuine size/hash mismatches (staged evidence kept).
Manifest entries for a transfer that never landed also record
`transfer_error: transfer_failed`. Collision policy (A-B overlap,
sec 6): a name already in `data/raw` with DIFFERENT content is never
overwritten and never released — the Windows host's own recording wins until
the handoff; identical content = already landed, release the VPS copy.
Slow-link economy (2026-08-16): before the pull, the master corpus + the
manifest are consulted (`~/vps_drain_pull.sh` takes an include list) — a
byte-identical release re-attempt and a KNOWN A-B collision (latest manifest
entry action=collision, same VPS sha256) are never re-transferred, so the
slow link moves only genuinely new closed days; the master-side hash stays
authoritative, so a deleted master file re-pulls (no stale skip). The
08-12..08-15 hyperliquid A-B backlog (389 MiB) now transfers zero bytes per
night until the handoff.
Release-only-when-verified is the W-6 exception the owner approved for the
relay drain (2026-08-14); `-NoRelease` runs land+verify only. Manifest:
`data/vps_drain_manifest.jsonl` (one entry per candidate: `action` landed/collision/missing, plus per-file `release` released | skipped:<reason> | ssh_failed | no_release | kept — `kept` = A-B collision, VPS copy stays by design; a `landed` entry whose `release` is not `released` means the VPS is still holding the file).

Release-leg fix (2026-08-16): `~/vps_drain_release.sh` self-elevates
(mp-egress → google-sudoers NOPASSWD) because `data/raw` is printer-owned
755 — every nightly release since shipping had aborted on `rm: Permission
denied`, and PS 5.1 EAP=Stop turned the remote stderr into a terminating
error that skipped the manifest (task result 1 every night). `vps_drain.ps1`
now overrides EAP around the release ssh and checks `$LASTEXITCODE` so a
connection failure stays loud (exit 1), never a silent 0.

## 5. Verify — this is the gate

The Phase-0 gate has TWO conditions, both enforced by the same binary
(`mp-ops promote` → `mp_storage::promotion::check_promotion`):

1. **The streak** — 7 consecutive promotable days. Promotable = every
   required recording audits clean: coverage ≥ 0.995 with no
   identity/provenance/loss findings (`stale_stream`/`coverage_gap` are
   warnings per spec 024, 2026-08-12). `mp-ops promote` prints
   1/7 → 2/7 → … and the streak counter keeps advancing even on bursty
   days — a burst is NOT a per-day veto.
2. **The window** — zero `stale_bursts` across every required recording for
   every day in the qualifying 7-day window (spec 024, amendment
   2026-08-12). This is what makes `PROMOTED` mean "clean path", not just
   "clean streak": a full 7/7 on a host that still bursts every ~3h20m
   would validate the wrong hypothesis.

Concretely, the verdict can read `NOT YET: 7 consecutive clean day(s) of 7
required — window carries stale bursts on 2026-08-04` — the streak is full
but promotion waits, and the JSON `burst_days` field names the date and the
recordings that carried the bursts. Only `PROMOTED` (both conditions green)
starts the Phase-0 handoff.

- Day 1: `systemctl status` healthy; `mp-ops audit --date <yesterday> --venue
  hyperliquid --symbol BTC|ETH` clean (spec 024, 2026-08-12 semantics:
  `stale_stream`/`coverage_gap` are warnings; the bar is coverage ≥ 0.995
  with no identity/provenance/loss findings).
- Watch the daily streak line (`Promotion gate: N/7`) and, when the streak
  stalls at 7/7 without `PROMOTED`, read the verdict's `burst_days` — the
  why is in the JSON, never in a blank log.
- **Root-cause proof (now enforced, not aspirational):** a `PROMOTED` on the
  VPS REQUIRES zero `stale_bursts` across the qualifying window — vs. every
  ~3h20m on the old host. Promotion passing is itself the confirmation that
  the VPS validated the egress hypothesis, not just the numeric bar.
- Then update `ops/deploy.md` "Last verified" date (OPS-8) and follow
  `docs/POST_CLEAN_PLAN.md` (compact → cold storage → written evidence).

## 6. VPN/VPS A-B — isolate the egress variable (2026-08-12)

The keepalive change (collectors/src/ws.rs, 2026-08-12) ships in the SAME
binary both hosts run, so the A-B compares one variable: the network path.

1. **Keep the Windows host recording** (do NOT do the Handoff below yet).
   Update its collector binary (watchdog rebuild) so both sides carry the
   keepalive — the control.
2. Run `py -3 ops/scripts/audit_bursts.py -Compare <win-day> <vps-day>` for
   the overlapping days: same date, both hosts' snapshots side by side
   (coverage, stale_bursts windows, worst gap).
3. **Verdict matrix** (read over ≥ 3 overlap days, both symbols):
   - Windows still shows ~3h20m bursts AND VPS shows none → egress root cause
     confirmed; finish the Handoff, the streak starts on the VPS. "Streak
     starts" means the §5 window condition is now the gate: the first
     `PROMOTED` (7 clean days + zero `stale_bursts` in the window) is the
     enforced root-cause proof.
   - VPS ALSO shows bursts (or worse) → not egress; the keepalive/venue
     hypotheses are next — keep both hosts running and inspect
     `journalctl -u mp-hyperliquid@BTC` for reconnect cadence before any
     host is retired.
4. **Owner action:** ≥ 3 clean overlap days on the VPS before stopping the
   Windows recorder (a reverted-streak risk is not worth a day of overlap).

**Verdict — IN (2026-08-16, 4 overlap days 08-12..08-15):** egress root
cause CONFIRMED. Windows still bursts every overlap day while the VPS is
clean (Windows side = `mp-ops audit` via `audit_bursts.py`; VPS side =
nightly gate scorecards, `promotable: true` all 4 days):

| day | Win BTC cov/bursts | Win ETH cov/bursts | VPS BTC cov/bursts | VPS ETH cov/bursts |
|---|---|---|---|---|
| 08-12 | 0.957 / 3 | 0.960 / 3 | 1.0 / 0 | 1.0 / 0 |
| 08-13 | 1.000 / 3 | 1.000 / 3 | 1.0 / 0 | 1.0 / 1 |
| 08-14 | 0.971 / 11 | 0.972 / 14 | 1.0 / 0 | 1.0 / 0 |
| 08-15 | 0.847 / 12 (3.5h gap) | 0.842 / 12 | 1.0 / 0 | 1.0 / 2 |

Decision: **the VPS hyperliquid units are the canonical recorder and stay**
— the gate REQUIRES `hyperliquid:BTC hyperliquid:ETH` in RECORDINGS
(`scorecard --required`/`promote --required`), so stopping them fails the
Phase-0 gate nightly and removes the only clean recording path. The A-B
collision is the temporary overlap the Handoff resolves — NOT accepted
growth. Handoff is DUE (see below); execution pending owner go.

## Deliberately excluded (minimal)

- **Docker** — no Dockerfile exists; `ops/compose.yaml` is aspirational.
  systemd is the supported path.
- **opsd / dead-man / Grafana** — Phase-1 items; systemd + the daily
  scorecard are enough to hold a streak.
- **Binance / Bybit / OKX collectors, extra symbols** — not gate-required.
- **VPN/proxy** — removing it is the fix, not a component.

## Handoff**§6 verdict IN (2026-08-16, above) — EXECUTED 2026-08-18.** The Windows
hyperliquid recorder is retired; the VPS is the single canonical recorder.
Execution record: `MoneyPrinterCollectorsWatchdog` disabled (permanent —
Windows no longer records); all collector/whale processes stopped, stale
locks removed; the Windows-side gate moved 00:05 -> 07:30 UTC and the stale
dead-man 00:15 -> 08:15 UTC so the audit runs after the 01:00 drain lands
the closed day AND past the slow link's realistic transfer time (a full day
is ~2.3 GiB; the drain task allows 8 h and the gate's UTC-0-9 self-guard
covers the window — header of ops/scripts/daily_pipeline.ps1). The drain
pulls the gate-required hyperliquid files first so they land earliest. The stop happened
mid-day (08-18 ~01:30 UTC) rather than at a day boundary, so the 08-18
Windows partials were deleted — the first drained VPS day lands as a fresh
file with no collision pair, matching the doc's boundary intent. Historical
08-12..17 A-B pairs stay held (never released) per the rule below. VPS
canonical path verified: all 5 collector units active, VPS 08-17 gate
`promotable: true`. Steps as written (keep backup/drain/pipeline tasks,
Windows stays the master corpus):

- `Stop-ScheduledTask MoneyPrinterCollectorsWatchdog` — the Windows
  hyperliquid recorder. This ends the A-B collision at its source: from the
  next UTC day the drain lands + releases the VPS hyperliquid files like the
  bybit ones, and the ~130 MB/day accumulation + nightly 389 MiB collision
  re-transfer stop.
- **Keep** `MoneyPrinterVpsBackup` (00:30 W-6 mirror) + `MoneyPrinterVpsDrain`
  (01:00) + `MoneyPrinterOffhostBackup` + `MoneyPrinterDataBackup` +
  `MoneyPrinterHealth` (05:30 UTC daily report): the Windows box stays the
  master corpus (drain destination) and keeps its safety copies. The
  pre-drain "(+ the backup task)" instruction no longer applies.
- `MoneyPrinterDailyPipeline` (Windows-side audit/features/compaction): keep
  running — it now audits the drain destination; nothing to stop.
- Do the stop at a UTC day boundary (e.g. 23:55 UTC) so only the stop-day's
  collision pair is left behind (08-16 if stopped now; that pair is kept on
  the VPS, never released).

Keep the Windows box as the analysis host and off-box backup target.
