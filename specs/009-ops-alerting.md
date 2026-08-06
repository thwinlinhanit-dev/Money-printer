# 009 — Ops, Monitoring, Alerting & Reporting

## Purpose
The system runs 24/7 unattended; ops is part of the edge. A recorder that dies
silently for 11 days, or a live loop nobody can flatten from a phone, is how
the moat and the account respectively stop existing.

## Scope
In: deployment, process supervision, dead-man switch, Telegram bot, alert
policy, backups, monthly report. Out: trading logic (all other specs).

## Design

### Topology (v1)
One VPS (non-US region for venue access), Docker Compose or systemd units:
`collector-{venue}` ×N, `compactor` (timer), `features-live`, `oms`,
`opsd` (monitor + Telegram + dead-man), `clickhouse` (optional).
Processes communicate via event-log files + local NATS (if enabled) — no
cross-host anything in v1.

### Alert policy (severity → channel → expectation)
| Sev | Meaning | Channel | Examples |
|---|---|---|---|
| P1 | money at risk NOW | Telegram + phone-call webhook | kill switch tripped, recon DIVERGED, UNKNOWN order unresolved, oms down in live |
| P2 | data/edge degrading | Telegram | stream gap > 5min, collector down, disk > 85%, determinism diff (SIM-11), stale Telegram batch (missed flush, OPS-14) |
| P3 | FYI | Telegram (quiet hours batched) | funnel transitions, daily digest, screener hits (if enabled), band-accuracy decay (RES-4 trend, OPS-13) |

Alert rules: every alert has an id, dedupe window, and runbook link (below).
Alert on ABSENCE (dead-man), not only on presence of errors.

### Dead-man switch
`opsd` expects heartbeats: each process POSTs `/beat/{proc}` every 30s
(collectors also expose /health per COL-10). Missed 3 beats ⇒ P2 (P1 if oms
in live mode). `opsd` itself is watched by an EXTERNAL dead-man (free tier
healthchecks.io-style): opsd pings out every 5 min; external service alerts
the phone if pings stop — the watcher has a watcher.

### Telegram bot (command surface — the phone is the console)
```
/status          per-process health, positions, equity, today P&L
/positions       open positions with age + unrealized
/kill <scope>    trip kill switch: strategy id | venue | GLOBAL (confirm dialog)
/flatten         GLOBAL kill + reduce-only flatten (double confirm)
/silence <id> <dur>   ack an alert
/funnel          strategy stages + pending gate evidence
/report          link to latest monthly report
```
Bot MUST be allowlisted to the owner's Telegram user id; every command
journaled; /kill and /flatten work even if oms is wedged (they write the
kill-latch file the gate reads — RG-10 — not an RPC to oms).

### Runbooks
`ops/runbooks/{alert-id}.md` — every P1/P2 alert id has one: symptoms,
diagnosis commands, safe remediation, escalation. Agents adding an alert MUST
add its runbook in the same commit.

### Backups & restore drill
Nightly: `journal/`, `runs/index.sqlite`, configs, funnel state → encrypted
tarball → off-host (rclone target). `cold/` per owner's budget decision
(spec 003 open question). **Quarterly restore drill is a calendared task**:
restore to a scratch dir, run `sim` golden fixture from restored state —
an untested backup is a hope, not a backup.

### Monthly report (the fund-of-one scoreboard)
Generated (ops job) from journals + tracker on the 1st, per
SYSTEM_BLUEPRINT §13: equity & DD curves (blended + per strategy),
expectancy table after costs, live-vs-paper-vs-backtest tracking error,
cost breakdown (fees, slippage vs model, funding, infra), funnel
transitions + kills, the RES-4 whale band-accuracy trend (weekly
`liq.est_bands` validation grades, spec 029 LIQ-6) as a grounded section read
from `research/band_accuracy/band_accuracy.jsonl`, delivery accountability
(the quiet-hours Telegram batch ledger — pending, un-flushed P3 dispatches
with queue age; anything still queued at month-end is a delivery gap the
report surfaces, never silently dropped, W-6 — plus the delivery log
`journal/telegram/delivered.jsonl`: the flushed records of what WAS
delivered this month, the evidence the ledger worked), benchmark row (vs BTC hold,
vs T-bill). Rendered to markdown + HTML in `journal/reports/{YYYY-MM}/`. An LLM MAY draft the prose
commentary; every number MUST come from the generated tables (grounded), and
the human reads it — the report is for the owner, not for the machine.

