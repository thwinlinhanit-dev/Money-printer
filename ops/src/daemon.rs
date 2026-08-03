//! The small, real `opsd` core: heartbeat ingestion plus a read-only status
//! snapshot.  Network/Telegram presentation is deliberately outside this
//! deterministic state holder, so an unauthenticated HTTP endpoint can never
//! enable trading or clear a kill latch.

use crate::{Alert, AlertRouter, DeadMan, RouteOutcome};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const NS_PER_SEC: i64 = 1_000_000_000;

#[derive(Debug, Clone, Serialize)]
pub struct ProcessHealth {
    pub process: String,
    pub last_beat_ns: i64,
    pub age_ns: i64,
    pub fresh: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub now_ns: i64,
    pub processes: Vec<ProcessHealth>,
    pub raw_log_count: usize,
    pub last_event_age_ns: Option<i64>,
    pub manifest_count: usize,
    pub mode: &'static str,
}

/// Long-lived operational state.  `mode` is deliberately read-only: opsd can
/// alert and persist a kill latch through the bot path, but cannot turn risk
/// on or transition the system to live (PD-1).
pub struct OpsDaemon {
    data_dir: PathBuf,
    beats: BTreeMap<String, i64>,
    deadman: DeadMan,
    router: AlertRouter,
    heartbeat_interval_ns: i64,
}

impl OpsDaemon {
    pub fn new(data_dir: impl Into<PathBuf>, heartbeat_interval_ns: i64) -> Self {
        Self {
            data_dir: data_dir.into(),
            beats: BTreeMap::new(),
            deadman: DeadMan::new(heartbeat_interval_ns),
            router: AlertRouter::new(None),
            heartbeat_interval_ns,
        }
    }

    /// Ingest an explicit `POST /beat/{process}` heartbeat.
    pub fn beat(&mut self, process: impl Into<String>, now_ns: i64) {
        let process = process.into();
        if !self.beats.contains_key(&process) {
            self.deadman.register(&process, false, now_ns);
        }
        self.deadman.beat(&process, now_ns);
        self.beats.insert(process, now_ns);
    }

    /// Ingest collector heartbeat files written by the Windows watchdog path.
    pub fn ingest_heartbeat_files(&mut self, now_ns: i64) {
        let raw_dir = self.data_dir.join("raw");
        let Ok(entries) = std::fs::read_dir(raw_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("heartbeat") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(seconds) = text
                .split_whitespace()
                .find_map(|part| part.strip_prefix("ts="))
                .and_then(|value| value.parse::<i64>().ok())
            else {
                continue;
            };
            let process = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("collector");
            self.beat(
                process.to_owned(),
                seconds.saturating_mul(NS_PER_SEC).min(now_ns),
            );
        }
    }

    /// Run the dead-man check and route resulting alerts.  The binary logs
    /// routed alerts; integration may attach Telegram without changing this
    /// safety-critical state path.
    pub fn check_alerts(&mut self, now_ns: i64) -> Vec<Alert> {
        self.deadman
            .check(now_ns, false)
            .into_iter()
            .filter(|alert| matches!(self.router.route(alert, now_ns), RouteOutcome::Sent(_)))
            .collect()
    }

    pub fn status(&self, now_ns: i64) -> StatusSnapshot {
        let processes = self
            .beats
            .iter()
            .map(|(process, beat)| {
                let age_ns = now_ns.saturating_sub(*beat);
                ProcessHealth {
                    process: process.clone(),
                    last_beat_ns: *beat,
                    age_ns,
                    fresh: age_ns <= self.heartbeat_interval_ns.saturating_mul(3),
                }
            })
            .collect();
        let raw_dir = self.data_dir.join("raw");
        let raw_logs = files_with_extension(&raw_dir, "log");
        let last_event_age_ns = raw_logs
            .iter()
            .filter_map(|path| std::fs::metadata(path).ok()?.modified().ok())
            .filter_map(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|modified| now_ns.saturating_sub(modified.as_nanos() as i64))
            .min();
        let manifest_count =
            files_with_extension(&self.data_dir.join("cold").join("manifests"), "json").len();
        StatusSnapshot {
            now_ns,
            processes,
            raw_log_count: raw_logs.len(),
            last_event_age_ns,
            manifest_count,
            mode: "read-only",
        }
    }
}

fn files_with_extension(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(files_with_extension(&path, extension));
        } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            files.push(path);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_11_opsd_ingests_heartbeat_and_reports_freshness() {
        let root = std::env::temp_dir().join(format!("mp-opsd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raw")).unwrap();
        std::fs::write(
            root.join("raw/mp-collector-binance-BTCUSDT.heartbeat"),
            "ts=10 pid=1\n",
        )
        .unwrap();
        let mut daemon = OpsDaemon::new(&root, 30 * NS_PER_SEC);
        daemon.ingest_heartbeat_files(11 * NS_PER_SEC);
        let status = daemon.status(20 * NS_PER_SEC);
        assert_eq!(status.processes.len(), 1);
        assert!(status.processes[0].fresh);
        assert_eq!(status.mode, "read-only");
        let _ = std::fs::remove_dir_all(root);
    }
}
