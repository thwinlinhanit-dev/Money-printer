//! `mp-ops` — monitoring, alerting, and reporting for the 24/7 system
//! (spec 009). Ops is part of the edge: a recorder that dies silently, or a
//! live loop nobody can flatten from a phone, is how the moat and the account
//! stop existing.
//!
//! This crate is the deterministic core: the alert framework (dedupe, quiet
//! hours), the dead-man switch (alert on absence), the kill-latch bridge that
//! lets `/kill` reach the risk gate even when oms is wedged (RG-10), and the
//! monthly-report renderer. Networked surfaces (the Telegram bot transport,
//! systemd units, the external watcher) are deployment artifacts under `ops/`
//! (`deploy.md`, `compose.yaml`, `runbooks/`), not decision-path code — the
//! logic here is clock-injected and I/O-free (PD-3).

pub mod alert;
pub mod deadman;
pub mod latch;
pub mod registry;
pub mod report;
pub mod watch;

pub use alert::{Alert, AlertRouter, Channel, Dispatch, QuietHours, RouteOutcome, Severity};
pub use deadman::DeadMan;
pub use latch::{KillLatch, LatchError, LatchScope};
pub use registry::{runbook_path, spec_for, AlertSpec, ALERTS};
pub use report::{Benchmark, CostBreakdown, FunnelEvent, MonthlyReport, StrategyRow, TrackingRow};
pub use watch::{clock_skew_alert, disk_alert, keyfile_perms_alert};