## Requirements
- **OPS-1** Every long-running binary MUST ship a systemd unit (or compose
  entry) with restart=always, resource limits, and log rotation.
- **OPS-2** Heartbeat + dead-man as designed; external watcher configured;
  missed-beat alerts within 2 minutes (P1 path if live).
- **OPS-3** Telegram bot with exactly the command surface above; owner-id
  allowlist; /kill and /flatten function via the latch file independent of
  oms process health; all commands journaled.
- **OPS-4** Alert framework: id, severity, dedupe, runbook link; adding an
  alert without a runbook fails CI (lint script checks ids ↔ files).
- **OPS-5** Nightly backup job + documented, quarterly-calendared restore
  drill script (`ops/restore-drill.sh`) that verifies via golden fixture.
- **OPS-6** Monthly report generator producing the §13 scoreboard from
  journals/tracker only (no hand-entered numbers); benchmark row REQUIRED. The
  RES-4 band-accuracy trend (spec 029 LIQ-6) MUST render as a grounded section
  from `research/band_accuracy/band_accuracy.jsonl`: a missing journal is an
  explicit "no data" row, a corrupt line fails closed (CONV-8) — never a
  fabricated number. A delivery-accountability section MUST render the
  quiet-hours Telegram batch ledger (`journal/telegram/batch.jsonl`): pending
  (un-flushed) P3 dispatches with queue age and their runbook link — a
  missing/empty ledger is the explicit healthy "nothing pending" state
  (strictly grounded: the ledger records only what is queued now), a corrupt
  line fails closed (CONV-8). The same section MUST also render the delivery
  log (`journal/telegram/delivered.jsonl` — appended by `telegram-flush` on
  each successful send): what WAS delivered this month (alert id + delivery
  time), the accountability counterpart to the queue — a missing/empty log
  is the explicit healthy "no deliveries recorded" state, a corrupt line
  fails closed (CONV-8).
  MUST render to markdown + HTML in
  `journal/reports/{YYYY-MM}/` (self-contained HTML, dynamic strings escaped).
- **OPS-7** `opsd` MUST watch disk (STO-7), clock skew (NTP; warn > 100ms —
  lead-lag research and venue timestamps depend on it), and cert/key file
  permissions (0600) — each with alerts + runbooks.
- **OPS-8** Deployment MUST be reproducible: one `ops/deploy.md` +
  `compose.yaml`/units checked in; a fresh VPS reaches running-collector
  state by following the doc verbatim (test this once, note the date).
- **OPS-9** Quiet hours (config) batch P3s; P1/P2 always break through.
- **OPS-10** Log retention: journals forever (they are the business record,
  W-6); process logs 30 days rotated.
