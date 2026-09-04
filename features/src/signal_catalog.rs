//! Signal catalog (spec 025): the funnel applied to *features* rather than
//! strategies. Every signal (a rule over one or more features, or a single
//! feature family like `footprint.imb.*`) gets a lifecycle: Hypothesis →
//! Tested → Graded → Deployed, with automatic decay re-testing and terminal
//! kill — the same asymmetry as the strategy funnel (strategies/funnel.rs):
//! **promotion needs evidence, demotion is automatic** (STR-3/5/6 mirrored).
//!
//! Pure: this module holds no I/O and no wall clock (PD-3). `now_ns` is a
//! parameter everywhere it matters; persistence is serde (the binary edge
//! reads/writes the JSON file). Decay math mirrors `research/grading.py`
//! `decay_flag` so Rust and Python arms cannot disagree on what "decayed"
//! means (RES-3).

use crate::signal_identity::SignalResearchIdentity;
use serde::{Deserialize, Serialize};

/// Lifecycle stages, ordered by rank. `Decayed` is a transient marker used by
/// the decay re-test path before automatic demotion to `Hypothesis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalStage {
    Hypothesis,
    Tested,
    Graded,
    Deployed,
    Decayed,
    Killed,
}

impl SignalStage {
    fn rank(self) -> i32 {
        match self {
            SignalStage::Hypothesis => 0,
            SignalStage::Tested => 1,
            SignalStage::Graded => 2,
            SignalStage::Deployed => 3,
            SignalStage::Decayed => 0,
            SignalStage::Killed => -1,
        }
    }

    fn next_up(self) -> Option<SignalStage> {
        Some(match self {
            SignalStage::Hypothesis => SignalStage::Tested,
            SignalStage::Tested => SignalStage::Graded,
            SignalStage::Graded => SignalStage::Deployed,
            SignalStage::Deployed | SignalStage::Killed => return None,
            SignalStage::Decayed => SignalStage::Hypothesis,
        })
    }
}

/// One grading batch (a research run over the hit journal, spec 017/025).
///
/// REL-4/REL-5 (spec 054): the grade carries the [`SignalResearchIdentity`]
/// fingerprint of the run that produced it; `apply_grade` refuses any grade
/// whose identity does not match the record's CURRENT identity. A params /
/// feature-version / data-schema / cost-model change therefore invalidates
/// all older grades — they can never silently promote. `#[serde(default)]`
/// keeps pre-hardening catalog files parseable: their grades decode with
/// `identity = ""`, which matches nothing and is refused loudly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeSnapshot {
    pub run_id: String,
    pub created_ts_ns: i64,
    pub horizon_ns: i64,
    pub n: u64,
    pub win_rate: f64,
    pub avg_excess: f64,
    /// Identity fingerprint of the run that produced this grade (REL-4).
    #[serde(default)]
    pub identity: String,
}

impl GradeSnapshot {
    /// Evidence quality gate: a grade with fewer than `min_n` samples cannot
    /// promote anything (SIG-2 — small samples are noise, not edge).
    pub fn has_min_samples(&self, min_n: u64) -> bool {
        self.n >= min_n
    }
}

/// Signal catalog errors (a refused promotion is a valid result, PD-5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignalError {
    #[error("hypothesis must be non-empty (SIG-1)")]
    EmptyHypothesis,
    #[error("kill requires a justification (SIG-4)")]
    MissingJustification,
    #[error("signal is Killed (terminal)")]
    Terminal,
    #[error("must promote exactly one stage up (SIG-2)")]
    NotNextStage,
    #[error("grade batch is too small to promote (needs {0}+ samples)")]
    TooFewSamples(u64),
    #[error("grade batch has no positive edge (avg_excess must be > 0)")]
    NoPositiveEdge,
    #[error("promotion to Deployed requires a human decision")]
    NeedsHuman,
    #[error("last grade is stale — re-test first (SIG-5, > {0}ns)")]
    StaleGrade(i64),
    #[error("demotion target must be a lower stage")]
    NotLower,
    #[error("signal id must be non-empty")]
    EmptyId,
    #[error("signal id already registered (SIG-1)")]
    DuplicateId,
    #[error("grade identity does not match the record's current identity — params, feature version, data schema, or cost model changed since this grade was produced (REL-5); re-run the experiment")]
    IdentityMismatch,
}

