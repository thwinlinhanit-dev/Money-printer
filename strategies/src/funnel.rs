//! The promotion funnel (STR-3/5/6). Deliberately slow. The whole safety
//! philosophy in one asymmetry: **demotion is automatic and instant; promotion
//! always requires a human click** (agents prepare evidence, never click).

use mp_core::StrategyId;
use serde::{Deserialize, Serialize};

/// Funnel stages, ordered by rank (Idea lowest, LiveScaled highest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    Idea,
    Hypothesis,
    Backtest,
    WalkForward,
    Paper,
    LiveSmall,
    LiveScaled,
    Killed,
}

impl Stage {
    fn rank(self) -> i32 {
        match self {
            Stage::Idea => 0,
            Stage::Hypothesis => 1,
            Stage::Backtest => 2,
            Stage::WalkForward => 3,
            Stage::Paper => 4,
            Stage::LiveSmall => 5,
            Stage::LiveScaled => 6,
            Stage::Killed => -1,
        }
    }

    /// The single next stage up, if any.
    fn next_up(self) -> Option<Stage> {
        Some(match self {
            Stage::Idea => Stage::Hypothesis,
            Stage::Hypothesis => Stage::Backtest,
            Stage::Backtest => Stage::WalkForward,
            Stage::WalkForward => Stage::Paper,
            Stage::Paper => Stage::LiveSmall,
            Stage::LiveSmall => Stage::LiveScaled,
            Stage::LiveScaled | Stage::Killed => return None,
        })
    }

    /// Gates that require a human click (G3, G4).
    fn requires_human(self, to: Stage) -> bool {
        matches!(
            (self, to),
            (Stage::Paper, Stage::LiveSmall) | (Stage::LiveSmall, Stage::LiveScaled)
        )
    }

    /// Gates that require experiment evidence (G1..G4).
    fn requires_evidence(self, to: Stage) -> bool {
        matches!(
            (self, to),
            (Stage::Backtest, Stage::WalkForward)
                | (Stage::WalkForward, Stage::Paper)
                | (Stage::Paper, Stage::LiveSmall)
                | (Stage::LiveSmall, Stage::LiveScaled)
        )
    }
}

/// Who/what performed a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Actor {
    Human,
    Auto,
}

/// A journaled stage transition (STR-5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: StrategyId,
    pub from: Stage,
    pub to: Stage,
    pub reason: String,
    pub evidence: Vec<String>,
    pub actor: Actor,
}

impl Transition {
    /// One JSONL journal line (`journal/funnel.log`).
    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).expect("transition serializes")
    }
}

/// Funnel errors — a refused promotion is a valid, valuable result (PD-5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FunnelError {
    #[error("must promote exactly one stage up")]
    NotNextStage,
    #[error("this gate requires a human click")]
    NeedsHuman,
    #[error("hypothesis.md must be complete before leaving Idea")]
    MissingHypothesis,
    #[error("this gate requires experiment evidence")]
    MissingEvidence,
    #[error("strategy is Killed (terminal)")]
    Terminal,
    #[error("kill requires an autopsy")]
    MissingAutopsy,
    #[error("demotion target must be a lower stage")]
    NotLower,
    #[error("evidence is stale (> 30 days old) — re-run the experiment (STR-4)")]
    StaleEvidence,
    #[error("autopsy must state what we believed, what the data said, and the lesson (STR-6)")]
    IncompleteAutopsy,
}

/// A gate-evidence reference (STR-4): the experiment-tracker run id (SIM-10)
/// plus when the run was produced, so staleness is checkable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub run_id: String,
    pub created_ts_ns: i64,
}

/// STR-4: evidence older than this is stale and refused (30 days).
pub const EVIDENCE_MAX_AGE_NS: i64 = 30 * 86_400_000_000_000;

/// The kill artifact (STR-6): what we believed, what the data said, the
/// lesson. Rendered to `strategies/{id}/AUTOPSY.md` — the kill log is a
/// product (W-6). All three fields must be non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Autopsy {
    pub believed: String,
    pub data_said: String,
    pub lesson: String,
}

impl Autopsy {
    pub fn is_complete(&self) -> bool {
        !self.believed.trim().is_empty()
            && !self.data_said.trim().is_empty()
            && !self.lesson.trim().is_empty()
    }