- **OPS-13** The RES-4 band-accuracy trend MUST be watched for drift/decay
  (OPS-11/OPS-12 are spec 021's bot-journal requirements; numbering continues
  here): with ≥ 12 graded weeks, when the trailing 4-week mean coverage drops
  below half the 12-week mean (baseline ≥ 0.5) or the trailing 4-week mean
  relative error more than doubles the 12-week mean (baseline > 0), a
  `band-accuracy-decay` P3 alert MUST be raised — RES-3 decay semantics
  (spec 010) applied to the RES-4 quality metrics. The check MUST run weekly
  after the band-accuracy study appends the trend (`mp-ops
  band-accuracy-decay --trend <journal> --runs-dir <runs> --telegram`,
  invoked by `run_whale_study_weekly.sh`) and MAY run from any other
  consumer of the journal (e.g. monthly report generation). Each weekly
  verdict — clean or decayed — MUST be journaled to `<runs>/index.jsonl` as
  its own `band_accuracy_decay` record line in the shared RES-4/SIM-10 run
  tracker (append-only, W-6), correlated to the study's `whale_study` run
  record by the `run_id` + `week` of the latest trend line — the tracker
  records the study AND its drift verdict per week; an empty trend has no
  run to attach and journals nothing. The raised P3 MUST be delivered
  to Telegram through the alert framework's dedupe + quiet-hours batching
  (OPS-9): sent immediately outside quiet hours, held in
  `journal/telegram/batch.jsonl` during them and flushed at quiet-hours end
  by `mp-ops telegram-flush --wait` — the wait lives inside the subcommand,
  which sleeps only while quiet hours are active and then drains (a flush
  that starts outside the window drains immediately).
  Missing credentials are a gated no-op (`"unconfigured"`, exit 0) — never a
  silent failure and never a fake send; a failed send exits 2 with the
  batch intact (CONV-8) — the same failed-job status the Python research
  siblings use. A missing binary or corrupt journal is a warning,
  never a study failure; the check itself refuses to fabricate a verdict
  (CONV-8). Alert-only (W-6): never mutates the journal.
- **OPS-14** The quiet-hours Telegram batch ledger MUST be watched
  near-real-time for a missed flush: when a dispatch has been queued longer
  than one full quiet window (default 24h), a `telegram-stale` P2 alert MUST
  be raised — the monthly report flags a stuck queue only at month-end, but
  a missed `telegram-flush` must alert near-real-time (hourly check via
  `systemd/telegram-stale.timer` → `mp-ops telegram-stale --telegram`; the
  check reads the ledger only, W-6). The check MUST fail closed on a corrupt
  ledger (CONV-8, exit 2) and stay silent on an empty one, and a fired P2
  MUST break through quiet hours (OPS-9): it is sent immediately, never
  re-queued into the very batch that is stuck. The oldest stale dispatch
  wins and the alert's dedupe key is that dispatch's id (per-entity, so one
  stuck alert never suppresses another).

