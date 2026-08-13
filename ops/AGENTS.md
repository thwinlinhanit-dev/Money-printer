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
- `src/bin/mp-ops.rs` — ops CLI (`compact`/`audit`/`scorecard`/`promote`/`band-accuracy-decay`/`telegram-flush [--wait]`/`telegram-stale`/`telegram-send`/`p1-webhook`)
- `src/bin/opsd.rs` — ops daemon binary
- `runbooks/` — incident response runbooks (14 runbooks: one per P1/P2 alert id in the registry, plus incident runbooks such as `ws-egress-filter.md` for known infrastructure conditions)
- `systemd/` — systemd service/timer units
- `ci/` — CI guardrails: `guardrails.sh` (bash/CI) + `guardrails.ps1` (PowerShell 5.1 port for the native Windows host — same rules, same exit contract; keep the two in sync)
- `compose.yaml` — Docker Compose
- `deploy.md` — deployment instructions
- `watchdog_collectors.ps1` — 24/7 collector watchdog
- `core_symbols.txt` — single source of truth for the Phase-0 core recording set, one `venue:SYMBOL` per line (2026-08-08: `hyperliquid:BTC`/`hyperliquid:ETH` — this host's egress is geo-filtered by Binance futures, spec 024; loaded by the collector watchdog AND the daily integrity pipeline so recorded == required)
- `scripts/weekly_band_accuracy.sh` — weekly whale band-accuracy study driver (RES-4/LIQ-6)
- `scripts/backup_data.ps1` — incremental mirror backup of the `data/` corpus (robocopy /E, W-6 read-only on source, integrity pass, JSONL manifest, `-Register` daily task; live `.lock_*` files are excluded from copy and count — held open exclusively by running collectors, transient coordination state, recreated on restore)
- `scripts/vps_backup.ps1` — nightly pull of the VPS recorder corpus (`/opt/money-printer/data`) to the Windows box (vps-phase0-bringup.md sec 4): ssh `tar --newer-mtime` incremental stream piped binary-safe via `cmd /c` to local tar.exe, mtime marker (`.last_backup_ts`, run-start epoch so mid-run writes are never missed), exact name-based integrity pass, JSONL manifest, `-Register` daily 00:30 UTC task `MoneyPrinterVpsBackup`; remote side is `~/vps_pull.sh` + `~/vps_stats.sh` on the VPS (read-only; transient `.lock_*`/`*.pid`/`*.heartbeat` excluded)
- `scripts/vps_restore_drill.ps1` — restore drill for the VPS backup (deploy.md sec 7): restores the latest backed-up day into a scratch dir, byte-verifies against the live VPS (prefix hash for the append-only raw logs, full hash for the static scorecard), then proves usability with the real `mp-ops audit`/`promote` on the restored state; PASS/FAIL exit code
- `scripts/pipeline_stale_check.sh` — VPS cron wrapper for the `pipeline-stale` dead-man (OPS-17, runbook `pipeline-stale.md`): hourly at :15 past the hour, runs `mp-ops pipeline-stale --telegram --webhook` (P1 when yesterday's scorecard has not landed by the deadline); probes for the subcommand and skips silently on binaries that predate OPS-17, so it is safe to install before the tree is rebuilt
- `scripts/daily_pipeline.ps1` — daily integrity gate → scorecard → feature materialization → compaction (spec 024 + spec 016 Phase 2; Task Scheduler `MoneyPrinterDailyPipeline` at 00:05 UTC; `-RegisterStaleTask` also registers the `MoneyPrinterPipelineStale` dead-man at 00:15 UTC — a separate watchdog for the gate itself). On a promotable day it runs `target/release/mp-materialize.exe` over the day's logs (recordings + whale-positions census, `features/features.toml`) into `data/features` with the git sha as engine provenance (FEA-6); `-SkipMaterialize` limits the run to audit + verdict
- `scripts/ws_probe.mjs` — raw WS probe for the spec 024 egress filter (`node ops/scripts/ws_probe.mjs`; node ≥ 22; direct egress only — proxy verification goes through the collector + audit `streams` map, see `runbooks/ws-egress-filter.md`). A SUBSCRIBE `{error}` response (rejection) sets `subscribeRejected` and exits 2, distinct from the success `{result: null}` ack — fail-closed (audit 2026-08-08)
- `scripts/set_ws_proxy.ps1` — one-command activation/rollback of the collector WS egress proxy: sets/clears User-scope `MP_WS_PROXY`, fail-closed `curl.exe -x` pre-flight to Binance REST, watchdog restart (`runbooks/ws-egress-filter.md`)

## Local Contracts

- Collector watchdog uses `Spawn-Process` with `UseShellExecute=true` for full process detachment
- A named local mutex permits only one collector-watchdog supervisor across scheduled and manual invocations.
- Collector `--trace-file` output is append-only and unbuffered; inspect the per-day trace after a watchdog restart before changing any liveness threshold.
- Watchdog spawns Binance collectors with `--trade-source rest`/`--mark-source rest` (fapi aggTrades/premiumIndex; spec 024 incident 08-04 — fstream silently drops aggTrade/markPrice/forceOrder from this egress) and hyperliquid collectors pure-WS (`trade_source=rest` requires a Binance normalizer)
- Collector watchdog validates `$Symbols` entries as `venue:SYMBOL` (lowercase alnum venue + uppercase alnum symbol; a plain `SYMBOL` means binance) and double-quotes them before spawn (anti-injection, audit 08-04)
- The collector watchdog, `start_collectors.ps1`, and the daily integrity pipeline (`scripts/daily_pipeline.ps1`) all derive their recording set from `core_symbols.txt` through ONE shared resolver, `scripts/recordings.ps1` (`Resolve-Recordings`, dot-sourced by all three — audit 08-10, after a hardcoded-binance heartbeat drift; invalid lines throw rather than silently narrowing the set) — keep recorded and required recordings in sync there, never as hand-maintained lists
- The Phase-0 gate is tolerance-based (2026-08-12): `is_clean()` = coverage ≥ 0.995 with no identity/provenance/`sequence_gap`/`backpressure_loss`/`low_coverage` findings; `stale_stream` and `coverage_gap` are warnings and `stale_bursts`/`worst_gap_ns` are margin fields surfaced in scorecards and `daily_pipeline.ps1` log lines (spec 024)
- The watchdog supervises `mp-whale` (spec 028 Hyperliquid on-chain census) alongside the market collectors via the same heartbeat/staleness model (`collectors/whale_positions.toml`); its log (`{date}_hyperliquid_positions.log`) feeds the liq-est band research edge (spec 029) and the whale.net/delta features (spec 016 materialization passes it to mp-materialize), and is not a required gate recording
- Deadman detection checks heartbeat files with configurable grace period
- Whale band-accuracy study runs weekly via `systemd/whale-study.timer` (Tue 06:30 UTC, clear of the Mon 06:00 grading run) → `scripts/run_whale_study_weekly.sh` → `research/run_band_accuracy.py`; the unit may write only `research/band_accuracy/` (weekly ledger + SIM-10 `runs/index.jsonl` tracker), RES-7 posture
- The daily promotion-gate verdict (`daily_pipeline.ps1`) is pushed to Telegram via `mp-ops telegram-send --id daily-pipeline --detail ... --severity p2|p3` (2026-08-10): one-shot, NO quiet-hours batching (the pipeline runs at 00:05 UTC, inside 22:00–07:00 — a batched P3 would sit until the next flush), fail-closed on missing creds (exit 2, WARN-only in the pipeline so the gate verdict/exit code is never masked by an alerting outage); non-promotable days send severity p2 with the per-recording DIRTY/blocking breakdown
- After the study, the wrapper runs the OPS-13 drift/decay watch: `mp-ops band-accuracy-decay --trend <out>/band_accuracy.jsonl --runs-dir /opt/money-printer/runs --telegram` (env `MP_OPS_CMD`, default `/opt/money-printer/bin/mp-ops`); the weekly verdict is journaled to `runs/index.jsonl` as a `band_accuracy_decay` record line (correlated to the study's run by `run_id`/`week`); best-effort — a missing binary skips, never fails the study
- The decay P3 is delivered via `src/telegram.rs` (the OPS-9 edge): the CLI routes it through `AlertRouter` (quiet hours from `MP_OPS_QUIET_START_MIN`/`END_MIN`, default 22:00–07:00 UTC), sends via curl to the Bot API (`TELEGRAM_BOT_TOKEN`/`TELEGRAM_CHAT_ID`, `MP_OPS_TELEGRAM_URL` override, `disable_notification=true`), or batches into `journal/telegram/batch.jsonl`; `mp-ops telegram-flush --wait` waits out quiet hours internally (sleeping only while the window is active; `MP_OPS_SLEEP` stubs the sleeper in tests) then drains (fail-closed: a failed send keeps the batch and exits 2); each successful send is also appended to `journal/telegram/delivered.jsonl` (id + delivered_ts_ns, fsynced W-6 — the delivery log); `load_telegram_batch`/`load_telegram_delivered` read the ledger pair (fail-closed, injected clock) for the monthly report's delivery-accountability section; `stale_batch_alert` raises `telegram-stale` (P2, OPS-14) when a dispatch sits queued ≥ one quiet window (default 24h — a missed flush), oldest wins, per-entity dedupe key — `mp-ops telegram-stale` runs it near-real-time (hourly `systemd/telegram-stale.timer`, P2 breaks through quiet hours)
- `src/report.rs` consumes `research/band_accuracy/band_accuracy.jsonl` (append-only, W-6): `load_band_accuracy_trend` grounds the monthly report's RES-4 section (OPS-6; missing journal = no-data month, corrupt line = fail closed); `band_accuracy_decay_alert` raises the `band-accuracy-decay` P3 on trailing-window drift (OPS-13, RES-3 semantics) — alert-only, never mutates the journal; `append_run_record` journals the weekly verdict to `runs/index.jsonl` (the RES-4 tracker, fsynced W-6)
- P1 webhook egress (owner decision 2026-08-06, audit 08-04 #3/#9): LIVE since 2026-08-09 — `MP_OPS_P1_WEBHOOK` is provisioned (user-level env) to an ntfy.sh secret topic (`https://ntfy.sh/<random-topic>`, topic name == the feed credential, embedded in the URL; the sink accepts the plain JSON POST with no auth headers). `mp-ops p1-webhook --id --detail` POSTs the P1 dispatch JSON to `MP_OPS_P1_WEBHOOK` via curl (https accepted, TLS is the host's) and fails loudly (exit 2, "dead until credentials") if the env var is ever unset. On a curl transport failure only the exit code is reported — stderr is suppressed (it can embed the token-bearing URL; audit 2026-08-08, PD-2)
- Runbooks must be followed for incident response; update runbook if procedure changes

## Verification

- `cargo test -p mp-ops`
- `cargo test -p mp-ops --test ops_slice`

## Child DOX Index

None.