/// Default re-test interval: evidence older than 30 days is stale (mirrors
/// `strategies::funnel::EVIDENCE_MAX_AGE_NS`, STR-4).
pub const RE_TEST_INTERVAL_NS: i64 = 30 * 86_400_000_000_000;

/// One catalog entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalRecord {
    pub id: String,
    pub stage: SignalStage,
    /// Why this signal should be an edge, written before any evidence.
    pub hypothesis: String,
    /// `features.toml` params hash of the feature config the signal runs on
    /// (FEA-7) — a params change means the graded history no longer applies.
    pub params_hash: String,
    /// Feature version the graded evidence was produced under (REL-4). A bump
    /// invalidates old grades. Default 1; `#[serde(default)]` for old files.
    #[serde(default = "default_feature_version")]
    pub feature_version: u16,
    /// Data-schema version of the raw corpus the evidence ran on (REL-4).
    /// Default `mp_core::SCHEMA_VER`; `#[serde(default)]` for old files.
    #[serde(default = "default_data_schema_version")]
    pub data_schema_version: u16,
    /// Cost-model hash the evidence assumed (REL-7). Default empty
    /// (cost-free research view). Any non-empty cost model differs.
    #[serde(default)]
    pub cost_model_hash: String,
    pub grades: Vec<GradeSnapshot>,
    /// Weekly mean excess per grading week (the decay re-test series, RES-3).
    pub weekly_avg_excess: Vec<f64>,
    pub last_grade_ts_ns: i64,
    pub re_test_interval_ns: i64,
    /// Whether this signal works under Zero-Cost Mode (no full L2 book).
    /// Default true; set false for signals requiring L2 depth data.
    #[serde(default = "default_zero_cost_compatible")]
    pub zero_cost_compatible: bool,
}

fn default_feature_version() -> u16 {
    1
}
fn default_data_schema_version() -> u16 {
    mp_core::SCHEMA_VER
}

fn default_zero_cost_compatible() -> bool {
    true
}