    /// Render `AUTOPSY.md` (STR-6).
    pub fn to_markdown(&self, id: &StrategyId) -> String {
        format!(
            "# AUTOPSY — {}\n\n## What we believed\n{}\n\n## What the data said\n{}\n\n## Lesson\n{}\n",
            id.0, self.believed, self.data_said, self.lesson
        )
    }
}

/// Persisted funnel state for one strategy (`strategies/{id}/funnel.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunnelState {
    pub id: StrategyId,
    pub stage: Stage,
    pub hypothesis_complete: bool,
    pub evidence: Vec<String>,
}

impl FunnelState {
    /// Register a new strategy at Idea (STR-2: hypothesis is checked at the
    /// Idea→Hypothesis promotion, not silently assumed).
    pub fn register(id: StrategyId, hypothesis_complete: bool) -> Self {
        Self {
            id,
            stage: Stage::Idea,
            hypothesis_complete,
            evidence: Vec::new(),
        }
    }

    /// Promote one stage (STR-3). G3/G4 require `human`; all evidence gates
    /// require non-empty `evidence`. Agents may never pass `human = true`.
    pub fn promote(
        &mut self,
        to: Stage,
        human: bool,
        evidence: Vec<EvidenceRef>,
        now_ns: i64,
    ) -> Result<Transition, FunnelError> {
        if self.stage == Stage::Killed {
            return Err(FunnelError::Terminal);
        }
        if self.stage.next_up() != Some(to) {
            return Err(FunnelError::NotNextStage);
        }
        if to == Stage::Hypothesis && !self.hypothesis_complete {
            return Err(FunnelError::MissingHypothesis);
        }
        if self.stage.requires_human(to) && !human {
            return Err(FunnelError::NeedsHuman);
        }
        if self.stage.requires_evidence(to) {
            if evidence.is_empty() {
                return Err(FunnelError::MissingEvidence);
            }
            // STR-4: run-id evidence must be fresh — older than 30 days is
            // stale and the promotion is refused (re-run the experiment).
            if evidence
                .iter()
                .any(|e| now_ns.saturating_sub(e.created_ts_ns) > EVIDENCE_MAX_AGE_NS)
            {
                return Err(FunnelError::StaleEvidence);
            }
        }
        let from = self.stage;
        self.stage = to;
        let run_ids: Vec<String> = evidence.iter().map(|e| e.run_id.clone()).collect();
        self.evidence.extend(run_ids.iter().cloned());
        Ok(Transition {
            id: self.id.clone(),
            from,
            to,
            reason: "promote".into(),
            evidence: run_ids,
            actor: if human { Actor::Human } else { Actor::Auto },
        })
    }

    /// Demote to a lower stage (STR — automatic, no human needed). Risk-off is
    /// always allowed.
    pub fn demote(
        &mut self,
        to: Stage,
        reason: impl Into<String>,
    ) -> Result<Transition, FunnelError> {
        if self.stage == Stage::Killed {
            return Err(FunnelError::Terminal);
        }
        if to.rank() >= self.stage.rank() {
            return Err(FunnelError::NotLower);
        }
        let from = self.stage;
        self.stage = to;
        Ok(Transition {
            id: self.id.clone(),
            from,
            to,
            reason: reason.into(),
            evidence: Vec::new(),
            actor: Actor::Auto,
        })
    }

    /// Kill a strategy (STR-6). Requires a COMPLETE autopsy (believed /
    /// data said / lesson) — the artifact, not a checkbox; terminal.
    pub fn kill(
        &mut self,
        autopsy: &Autopsy,
        reason: impl Into<String>,
    ) -> Result<Transition, FunnelError> {
        if self.stage == Stage::Killed {
            return Err(FunnelError::Terminal);
        }
        if !autopsy.is_complete() {
            return Err(FunnelError::IncompleteAutopsy);
        }
        let from = self.stage;
        self.stage = Stage::Killed;
        Ok(Transition {
            id: self.id.clone(),
            from,
            to: Stage::Killed,
            reason: reason.into(),
            evidence: Vec::new(),
            actor: Actor::Auto,
        })
    }
}
