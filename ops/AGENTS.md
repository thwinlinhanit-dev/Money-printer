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
- `src/report.rs` — report generation (markdown + HTML renderers, `write_monthly_report` → `journal/reports/{YYYY-MM}/`) + the RES-4 band-accuracy trend (loader, grounded section, OPS-13 decay watch) + the delivery-accountability section (grounded on the `journal/telegram/` ledger pair via `load_telegram_batch`/`load_telegram_delivered` — pending un-flushed P3 dispatches with queue age, plus what WAS delivered from the flush's `delivered.jsonl` log)
- `src/watch.rs` — watch loops
- `src/bin/mp-ops.rs` — ops CLI (`compact`/`audit`/`scorecard`/`promote`/`band-accuracy-decay`/`telegram-flush [--wait]`/`telegram-stale`)
- `src/bin/opsd.rs` — ops daemon binary
- `runbooks/` — incident response runbooks (14 runbooks: one per P1/P2 alert id in the registry, plus incident runbooks such as `ws-egress-filter.md` for known infrastructure conditions)
- `systemd/` — systemd service/timer units
- `ci/` — CI guardrails
- `compose.yaml` — Docker Compose
- `deploy.md` — deployment instructions
- `watchdog_collectors.ps1` — 24/7 collector watchdog
- `core_symbols.txt` — single source of truth for the Phase-0 core Binance perpetual symbol set (loaded by the collector watchdog AND the daily integrity pipeline so recorded == required)
- `scripts/weekly_band_accuracy.sh` — weekly whale band-accuracy study driver (RES-4/LIQ-6)
- `scripts/backup_data.ps1` — incremental mirror backup of the `data/` corpus (robocopy /E, W-6 read-only on source, integrity pass, JSONL manifest, `-Register` daily task)
- `scripts/daily_pipeline.ps1` — daily integrity gate → scorecard → compaction (spec 024; Task Scheduler `MoneyPrinterDailyPipeline` at 00:05 UTC)
- `scripts/ws_probe.mjs` — raw WS probe for the spec 024 egress filter (`node ops/scripts/ws_probe.mjs`; node ≥ 22; direct egress only — proxy verification goes through the collector + audit `streams` map, see `runbooks/ws-egress-filter.md`)

## Local Contracts

- Collector watchdog uses `Spawn-Collector` with `UseShellExecute=true` for full process detachment
- Watchdog spawns collectors with `--trade-source rest` (Binance trade ingestion via fapi aggTrades; spec 024 incident 08-04 — fstream silently drops aggTrade from datacenter egress)
- Collector watchdog validates `$Symbols` against `^[A-Z0-9]{2,20}$` and double-quotes them before spawn (anti-injection, audit 08-04)
- The collector watchdog and the daily integrity pipeline (`scripts/daily_pipeline.ps1`) both derive their symbol set from `core_symbols.txt` (one per line) when not overridden — keep recorded and required symbols in sync there, never as two hand-maintained lists
- Deadman detection checks heartbeat files with configurable grace period
- Whale band-accuracy study runs weekly via `systemd/whale-study.timer` (Tue 06:30 UTC, clear of the Mon 06:00 grading run) → `scripts/run_whale_study_weekly.sh` → `research/run_band_accuracy.py`; the unit may write only `research/band_accuracy/` (weekly ledger + SIM-10 `runs/index.jsonl` tracker), RES-7 posture
- After the study, the wrapper runs the OPS-13 drift/decay watch: `mp-ops band-accuracy-decay --trend <out>/band_accuracy.jsonl --runs-dir /opt/money-printer/runs --telegram` (env `MP_OPS_CMD`, default `/opt/money-printer/bin/mp-ops`); the weekly verdict is journaled to `runs/index.jsonl` as a `band_accuracy_decay` record line (correlated to the study's run by `run_id`/`week`); best-effort — a missing binary skips, never fails the study
- The decay P3 is delivered via `src/telegram.rs` (the OPS-9 edge): the CLI routes it through `AlertRouter` (quiet hours from `MP_OPS_QUIET_START_MIN`/`END_MIN`, default 22:00–07:00 UTC), sends via curl to the Bot API (`TELEGRAM_BOT_TOKEN`/`TELEGRAM_CHAT_ID`, `MP_OPS_TELEGRAM_URL` override, `disable_notification=true`), or batches into `journal/telegram/batch.jsonl`; `mp-ops telegram-flush --wait` waits out quiet hours internally (sleeping only while the window is active; `MP_OPS_SLEEP` stubs the sleeper in tests) then drains (fail-closed: a failed send keeps the batch and exits 2); each successful send is also appended to `journal/telegram/delivered.jsonl` (id + delivered_ts_ns, fsynced W-6 — the delivery log); `load_telegram_batch`/`load_telegram_delivered` read the ledger pair (fail-closed, injected clock) for the monthly report's delivery-accountability section; `stale_batch_alert` raises `telegram-stale` (P2, OPS-14) when a dispatch sits queued ≥ one quiet window (default 24h — a missed flush), oldest wins, per-entity dedupe key — `mp-ops telegram-stale` runs it near-real-time (hourly `systemd/telegram-stale.timer`, P2 breaks through quiet hours)
- `src/report.rs` consumes `research/band_accuracy/band_accuracy.jsonl` (append-only, W-6): `load_band_accuracy_trend` grounds the monthly report's RES-4 section (OPS-6; missing journal = no-data month, corrupt line = fail closed); `band_accuracy_decay_alert` raises the `band-accuracy-decay` P3 on trailing-window drift (OPS-13, RES-3 semantics) — alert-only, never mutates the journal; `append_run_record` journals the weekly verdict to `runs/index.jsonl` (the RES-4 tracker, fsynced W-6)
- Runbooks must be followed for incident response; update runbook if procedure changes

## Verification

- `cargo test -p mp-ops`
- `cargo test -p mp-ops --test ops_slice`

## Child DOX Index

None.