## Acceptance criteria
- [x] Dead-man fires only after the missed-beat deadline and escalates P2→P1 for a critical proc in live mode (OPS-2). `ops_2_deadman_fires_after_three_missed_beats_and_escalates_in_live`. (The literal `kill -9`→restart supervision is systemd's `Restart=always` in `ops/systemd/*.service`, exercised at deploy time per OPS-8, not in-crate.)
- [x] `/kill` latch → real `mp_risk::evaluate` rejects the next intent with the RG-10 `KillSwitchTripped` verdict, independent of oms (OPS-3). `ops_3_kill_latch_makes_the_real_gate_reject_with_rg10` (plus `ops_3_kill_latch_roundtrips_and_trips_kill_switches`, `ops_3_flatten_is_global_kill`).
- [x] Alert-without-runbook fails CI (OPS-4). `ops_4_every_catalog_alert_has_a_runbook_file` + the guardrails lint (verified to exit non-zero on a removed runbook).
- [x] Alert dedupe + quiet-hours P3 batching with P1/P2 breakthrough (OPS-4/9). `ops_4_alert_dedupes_within_window_then_fires_again`, `ops_9_quiet_hours_batch_p3_but_p1_breaks_through`.
- [x] Monthly report generates from fixture inputs with all §13 sections + the required benchmark row, numbers grounded (OPS-6). `ops_6_report_has_all_sections_and_benchmark_row`, `ops_6_report_numbers_are_grounded_not_invented`.
- [x] The HTML render shows the same grounded numbers as the markdown, renders explicit "no data" rows for empty inputs, escapes dynamic strings (a hostile strategy name cannot break the document), and `write_monthly_report` lands `report.md` + `report.html` in `journal/reports/{YYYY-MM}/` (OPS-6). `ops_6_report_html_has_all_sections_and_grounded_numbers`, `ops_6_report_html_renders_no_data_and_escapes_dynamic_strings`, `ops_6_report_writes_markdown_and_html_to_month_dir`.
- [x] The RES-4 band-accuracy section is grounded on `band_accuracy.jsonl`: rows sorted by ISO week and rendered exactly as loaded; a corrupt line fails the whole load naming the line; a missing journal is a "no data" month (OPS-6). `ops_6_band_accuracy_trend_loads_from_jsonl_sorted_and_grounded`, `ops_6_band_accuracy_trend_fails_closed_on_corruption_and_missing_is_no_data`.
- [x] The delivery-accountability section is grounded on `journal/telegram/batch.jsonl`: the loader round-trips what `append_batch` writes, sorts oldest-first, computes queue age from an injected clock, fails closed on a corrupt line naming it, and treats a missing ledger as the healthy empty state; both renderers show the pending table (parity enforced) or the explicit "all flushed" state, and hostile details are HTML-escaped (OPS-6). `ops_6_telegram_batch_loads_pending_rows_fail_closed`, `ops_6_report_delivery_accountability_section_renders`.
- [x] The same section also renders what WAS delivered: `telegram-flush` appends a `{id, delivered_ts_ns}` record to `journal/telegram/delivered.jsonl` per successful send (never on a failed one — the dispatch stays pending in the batch), the loader round-trips/sorts/fails-closed on the log, and both renderers show the "Delivered this month (Telegram delivery log)" table or the explicit no-deliveries state (OPS-6). `ops_6_telegram_delivered_loads_rows_fail_closed`, `ops_9_telegram_flush_failure_keeps_batch_and_never_logs_delivered`, `ops_6_report_delivery_accountability_section_renders`.
- [x] Band-accuracy drift/decay raises the `band-accuracy-decay` P3 alert on a sustained trailing-window quality loss and stays silent on healthy/young/never-good trends (OPS-13). `ops_13_band_accuracy_decay_alerts_on_sustained_quality_loss`, `ops_13_band_accuracy_decay_ignores_healthy_and_young_trends`.
- [x] The weekly verdict is journaled to `runs/index.jsonl` as a `band_accuracy_decay` record line correlated to the study's run by `run_id` + `week` (clean verdicts journaled too; an empty trend writes nothing), and the wrapper passes `--runs-dir` to the decay check (OPS-13). `ops_13_mp_ops_decay_journals_verdict_to_runs_index`, `ops_13_weekly_wrapper_invokes_decay_check_after_study`.
- [x] The batch ledger is watched near-real-time: `stale_batch_alert` raises `telegram-stale` (P2) when a dispatch sits queued ≥ 24h (oldest wins, per-entity dedupe key, detail names queued hours + remediation); `mp-ops telegram-stale` prints the JSON verdict, stays silent on fresh/missing ledgers, fails closed on a corrupt one (exit 2); with `--telegram` the P2 is SENT even inside quiet hours (OPS-9 breakthrough, verified against the Bot API stub); and the hourly `telegram-stale.timer`/`.service` pair is pinned read-only on the ledger with `WorkingDirectory=/opt/money-printer` so the relative `journal/telegram` resolves under the money-printer tree, not systemd's `/` (OPS-14). `ops_14_telegram_stale_alert_fires_on_missed_flush`, `ops_14_telegram_stale_subcommand_reports_verdict`, `ops_14_telegram_stale_p2_breaks_through_quiet_hours`, `ops_14_telegram_stale_timer_runs_hourly_and_reads_only_the_ledger`.
- [x] Restore drill restores a real tarball into a scratch dir, refuses a backup missing the business records, and verifies via an injectable command (default: the sim golden fixture) (OPS-5). `ops_5_restore_drill_restores_a_backup_and_verifies`, `ops_5_restore_drill_script_exists_and_refuses_without_backup`.
- [x] Bot command surface: exact commands parsed, single-owner allowlist, every command journaled, `/kill` one confirm and `/flatten` double confirm, latch reaches the real gate as RG-10 (OPS-3). `ops_3_bot_allowlists_owner_and_journals_every_command`, `ops_3_kill_needs_confirm_and_flatten_needs_double_confirm`.
- [ ] The `opsd` binary (live heartbeat endpoint feeding `DeadMan`, host sampling feeding `watch.rs`, Telegram transport feeding `Bot`) and the once-per-host OPS-8 verbatim bring-up — the deterministic cores are all implemented and tested above; the long-running process wiring is the remaining work.

## Decisions
- 2026-07-10: Telegram (not Discord) as the command channel — better mobile
  interrupt behavior; single-owner allowlist model.
- 2026-07-11: Ops deterministic core implemented as the `mp-ops` crate. The
  logic that must be correct — alert dedupe + quiet-hours batching (OPS-4/9),
  the dead-man switch (OPS-2), the kill-latch bridge (OPS-3/RG-10), and the
  monthly-report renderer (OPS-6) — is clock-injected and I/O-free so it is
  unit-testable offline (8 tests, IDs in names). Networked/host surfaces (the
  Telegram bot transport, the external watcher ping, live heartbeat HTTP) are
  deployment artifacts, not decision-path code, and ship as `ops/deploy.md`,
  `ops/compose.yaml`, `ops/systemd/*.service`, `ops/restore-drill.sh`, and the
  `ops/runbooks/` set — the wiring is documented and reproducible (OPS-8) but
  the bot's live command-execution loop is deferred (status stays
  `implementing`).
