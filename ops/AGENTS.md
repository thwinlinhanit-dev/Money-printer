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
- `src/report.rs` — report generation
- `src/watch.rs` — watch loops
- `src/bin/mp-ops.rs` — ops CLI
- `src/bin/opsd.rs` — ops daemon binary
- `runbooks/` — incident response runbooks (10 runbooks)
- `systemd/` — systemd service/timer units
- `ci/` — CI guardrails
- `compose.yaml` — Docker Compose
- `deploy.md` — deployment instructions
- `watchdog_collectors.ps1` — 24/7 collector watchdog

## Local Contracts

- Collector watchdog uses `Spawn-Collector` with `UseShellExecute=true` for full process detachment
- Deadman detection checks heartbeat files with configurable grace period
- Runbooks must be followed for incident response; update runbook if procedure changes

## Verification

- `cargo test -p mp-ops`
- `cargo test -p mp-ops --test ops_slice`

## Child DOX Index

None.
