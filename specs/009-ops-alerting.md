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
- [ ] Chaos: `kill -9` any process ⇒ supervisor restarts it, dead-man does NOT fire; stop it fully ⇒ alert within 2 min (OPS-1/2).
- [ ] Telegram /kill writes the latch; risk gate rejects intents on next check with RG-10 verdict — proven in an integration test with oms running (OPS-3).
- [ ] Alert-without-runbook fails CI (OPS-4).
- [ ] Restore drill script passes on a scratch dir (OPS-5).
- [ ] Monthly report generates from fixture journals with all §13 sections + benchmark row (OPS-6).

## Decisions
- 2026-07-10: Telegram (not Discord) as the command channel — better mobile
  interrupt behavior; single-owner allowlist model.

## Open questions
- Phone-call escalation provider for P1 (Twilio vs a healthchecks add-on) —
  owner picks by budget.