- 2026-07-11: OPS-3 kill-latch is a JSON file (`KillLatch` → `LatchScope`)
  that deserializes into `mp_risk::KillSwitches`, so `/kill` and `/flatten`
  reach the gate as a file read, independent of oms health (RG-10). `mp_risk::
  Scope` is not itself `Serialize`, so `LatchScope` is a portable mirror with a
  `to_scope()` conversion — the latch format stays decoupled from internal
  types. Latches remain one-way; only a human clears the file (EXE-7).
- 2026-07-11: OPS-4 "alert without a runbook fails CI" is enforced two ways:
  an in-crate test (`ops_4_every_catalog_alert_has_a_runbook_file`) and a
  guardrails lint that extracts every `alert!("id", …)` from the registry and
  checks `ops/runbooks/{id}.md` exists (verified it fails CI when a runbook is
  removed). The 11 P1/P2 alert ids from the policy table each have a runbook
  (symptoms/diagnosis/remediation/escalation); remediation steps are risk-off
  only (never widen a limit, PD-1).
- 2026-07-11 (audit): `Alert.dedupe_key` added — dead-man alerts dedupe per
  process, so one process's death never suppresses another's
  (`regression_audit3_*`, docs/AUDIT-2026-07-11.md). OPS-7 decision functions
  implemented (`watch.rs`: disk/clock-skew/keyfile checks, alert-only, W-6)
  and deployment artifacts pinned by tests (`ops_1/5/8/10_*`). Test names
  normalized to `ops_N_*` for CONV-21 traceability. Remaining before
  `implemented`: the live Telegram command loop (OPS-3's command surface,
  owner allowlist, command journaling) and the opsd host-sampling loop that
  feeds `watch.rs`.
- 2026-07-11 (fix-all): OPS-3's command surface implemented as the `Bot`
  state machine (parse → allowlist → confirm flows → journal → latch), fully
  offline-testable; the Telegram HTTP transport remains a thin binary-edge
  loop with no decision logic. OPS-5's drill got an injectable verifier
  (`MP_DRILL_VERIFY_CMD`, default = the sim golden fixture) so the full
  restore path runs in CI without nesting cargo. Status stays `implementing`
  for exactly one reason: the long-running `opsd` process (heartbeat HTTP +
  host sampling + bot transport) is not built — its decision cores are.