impl SignalRecord {
    /// Register a new signal at Hypothesis (SIG-1). The hypothesis is the
    /// pre-registration artifact — no empty hunches.
    pub fn register(
        id: impl Into<String>,
        hypothesis: impl Into<String>,
        params_hash: impl Into<String>,
    ) -> Result<Self, SignalError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(SignalError::EmptyId);
        }
        let hypothesis = hypothesis.into();
        if hypothesis.trim().is_empty() {
            return Err(SignalError::EmptyHypothesis);
        }
        Ok(Self {
            id,
            stage: SignalStage::Hypothesis,
            hypothesis,
            params_hash: params_hash.into(),
            feature_version: default_feature_version(),
            data_schema_version: default_data_schema_version(),
            cost_model_hash: String::new(),
            grades: Vec::new(),
            weekly_avg_excess: Vec::new(),
            last_grade_ts_ns: 0,
            re_test_interval_ns: RE_TEST_INTERVAL_NS,
            zero_cost_compatible: true,
        })
    }

    /// Override the identity dimensions (REL-4) — call before grading. A
    /// feature-version bump, a schema migration, or a cost-model change here
    /// invalidates all previously recorded grades (they can no longer match).
    pub fn with_identity(
        mut self,
        feature_version: u16,
        data_schema_version: u16,
        cost_model_hash: impl Into<String>,
    ) -> Self {
        self.feature_version = feature_version;
        self.data_schema_version = data_schema_version;
        self.cost_model_hash = cost_model_hash.into();
        self
    }

    /// The canonical research identity of this record's evidence (REL-4).
    pub fn identity(&self) -> SignalResearchIdentity {
        SignalResearchIdentity {
            signal_id: self.id.clone(),
            feature_version: self.feature_version,
            data_schema_version: self.data_schema_version,
            params_hash: self.params_hash.clone(),
            cost_model_hash: self.cost_model_hash.clone(),
        }
    }

    /// SIG-5: is a re-test due? (last grade older than the interval, or never
    /// graded.)
    pub fn re_test_due(&self, now_ns: i64) -> bool {
        self.last_grade_ts_ns == 0
            || now_ns.saturating_sub(self.last_grade_ts_ns) >= self.re_test_interval_ns
    }

    /// Apply one grading batch and, when evidence supports it, promote one
    /// stage (SIG-2). Promotion rules: batch must meet `min_n` samples, show
    /// positive mean excess, and the previous grade must not be stale.
    /// `Deployed` additionally requires `human` (mirrors G3/G4 — agents can
    /// never pass it).
    pub fn apply_grade(
        &mut self,
        g: GradeSnapshot,
        min_n: u64,
        human: bool,
        now_ns: i64,
    ) -> Result<(), SignalError> {
        if self.stage == SignalStage::Killed {
            return Err(SignalError::Terminal);
        }
        // REL-5: evidence is only valid under the identity that produced it.
        // A grade with a different (or missing — pre-hardening) identity is
        // refused before any promotion logic runs.
        if g.identity != self.identity().fingerprint() {
            return Err(SignalError::IdentityMismatch);
        }
        if !g.has_min_samples(min_n) {
            return Err(SignalError::TooFewSamples(min_n));
        }
        if g.avg_excess <= 0.0 {
            return Err(SignalError::NoPositiveEdge);
        }
        if self.re_test_due(now_ns) && self.last_grade_ts_ns > 0 {
            return Err(SignalError::StaleGrade(self.re_test_interval_ns));
        }
        let next = self.stage.next_up().ok_or(SignalError::Terminal)?;
        if next == SignalStage::Deployed && !human {
            return Err(SignalError::NeedsHuman);
        }
        self.grades.push(g.clone());
        self.weekly_avg_excess.push(g.avg_excess);
        self.last_grade_ts_ns = g.created_ts_ns;
        self.stage = next;
        Ok(())
    }

    /// Read-only decay check for health summaries (no mutation).
    pub fn would_decay(&self) -> bool {
        if self.stage == SignalStage::Killed {
            return false;
        }
        if self.weekly_avg_excess.len() < 12 {
            return false;
        }
        let w12: Vec<f64> = self.weekly_avg_excess.iter().rev().take(12).copied().collect();
        let mean12: f64 = w12.iter().sum::<f64>() / 12.0;
        if mean12 <= 0.0 {
            return false;
        }
        let mean4: f64 = w12[..4].iter().sum::<f64>() / 4.0;
        mean4 < 0.5 * mean12
    }

    /// SIG-3: decay re-test over the weekly-mean series (RES-3 semantics —
    /// trailing 4-week mean below half the trailing 12-week mean, needing
    /// ≥ 12 weeks, only for a positive edge). When decayed: stamps the
    /// transient `Decayed` marker (the caller journals it) and demotes to
    /// `Hypothesis` automatically — risk-off never needs a human.
    pub fn detect_decay(&mut self) -> bool {
        // SIG-4: a killed signal is terminal. Decay re-test must never resurrect
        // it — without this guard, a Killed record that retained ≥ 12 fading
        // weekly points would be demoted back to Hypothesis (regression found in
        // the 0a55460 review; see `sig_4_killed_resists_decay_retest`).
        if self.stage == SignalStage::Killed {
            return false;
        }
        if self.weekly_avg_excess.len() < 12 {
            return false;
        }
        let w12: Vec<f64> = self
            .weekly_avg_excess
            .iter()
            .rev()
            .take(12)
            .copied()
            .collect();
        let mean12: f64 = w12.iter().sum::<f64>() / 12.0;
        if mean12 <= 0.0 {
            return false;
        }
        let mean4: f64 = w12[..4].iter().sum::<f64>() / 4.0;
        if mean4 >= 0.5 * mean12 {
            return false;
        }
        let was = self.stage;
        self.stage = SignalStage::Decayed;
        let _ = self.demote(SignalStage::Hypothesis);
        debug_assert!(was != SignalStage::Killed);
        true
    }

    /// SIG-4: kill a signal. Requires a non-empty justification (the autopsy
    /// artifact, mirroring `strategies::funnel::Autopsy`). Terminal.
    pub fn kill(&mut self, justification: impl Into<String>) -> Result<(), SignalError> {
        if self.stage == SignalStage::Killed {
            return Err(SignalError::Terminal);
        }
        if justification.into().trim().is_empty() {
            return Err(SignalError::MissingJustification);
        }
        self.stage = SignalStage::Killed;
        Ok(())
    }

    /// Demote to a strictly lower stage — automatic risk-off (SIG-3 path).
    /// `Decayed → Hypothesis` is the one special case (the decay marker is
    /// transient, rank-0 like Hypothesis).
    pub fn demote(&mut self, to: SignalStage) -> Result<(), SignalError> {
        if self.stage == SignalStage::Killed || to == SignalStage::Killed {
            return Err(SignalError::Terminal);
        }
        if self.stage == SignalStage::Decayed && to == SignalStage::Hypothesis {
            self.stage = to;
            return Ok(());
        }
        if to.rank() >= self.stage.rank() {
            return Err(SignalError::NotLower);
        }
        self.stage = to;
        Ok(())
    }
}

