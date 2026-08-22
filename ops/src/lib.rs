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
pub mod bot;
pub mod daemon;
pub mod deadman;
pub mod journal;
pub mod latch;
pub mod registry;
pub mod report;
pub mod storage;
pub mod telegram;
pub mod watch;

pub use alert::{Alert, AlertRouter, Channel, Dispatch, QuietHours, RouteOutcome, Severity};
pub use bot::{parse as parse_command, Bot, BotReply, Command, KillScope};
pub use daemon::{OpsDaemon, ProcessHealth, StatusSnapshot};
pub use deadman::DeadMan;
pub use latch::{KillLatch, LatchError, LatchScope};
pub use registry::{runbook_path, spec_for, AlertSpec, ALERTS};
pub use report::{
    append_run_record, band_accuracy_decay_alert, load_band_accuracy_trend,
    parse_band_accuracy_trend, write_monthly_report, BandAccuracyRow, Benchmark, CostBreakdown,
    FunnelEvent, MonthlyReport, StrategyRow, TrackingRow,
};
pub use storage::{
    days_from_yyyymmdd, growth_rate_per_day, held_drain_files, parse_drain_manifest_line,
    project_storage, sample_daily_sizes, storage_budget_alert, DrainManifestEntry,
    StorageProjection,
};
pub use telegram::{
    append_batch, append_delivered, flush_batch, load_telegram_batch, load_telegram_delivered,
    post_telegram, stale_batch_alert, TelegramConfig, TelegramDeliveredRow, TelegramPendingRow,
};
pub use watch::{clock_skew_alert, disk_alert, keyfile_perms_alert};