- 2026-08-06: the RES-4 band-accuracy trend (spec 029 LIQ-6) is wired into the
  monthly report as a grounded section (OPS-6). `ops::report` owns the shape:
  `parse_band_accuracy_trend` is the pure, fail-closed parser (CONV-8 — every
  non-empty line must be the job's exact journal shape, well-typed; a corrupt
  line fails the load naming the line, never a silently dropped week);
  `load_band_accuracy_trend` is the thin read boundary — a missing journal is
  a "no data" month (RES-5), not an error, so an early deployment's report
  renders the honest empty row. The renderer stays I/O-free; the caller
  grounds the section by passing the loaded rows.
- 2026-08-06: the HTML renderer is hand-built from the same input struct as
  the markdown (`MonthlyReport::render_html`), not a markdown→HTML converter
  — the report's markup is fully controlled, so rendering directly needs zero
  new dependencies and the numbers share ONE input source (the struct). The
  two formatters are pinned together by a parity test
  (`ops_6_report_html_matches_markdown_cells`: every markdown table cell must
  appear in the HTML), so a one-sided edit to a section fails CI. It is a
  self-contained page (inline CSS, HTML-escaped dynamic strings) written via
  `write_monthly_report(dir, report)` as `report.md` + `report.html` under
  `journal/reports/{YYYY-MM}/`. Rendered files are derived output, not
  append-only evidence (W-6): regenerating a month overwrites its renders.