/// The whole catalog (serde only — the binary edge owns file I/O, PD-3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SignalCatalog {
    pub version: u32,
    pub signals: Vec<SignalRecord>,
}

impl SignalCatalog {
    pub fn new() -> Self {
        Self {
            version: 1,
            signals: Vec::new(),
        }
    }

    pub fn get(&self, id: &str) -> Option<&SignalRecord> {
        self.signals.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut SignalRecord> {
        self.signals.iter_mut().find(|s| s.id == id)
    }

    /// Register a signal; a duplicate id is an error (one catalog entry per
    /// signal).
    pub fn register(
        &mut self,
        id: impl Into<String>,
        hypothesis: impl Into<String>,
        params_hash: impl Into<String>,
    ) -> Result<&mut SignalRecord, SignalError> {
        let id = id.into();
        if self.get(&id).is_some() {
            return Err(SignalError::DuplicateId);
        }
        let rec = SignalRecord::register(id, hypothesis, params_hash)?;
        self.signals.push(rec);
        Ok(self.signals.last_mut().unwrap())
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_784_505_600_000_000_000; // 2026-07-19T00:00:00Z, deterministic

    fn grade(rec: &SignalRecord, run_id: &str, n: u64, avg_excess: f64, ts: i64) -> GradeSnapshot {
        GradeSnapshot {
            run_id: run_id.into(),
            created_ts_ns: ts,
            horizon_ns: 3_600_000_000_000,
            n,
            win_rate: 0.6,
            avg_excess,
            identity: rec.identity().fingerprint(),
        }
    }

    #[test]
    fn sig_1_register_requires_hypothesis() {
        assert!(SignalRecord::register("sig", "  ", "hash").is_err());
        let r = SignalRecord::register("whale_imb", "whale flow predicts reversal", "h1").unwrap();
        assert_eq!(r.stage, SignalStage::Hypothesis);
        assert!(r.grades.is_empty());
    }

    #[test]
    fn sig_2_grade_promotes_only_with_evidence() {
        let mut r = SignalRecord::register("sig", "hyp", "h").unwrap();
        // Too few samples: refused (small samples are noise, not edge).
        assert!(r
            .apply_grade(grade(&r, "r1", 5, 0.001, NOW), 30, false, NOW)
            .is_err());
        assert_eq!(r.stage, SignalStage::Hypothesis);
        // No positive edge: refused.
        assert!(r
            .apply_grade(grade(&r, "r2", 30, -0.001, NOW), 30, false, NOW)
            .is_err());
        // Good batch: Hypothesis → Tested.
        r.apply_grade(grade(&r, "r3", 30, 0.002, NOW), 30, false, NOW)
            .unwrap();
        assert_eq!(r.stage, SignalStage::Tested);
        // Tested → Graded.
        r.apply_grade(grade(&r, "r4", 40, 0.002, NOW), 30, false, NOW)
            .unwrap();
        assert_eq!(r.stage, SignalStage::Graded);
        // Graded → Deployed needs a human (agents can never pass it).
        assert!(r
            .apply_grade(grade(&r, "r5", 50, 0.002, NOW), 30, false, NOW)
            .is_err());
        r.apply_grade(grade(&r, "r6", 50, 0.002, NOW), 30, true, NOW)
            .unwrap();
        assert_eq!(r.stage, SignalStage::Deployed);
        // Deployed has no next stage.
        assert!(r
            .apply_grade(grade(&r, "r7", 50, 0.002, NOW), 30, true, NOW)
            .is_err());
    }

    #[test]
    fn sig_5_stale_grade_refuses_promotion_until_retest() {
        let mut r = SignalRecord::register("sig", "hyp", "h").unwrap();
        r.apply_grade(grade(&r, "r1", 30, 0.002, NOW), 30, false, NOW)
            .unwrap();
        // A grade 40 days later is stale (interval = 30d): refused until re-test.
        let late = NOW + 40 * 86_400_000_000_000;
        assert!(matches!(
            r.apply_grade(grade(&r, "r2", 30, 0.002, late), 30, false, late),
            Err(SignalError::StaleGrade(_))
        ));
        // A re-test within the interval is accepted.
        let on_time = NOW + 10 * 86_400_000_000_000;
        assert!(r
            .apply_grade(grade(&r, "r2", 30, 0.002, on_time), 30, false, on_time)
            .is_ok());
        assert!(!r.re_test_due(on_time));
    }

    #[test]
    fn rel_5_identity_change_invalidates_old_evidence() {
        // REL-5: evidence produced under one identity must be refused after
        // ANY identity dimension changes (params / feature version / data
        // schema / cost model).
        // Helper: a grade carrying the OLD identity fingerprint (evidence
        // produced before a mutation) — this is what invalidation must refuse.
        let old_grade = |rec: &SignalRecord, old_fp: String| GradeSnapshot {
            identity: old_fp,
            ..grade(rec, "r_old", 40, 0.002, NOW + 1000)
        };

        // 1) params change: evidence under the old fingerprint is refused.
        let mut r = SignalRecord::register("sig", "hyp", "h").unwrap();
        r.apply_grade(grade(&r, "r1", 30, 0.002, NOW), 30, false, NOW)
            .unwrap();
        assert_eq!(r.stage, SignalStage::Tested);
        let fp_before = r.identity().fingerprint();
        r.params_hash = "h2".into();
        assert_ne!(r.identity().fingerprint(), fp_before);
        assert!(matches!(
            r.apply_grade(old_grade(&r, fp_before.clone()), 30, false, NOW + 1000),
            Err(SignalError::IdentityMismatch)
        ));

        // 2) feature-version bump invalidates too.
        let mut r2 = SignalRecord::register("sig2", "hyp", "h").unwrap();
        r2.apply_grade(grade(&r2, "r1", 30, 0.002, NOW), 30, false, NOW)
            .unwrap();
        let fp2 = r2.identity().fingerprint();
        r2.feature_version = 2;
        assert!(matches!(
            r2.apply_grade(old_grade(&r2, fp2), 30, false, NOW + 1000),
            Err(SignalError::IdentityMismatch)
        ));

        // 3) cost-model change invalidates (REL-7).
        let mut r3 = SignalRecord::register("sig3", "hyp", "h").unwrap();
        r3.apply_grade(grade(&r3, "r1", 30, 0.002, NOW), 30, false, NOW)
            .unwrap();
        let fp3 = r3.identity().fingerprint();
        r3.cost_model_hash = crate::signal_identity::cost_model_hash(0.00055, 0.0002, 0.0001);
        assert!(matches!(
            r3.apply_grade(old_grade(&r3, fp3), 30, false, NOW + 1000),
            Err(SignalError::IdentityMismatch)
        ));

        // 4) a CURRENT-identity grade still promotes (sanity).
        let mut r4 = SignalRecord::register("sig4", "hyp", "h").unwrap();
        r4.apply_grade(grade(&r4, "r1", 30, 0.002, NOW), 30, false, NOW)
            .unwrap();
        assert_eq!(r4.stage, SignalStage::Tested);
    }

    #[test]
    fn rel_5_pre_hardening_grades_without_identity_are_refused() {
        // A legacy catalog file's grades decode with identity="" — they must
        // be refused loudly, not silently accepted under the new identity.
        let mut r = SignalRecord::register("sig", "hyp", "h").unwrap();
        let legacy = GradeSnapshot {
            run_id: "legacy".into(),
            created_ts_ns: NOW,
            horizon_ns: 3_600_000_000_000,
            n: 50,
            win_rate: 0.6,
            avg_excess: 0.01,
            identity: String::new(), // absent in old files
        };
        assert!(matches!(
            r.apply_grade(legacy, 30, false, NOW),
            Err(SignalError::IdentityMismatch)
        ));
    }

    #[test]
    fn sig_3_decay_detection_demotes_automatically() {
        let mut r = SignalRecord::register("sig", "hyp", "h").unwrap();
        // 12 weeks of strong edge, then a 4-week fade below half the trailing
        // 12-week mean (RES-3): decayed ⇒ automatic demotion to Hypothesis.
        for _ in 0..8 {
            r.weekly_avg_excess.push(0.002);
        }
        for _ in 0..4 {
            r.weekly_avg_excess.push(0.0005);
        }
        assert!(r.detect_decay());
        assert_eq!(r.stage, SignalStage::Hypothesis);
        // Not enough history: never flags.
        let mut young = SignalRecord::register("young", "hyp", "h").unwrap();
        for _ in 0..11 {
            young.weekly_avg_excess.push(0.0001);
        }
        assert!(!young.detect_decay());
        // A faded-but-never-positive edge is not "decay" (it was never good).
        let mut weak = SignalRecord::register("weak", "hyp", "h").unwrap();
        for _ in 0..12 {
            weak.weekly_avg_excess.push(-0.0001);
        }
        assert!(!weak.detect_decay());
    }

    #[test]
    fn sig_4_kill_is_terminal_and_needs_justification() {
        let mut r = SignalRecord::register("sig", "hyp", "h").unwrap();
        assert!(r.kill("  ").is_err());
        r.kill("regression to mean baseline; no edge after 90 days")
            .unwrap();
        assert_eq!(r.stage, SignalStage::Killed);
        assert!(r
            .apply_grade(grade(&r, "r", 30, 0.002, NOW), 30, true, NOW)
            .is_err());
        assert!(r.kill("again").is_err());
    }

    #[test]
    fn sig_4_killed_resists_decay_retest() {
        // Regression (0a55460 review): detect_decay had no Killed guard, so a
        // killed signal that retained ≥ 12 fading weekly points would be
        // silently resurrected to Hypothesis — breaking SIG-4's terminal
        // guarantee. Build exactly that history, kill, then re-test.
        let mut r = SignalRecord::register("sig", "hyp", "h").unwrap();
        // 8 strong weeks then 4 fading weeks (the decay signature from sig_3).
        for _ in 0..8 {
            r.weekly_avg_excess.push(0.002);
        }
        for _ in 0..4 {
            r.weekly_avg_excess.push(0.0005);
        }
        r.kill("edge gone; autopsy on file").unwrap();
        assert_eq!(r.stage, SignalStage::Killed);
        // The re-test must NOT flag decay or move the stage off Killed.
        assert!(!r.detect_decay());
        assert_eq!(r.stage, SignalStage::Killed);
    }

    #[test]
    fn sig_1_catalog_serde_roundtrip_and_duplicate_refusal() {
        let mut c = SignalCatalog::new();
        c.register("a", "hyp a", "h1").unwrap();
        assert!(c.register("a", "dup", "h1").is_err());
        let json = c.to_json().unwrap();
        let back = SignalCatalog::from_json(&json).unwrap();
        assert_eq!(c, back);
        assert_eq!(back.get("a").unwrap().hypothesis, "hyp a");
    }
}
