//! Dead-man switch (OPS-2): alert on ABSENCE. Each process beats every
//! `interval_ns`; missing `miss_threshold` beats raises an alert. In live mode
//! a missed critical process (oms) escalates P2 → P1. Time is injected.

use crate::alert::{Alert, Severity};
use std::collections::BTreeMap;

/// Tracks last-seen heartbeats per process and detects silence.
#[derive(Debug, Clone)]
pub struct DeadMan {
    interval_ns: i64,
    miss_threshold: i64,
    last_beat: BTreeMap<String, i64>,
    critical: BTreeMap<String, bool>,
}

impl DeadMan {
    /// `interval_ns` is the expected beat period (spec 009: 30s); default
    /// threshold is 3 missed beats before alerting.
    pub fn new(interval_ns: i64) -> Self {
        DeadMan {
            interval_ns,
            miss_threshold: 3,
            last_beat: BTreeMap::new(),
            critical: BTreeMap::new(),
        }
    }

    pub fn with_threshold(mut self, misses: i64) -> Self {
        self.miss_threshold = misses;
        self
    }

    /// Register a process. `critical` procs (oms) escalate to P1 in live mode.
    /// Registering seeds the first beat so a just-started proc is not instantly
    /// flagged; call `beat` as real heartbeats arrive.
    pub fn register(&mut self, proc: impl Into<String>, critical: bool, now_ns: i64) {
        let p = proc.into();
        self.last_beat.insert(p.clone(), now_ns);
        self.critical.insert(p, critical);
    }

    /// Record a heartbeat for `proc` at `ts_ns`.
    pub fn beat(&mut self, proc: &str, ts_ns: i64) {
        self.last_beat.insert(proc.to_string(), ts_ns);
    }

    /// Deadline (ns) after which a process is considered silent.
    fn deadline(&self) -> i64 {
        self.interval_ns.saturating_mul(self.miss_threshold)
    }

    /// Alerts for every process silent past its deadline at `now_ns`. In live
    /// mode a silent critical process is P1; otherwise P2 (spec 009 policy).
    /// Deterministic order (BTreeMap iteration, CONV-10).
    pub fn check(&self, now_ns: i64, live: bool) -> Vec<Alert> {
        let deadline = self.deadline();
        let mut out = Vec::new();
        for (proc, &last) in &self.last_beat {
            let silent_for = now_ns.saturating_sub(last);
            if silent_for > deadline {
                let is_critical = *self.critical.get(proc).unwrap_or(&false);
                let sev = if live && is_critical {
                    Severity::P1
                } else {
                    Severity::P2
                };
                out.push(Alert::new(
                    "process-deadman",
                    sev,
                    // Dedupe on the beat interval so we don't spam every check.
                    self.interval_ns,
                    format!("process '{proc}' silent for {silent_for}ns (> {deadline}ns)"),
                ));
            }
        }
        out
    }
}