- 2026-08-06: OPS-13 — the trend is also watched for drift/decay.
  `band_accuracy_decay_alert` mirrors the RES-3 edge-decay flag (spec 010):
  ≥ 12 graded weeks, trailing 4-week mean vs the 12-week mean, coverage below
  half (baseline ≥ 0.5) or MRE more than doubled (baseline > 0) raises the
  `band-accuracy-decay` P3 alert. The baseline guards mirror RES-3's "a
  never-good edge is a kill decision, not decay" (a grade that was never
  ≥ 50% covered is a different problem). The trailing rows are the most recent
  *graded* weeks — the study skips weeks without census data (LIQ-10), so ISO
  gaps are not decay. `mean12` is the full 12-row window mean (RES-3-faithful:
  the recent 4 weeks are included in it), not a disjoint prior window.
  Invocation: `run_whale_study_weekly.sh` calls `mp-ops band-accuracy-decay
  --trend $OUT_DIR/band_accuracy.jsonl --telegram` after each study
  (best-effort — a missing mp-ops binary or unreadable journal warns in
  journald and never fails the study; the monthly report surfaces the same
  journal). Delivery edge (2026-08-06): the CLI routes the fired P3 through
  `AlertRouter` with env-overridable quiet hours (MP_OPS_QUIET_START_MIN /
  MP_OPS_QUIET_END_MIN, default 22:00–07:00 UTC) — Sent now via
  `post_telegram` (curl to the Bot API, `disable_notification=true`),
  Batched into `journal/telegram/batch.jsonl` (append-only, fsynced, W-6),
  or gated `"unconfigured"` without credentials. The weekly wrapper runs
  `mp-ops telegram-flush --wait`, which waits out the quiet window
  internally (sleeping only while the window is active — a wrap-around
  window never sleeps ~24h for a flush that started after quiet end; the
  `MP_OPS_SLEEP` env overrides the sleeper for tests) and then drains
  (fail-closed: any send
  failure keeps the batch and exits 2, and a mid-batch failure rewrites the
  ledger to the un-sent lines so a retry never duplicates a delivery). TLS is the host's: the ops crate stays TLS-free
  like `post_p1_webhook`, so the send shells out to `curl`;
  `MP_OPS_TELEGRAM_URL` overrides the endpoint (tests use a local stub).
  `Dispatch::from_alert` is the shared constructor, so what gets batched is
  exactly what would have been sent.  Dedupe is in-process only: a persistent
  decay re-alerts on each weekly run by design (P3 FYI, the 7-day window is
  for a future always-on consumer). Credentials come from ops.env
  (`TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHAT_ID` — deploy.md). The weekly verdict
  is also journaled: `band-accuracy-decay --runs-dir <runs>` appends a
  `band_accuracy_decay` record line to `runs/index.jsonl` — the same RES-4
  tracker the `whale_study` binary owns (SIM-10) — carrying the printed
  verdict (decayed/alert/telegram) plus `run_id` + `week` echoed from the
  latest trend line, so each week's study record and its drift verdict live
  next to each other in one append-only journal (W-6). A clean verdict is
  journaled too — the tracker records that the watch ran and passed; an
  empty trend (no graded week yet) has no run to attach and writes nothing
  (RES-5). Fail-closed: an unwritable tracker exits 2 (the wrapper warns in
  journald, never fails the study). Verdict lines are per check invocation —
  like the study's per-run records, a re-fired timer appends another line for
  the same week (append-only W-6; readers join on run_id/week). A Telegram
  delivery failure never blanks the week's verdict: the record is journaled
  with `"telegram": "send_failed"` and the command still exits 2 (verified
  by `ops_13_mp_ops_decay_send_failure_still_journals_verdict`). Delivery
  accountability (2026-08-06): the monthly report gains a section grounded on
  the same ledger — `load_telegram_batch` reads `journal/telegram/
  batch.jsonl` (the `Dispatch` shape `append_batch` writes; fail-closed on a
  corrupt line, CONV-8) into pending rows with injected-clock queue age, and
  the section renders them as "Delivery Accountability (Telegram queue)" —
  alert/severity/queue age/detail plus the runbook link a human needs to
  remediate an undelivered alert. An empty ledger renders the strictly
  grounded "No pending alerts in the quiet-hours Telegram queue" (the ledger
  records only what is queued now, not delivery history), so an alert stuck
  in the queue at month-end is visible, never silently lost (W-6). The loader
  is also what caught and fixed a latent bug: `append_batch` previously
  wrote dispatches without a trailing newline, so a second dispatch
  concatenated onto the first line (the ledger is JSONL, W-6); a ledger line
  written that way fails closed on read. Delivery log (2026-08-06):
  `telegram-flush` also appends a flushed record `{id, delivered_ts_ns}` to
  `journal/telegram/delivered.jsonl` per successful send — before the
  dispatch leaves the batch, fsynced append-only like the ledger (W-6) — and
  the monthly report renders it as "Delivered this month (Telegram delivery
  log)": what WAS delivered (id + relative delivery time from the injected
  clock, PD-3), the accountability counterpart to the pending queue — the
  report answers both questions with evidence: what is stuck, and what the
  ledger actually delivered. A failed dispatch is NEVER logged as delivered
  (it stays pending in the batch, visible as a gap); `flush_batch` takes the
  injected `now_ns` (PD-3) so delivery timestamps are deterministic in
  tests. Stale-batch watch (2026-08-06): the same ledger is watched
  near-real-time by OPS-14 — `stale_batch_alert` (pure, clock-injected,
  PD-3) raises `telegram-stale` (P2) when a dispatch has been queued ≥ 24h
  (one full quiet window), the OLDEST stale dispatch wins, and the alert's
  dedupe key is that dispatch's id (regression_audit3 pattern) so one stuck
  alert never suppresses another; `mp-ops telegram-stale` (hourly via
  `telegram-stale.timer`) prints a JSON verdict and with `--telegram` sends
  the P2 immediately — P2 breaks through quiet hours (OPS-9), so the alert
  escapes the very batch that is stuck instead of being re-queued into it.
  Requirement numbering continues at
  OPS-13/OPS-14 because OPS-11/OPS-12 belong to spec 021's bot journal.

## Open questions
- Phone-call escalation provider for P1 (Twilio vs a healthchecks add-on) —
  owner picks by budget.
