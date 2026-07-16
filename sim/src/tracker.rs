//! Experiment tracker (SIM-10). Every run is reproducible from its record
//! alone: config hash + git SHA + data range + manifest hashes + decision-log
//! hash + a metrics summary. Records serialize to one JSONL line for
//! `runs/index.jsonl`; the `run_id` is supplied by the binary edge (a ULID in
//! production) so this crate stays wall-clock-free (PD-3).

use crate::harness::MetricsSummary;
use mp_core::fnv1a_64;

/// Non-cryptographic FNV-1a content hash used for config/manifest identity
/// (matches the decision-log hash family; sufficient for "have we tried this?").
pub fn content_hash(bytes: &[u8]) -> u64 {
    fnv1a_64(bytes)
}

/// One tracked run.
#[derive(Debug, Clone)]
pub struct RunRecord {
    /// Caller-supplied unique id (ULID in production).
    pub run_id: String,
    pub git_sha: String,
    pub config_hash: u64,
    /// `[from_ns, to_ns)` of the consumed data.
    pub data_from_ns: i64,
    pub data_to_ns: i64,
    /// Hashes of each consumed stream's quality manifest (order-stable).
    pub manifest_hashes: Vec<u64>,
    pub decision_log_hash: u64,
    pub metrics: MetricsSummary,
}

impl RunRecord {
    /// Build a record, hashing the full config text (SIM-10 reproducibility).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: impl Into<String>,
        git_sha: impl Into<String>,
        config_text: &str,
        data_from_ns: i64,
        data_to_ns: i64,
        manifest_hashes: Vec<u64>,
        decision_log_hash: u64,
        metrics: MetricsSummary,
    ) -> Self {
        RunRecord {
            run_id: run_id.into(),
            git_sha: git_sha.into(),
            config_hash: content_hash(config_text.as_bytes()),
            data_from_ns,
            data_to_ns,
            manifest_hashes,
            decision_log_hash,
            metrics,
        }
    }

    /// A single JSONL line for `runs/index.jsonl` (append-only, W-6).
    pub fn to_jsonl(&self) -> String {
        let mh = self
            .manifest_hashes
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"run_id\":\"{}\",\"git_sha\":\"{}\",\"config_hash\":{},\"data_from_ns\":{},\"data_to_ns\":{},\"manifest_hashes\":[{}],\"decision_log_hash\":{},\"trades\":{},\"expectancy\":{},\"stress_expectancy_2x\":{},\"max_drawdown\":{}}}",
            self.run_id,
            self.git_sha,
            self.config_hash,
            self.data_from_ns,
            self.data_to_ns,
            mh,
            self.decision_log_hash,
            self.metrics.trades,
            self.metrics.expectancy,
            self.metrics.stress_expectancy_2x,
            self.metrics.max_drawdown,
        )
    }

    /// Whether two records describe the same experiment (same config + data +
    /// manifests) — the "have we tried this?" identity, ignoring the run_id.
    pub fn same_experiment(&self, other: &RunRecord) -> bool {
        self.config_hash == other.config_hash
            && self.data_from_ns == other.data_from_ns
            && self.data_to_ns == other.data_to_ns
            && self.manifest_hashes == other.manifest_hashes
    }
}
