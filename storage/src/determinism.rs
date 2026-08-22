//! Daily determinism artifact (spec 018 MOD-9..11) — the per-day proof that
//! the decision path is reproducible. Written by `mp-determinism` (sim crate)
//! after the daily scorecard, read by the promotion gate (`mp-ops promote`).
//! A gate INPUT, not gate data: the scorecard verdict always comes from the
//! audit; the determinism artifact adds the second, orthogonal condition —
//! every day in the qualifying window must also carry a PASSING determinism
//! artifact, or the window does not promote (MOD-9 "a diff MUST block
//! promotion").
//!
//! Layout: `data/scorecards/{date}.determinism.json`, sibling of the scorecard
//! (the `.determinism.json` stem deliberately fails the scorecard loader's
//! `YYYY-MM-DD.json` filter, so the two never collide).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One day's determinism verdict, as persisted by `mp-determinism --write`.
/// The gate reads `passed` + `date` only; the rest is the `why` and the
/// reproducibility evidence. Fail-closed semantics: a MISSING or corrupt
/// artifact is a not-passed day for the gate (absence of proof is proof of
/// absence for promotion — the check must have run and passed).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeterminismArtifact {
    pub date: String,
    /// Replay produced a byte-identical decision log across two fresh runs
    /// (and matched the live/paper log when one existed).
    #[serde(default)]
    pub passed: bool,
    /// Two fresh runs over the same recorded session were byte-identical.
    #[serde(default)]
    pub self_consistent: bool,
    /// Whether a live/paper decision log existed to compare against.
    #[serde(default)]
    pub live_present: bool,
    /// Replay vs live/paper byte-identity (None when no live log existed).
    #[serde(default)]
    pub live_matches: Option<bool>,
    /// Strategy the replay ran (`carry-v1` | `null`).
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub event_count: Option<u64>,
    #[serde(default)]
    pub replayed_lines: Option<u64>,
    /// Rolling FNV-1a hash of the replayed decision log (CONV-12).
    #[serde(default)]
    pub replayed_hash: Option<u64>,
    #[serde(default)]
    pub reason: Option<String>,
    /// When the check ran (evidence; the gate ignores it).
    #[serde(default)]
    pub ts_ns: Option<i64>,
}

/// `{score_dir}/{date}.determinism.json` (spec 018 MOD-9 artifact layout).
pub fn determinism_file(score_dir: &Path, date: &str) -> PathBuf {
    score_dir.join(format!("{date}.determinism.json"))
}

/// Load the artifact for `date` from the scorecards dir. `Err` on a CORRUPT
/// file (fail-closed: the gate must know the proof cannot be read), `Ok(None)`
/// when absent.
pub fn load_determinism(
    score_dir: &Path,
    date: &str,
) -> Result<Option<DeterminismArtifact>, String> {
    let path = determinism_file(score_dir, date);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            serde_json::from_str(text)
                .map(Some)
                .map_err(|e| format!("parse {}: {e}", path.display()))
        }
        Err(_) => Ok(None),
    }
}

/// Serialize + write the artifact (replacing any prior verdict for the date —
/// a re-check supersedes, W-6 applies to recorded data, not gate artifacts).
pub fn write_determinism(
    score_dir: &Path,
    artifact: &DeterminismArtifact,
) -> Result<PathBuf, String> {
    let path = determinism_file(score_dir, &artifact.date);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(artifact)
        .map_err(|e| format!("serialize determinism artifact: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}
