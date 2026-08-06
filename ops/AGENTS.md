# ops

## Purpose

Production operations: alerting, bot journal, ops daemon, deadman detection, systemd units, CI scripts, Docker compose, runbooks, and deployment tooling.

## Ownership

- `src/alert.rs` — alert dispatch
- `src/bot.rs` — bot journaling
- `src/daemon.rs` — ops daemon service
- `src/deadman.rs` — deadman detection
- `src/journal.rs` — operations journal
- `src/latch.rs` — latch mechanism
- `src/registry.rs` — component registry
- `src/report.rs` — report generation + the RES-4 band-accuracy trend (loader, grounded section, OPS-13 decay watch)
- `src/watch.rs` — watch loops
- `src/bin/mp-ops.rs` — ops CLI (`compact`/`audit`/`scorecard`/`promote`/`band-accuracy-decay`)
- `src/bin/opsd.rs` — ops daemon binary
- `runbooks/` — incident response runbooks (11 runbooks)
- `systemd/` — systemd service/timer units
- `ci/` — CI guardrails
- `compose.yaml` — Docker Compose
- `deploy.md` — deployment instructions
- `watchdog_collectors.ps1` — 24/7 collector watchdog
- `scripts/weekly_band_accuracy.sh` — weekly whale band-accuracy study driver (RES-4/LIQ-6)
- `scripts/backup_data.ps1` — incremental mirror backup of the `data/` corpus (robocopy /E, W-6 read-only on source, integrity pass, JSONL manifest, `-Register` daily task)

## Local Contracts

- Collector watchdog uses `Spawn-Collector` with `UseShellExecute=true` for full process detachment
- Watchdog spawns collectors with `--trade-source rest` (Binance trade ingestion via fapi aggTrades; spec 024 incident 08-04 — fstream silently drops aggTrade from datacenter egress)
- Collector watchdog validates `$Symbols` against `^[A-Z0-9]{2,20}$` and double-quotes them before spawn (anti-injection, audit 08-04)
- Deadman detection checks heartbeat files with configurable grace period
- Whale band-accuracy study runs weekly via `systemd/whale-study.timer` (Tue 06:30 UTC, clear of the Mon 06:00 grading run) → `scripts/run_whale_study_weekly.sh` → `research/run_band_accuracy.py`; the unit may write only `research/band_accuracy/` (weekly ledger + SIM-10 `runs/index.jsonl` tracker), RES-7 posture
- After the study, the wrapper runs the OPS-13 drift/decay watch: `mp-ops band-accuracy-decay --trend <out>/band_accuracy.jsonl` (env `MP_OPS_CMD`, default `/opt/money-printer/bin/mp-ops`); best-effort — a missing binary skips, never fails the study
- `src/report.rs` consumes `research/band_accuracy/band_accuracy.jsonl` (append-only, W-6): `load_band_accuracy_trend` grounds the monthly report's RES-4 section (OPS-6; missing journal = no-data month, corrupt line = fail closed); `band_accuracy_decay_alert` raises the `band-accuracy-decay` P3 on trailing-window drift (OPS-13, RES-3 semantics) — alert-only, never mutates the journal
- Runbooks must be followed for incident response; update runbook if procedure changes

## Verification

- `cargo test -p mp-ops`
- `cargo test -p mp-ops --test ops_slice`

## Child DOX Index

None.
