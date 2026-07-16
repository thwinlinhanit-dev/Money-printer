//! Per-stream staleness watchdog (COL-2). Uses injected time; emits nothing
//! itself — the driver turns a stale verdict into a `Status::Stale` event and
//! a reconnect.

use mp_core::Nanos;
use std::collections::BTreeMap;

/// Tracks last-seen time per stream topic and flags streams that went quiet.
#[derive(Debug, Clone, Default)]
pub struct Staleness {
    /// topic -> (last_recv_ns, threshold_ns)
    streams: BTreeMap<String, (Nanos, Nanos)>,
    default_threshold_ns: Nanos,
}

impl Staleness {
    pub fn new(default_threshold_ns: Nanos) -> Self {
        Self {
            streams: BTreeMap::new(),
            default_threshold_ns,
        }
    }

    /// Register a per-topic threshold (overrides the default).
    pub fn set_threshold(&mut self, topic: &str, threshold_ns: Nanos) {
        let entry = self
            .streams
            .entry(topic.to_owned())
            .or_insert((0, threshold_ns));
        entry.1 = threshold_ns;
    }

    /// Record that `topic` produced a message at `recv_ts_ns`.
    pub fn observe(&mut self, topic: &str, recv_ts_ns: Nanos) {
        let default = self.default_threshold_ns;
        let entry = self
            .streams
            .entry(topic.to_owned())
            .or_insert((recv_ts_ns, default));
        entry.0 = recv_ts_ns;
    }

    /// Topics whose last message is older than their threshold at `now_ns`.
    /// Deterministic order (BTreeMap, CONV-10).
    pub fn stale_streams(&self, now_ns: Nanos) -> Vec<String> {
        self.streams
            .iter()
            .filter(|(_, (last, thresh))| *thresh > 0 && now_ns - *last > *thresh)
            .map(|(topic, _)| topic.clone())
            .collect()
    }
}
