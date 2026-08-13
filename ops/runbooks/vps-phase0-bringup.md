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

The bybit unit is what makes the `Liquidation` event real (COL-29, spec 024):
Bybit's public WS `liquidation.BTCUSDT` topic flows from this egress where
Binance's liq paths are dead, and its recording carries the full required
stream set — `publicTrade.` + `orderbook.50.` (the gate's `book` stream; a
bybit recording from a pre-orderbook-fix binary would fail the gate on the
missing `book` stream) + `tickers.` (funding/mark/OI) + `liquidation.`.

Optional (not gate-required): `mp-whale` census collector
(`collectors/whale_positions.toml`) if the whale-study research edge is
wanted during the streak.

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

## Deliberately excluded (minimal)

- **Docker** — no Dockerfile exists; `ops/compose.yaml` is aspirational.
  systemd is the supported path.
- **opsd / dead-man / Grafana** — Phase-1 items; systemd + the daily
  scorecard are enough to hold a streak.
- **Binance / Bybit / OKX collectors, extra symbols** — not gate-required.
- **VPN/proxy** — removing it is the fix, not a component.

## Handoff

**Only after the §6 A-B verdict** (VPS clean, ≥ 3 overlap days): stop the
Windows recorder to avoid two hosts writing the same dates:
`Stop-ScheduledTask MoneyPrinterCollectorsWatchdog` +
`Stop-ScheduledTask MoneyPrinterDailyPipeline` (+ the backup task). Keep the
Windows box as the analysis host and off-box backup target.
