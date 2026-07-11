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
| P2 | data/edge degrading | Telegram | stream gap > 5min, collector down, disk > 85%, determinism diff (SIM-11) |
| P3 | FYI | Telegram (quiet hours batched) | funnel transitions, daily digest, screener hits (if enabled) |

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
transitions + kills, benchmark row (vs BTC hold, vs T-bill). Rendered to
markdown + HTML in `journal/reports/{YYYY-MM}/`. An LLM MAY draft the prose
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
  journals/tracker only (no hand-entered numbers); benchmark row REQUIRED.
- **OPS-7** `opsd` MUST watch disk (STO-7), clock skew (NTP; warn > 100ms —
  lead-lag research and venue timestamps depend on it), and cert/key file
  permissions (0600) — each with alerts + runbooks.
- **OPS-8** Deployment MUST be reproducible: one `ops/deploy.md` +
  `compose.yaml`/units checked in; a fresh VPS reaches running-collector
  state by following the doc verbatim (test this once, note the date).
- **OPS-9** Quiet hours (config) batch P3s; P1/P2 always break through.
- **OPS-10** Log retention: journals forever (they are the business record,
  W-6); process logs 30 days rotated.

## Acceptance criteria
- [x] Dead-man fires only after the missed-beat deadline and escalates P2→P1 for a critical proc in live mode (OPS-2). `ops_2_deadman_fires_after_three_missed_beats_and_escalates_in_live`. (The literal `kill -9`→restart supervision is systemd's `Restart=always` in `ops/systemd/*.service`, exercised at deploy time per OPS-8, not in-crate.)
- [x] `/kill` latch → real `mp_risk::evaluate` rejects the next intent with the RG-10 `KillSwitchTripped` verdict, independent of oms (OPS-3). `ops_3_kill_latch_makes_the_real_gate_reject_with_rg10` (plus `ops_3_kill_latch_roundtrips_and_trips_kill_switches`, `ops_3_flatten_is_global_kill`).
- [x] Alert-without-runbook fails CI (OPS-4). `ops_4_every_catalog_alert_has_a_runbook_file` + the guardrails lint (verified to exit non-zero on a removed runbook).
- [x] Alert dedupe + quiet-hours P3 batching with P1/P2 breakthrough (OPS-4/9). `ops_4_alert_dedupes_within_window_then_fires_again`, `ops_9_quiet_hours_batch_p3_but_p1_breaks_through`.
- [x] Monthly report generates from fixture inputs with all §13 sections + the required benchmark row, numbers grounded (OPS-6). `ops_6_report_has_all_sections_and_benchmark_row`, `ops_6_report_numbers_are_grounded_not_invented`.
- [ ] Restore-drill script exercised against a real off-host backup tarball (OPS-5) — `ops/restore-drill.sh` is implemented (decrypt → extract → golden-sim replay), but a true end-to-end pass needs an actual encrypted backup, which is an operational step, not a unit test.

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

## Open questions
- Phone-call escalation provider for P1 (Twilio vs a healthchecks add-on) —
  owner picks by budget.
