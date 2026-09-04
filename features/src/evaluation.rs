//! Evaluation & promotion (research-lab hardening Phase 5, REL-15..REL-19).
//!
//! Turns recorded observations with attached forward outcomes into a
//! structured promotion decision with EXPLICIT reject reasons (PD-5: a
//! refused promotion is a valid, valuable result). Builds on the existing
//! catalog lifecycle (`SignalRecord::apply_grade`) rather than replacing it:
//! this module produces the evidence (`GradeSnapshot` via
//! [`grade_from_report`]) and the catalog still enforces stages, human clicks,
//! staleness and — since the hardening — identity.
//!
//! Pure: no I/O, no wall clock (PD-3). All math is deterministic.

use crate::observation::SignalObservation;

/// Default minimum sample size for a promotion-grade evaluation (REL-15).
pub const DEFAULT_MIN_N: u64 = 30;

/// Structured evaluation report (REL-16/REL-17).
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationReport {
    /// Horizon the outcomes were measured at.
    pub horizon_ns: i64,
    /// Observations with a closed outcome at this horizon.
    pub n: u64,
    /// Sample-size gate (REL-15).
    pub min_n: u64,
    /// Whether the evidence passes ALL gates (sample size, finite, positive
    /// net expectancy).
    pub gate_passed: bool,
    /// Mean gross (pre-cost) return across the sample.
    pub gross_expectancy: f64,
    /// Mean net (post-cost) return across the sample — the honest number.
    pub net_expectancy: f64,
    /// Fraction of outcomes with `gross > 0`.
    pub win_rate: f64,
    /// Distribution of NET returns (REL-17): 25th / 50th / 75th percentiles.
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    /// Explicit reasons a promotion was refused; empty when `gate_passed`.
    pub reject_reasons: Vec<String>,
}

/// Structured promotion decision (REL-18): `promote` + the reasons, so a
/// refusal is an artifact, not a bare error.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionDecision {
    pub promote: bool,
    pub reasons: Vec<String>,
}

impl PromotionDecision {
    pub fn pass(reason: impl Into<String>) -> Self {
        Self {
            promote: true,
            reasons: vec![reason.into()],
        }
    }
    pub fn refuse(reasons: Vec<String>) -> Self {
        Self {
            promote: false,
            reasons,
        }
    }
}

/// Evaluate observations with attached outcomes at one horizon (REL-15..17).
/// Observations without a closed outcome at `horizon_ns` are excluded from
/// the sample (an open window is never imputed).
pub fn evaluate(
    observations: &[SignalObservation],
    horizon_ns: i64,
    min_n: u64,
) -> EvaluationReport {
    let mut rejects = Vec::new();
    let net: Vec<f64> = observations
        .iter()
        .filter_map(|o| {
            o.outcomes
                .iter()
                .find(|oc| oc.horizon_ns == horizon_ns)
                .map(|oc| oc.net_return)
        })
        .collect();
    let n = net.len() as u64;
    if n < min_n {
        return EvaluationReport {
            horizon_ns,
            n,
            min_n,
            gate_passed: false,
            gross_expectancy: 0.0,
            net_expectancy: 0.0,
            win_rate: 0.0,
            p25: 0.0,
            p50: 0.0,
            p75: 0.0,
            reject_reasons: vec![format!(
                "insufficient closed outcomes: {n} < required {min_n} (REL-15 sample-size gate)"
            )],
        };
    }
    let gross: Vec<f64> = observations
        .iter()
        .filter_map(|o| {
            o.outcomes
                .iter()
                .find(|oc| oc.horizon_ns == horizon_ns)
                .map(|oc| oc.gross_return)
        })
        .collect();
    let gross_expectancy = gross.iter().sum::<f64>() / gross.len() as f64;
    let net_expectancy = net.iter().sum::<f64>() / net.len() as f64;
    if !gross_expectancy.is_finite() || !net_expectancy.is_finite() {
        rejects.push("non-finite expectancy — data integrity failure (REL-1)".into());
    }
    if net_expectancy <= 0.0 {
        rejects.push(format!(
            "no positive net edge after costs: net expectancy {net_expectancy:.6} ≤ 0 (REL-16)"
        ));
    }
    let hits = observations
        .iter()
        .filter(|o| {
            o.outcomes
                .iter()
                .any(|oc| oc.horizon_ns == horizon_ns && oc.hit)
        })
        .count();
    let win_rate = hits as f64 / n as f64;
    let (p25, p50, p75) = percentiles(&net);
    EvaluationReport {
        horizon_ns,
        n,
        min_n,
        gate_passed: rejects.is_empty(),
        gross_expectancy,
        net_expectancy,
        win_rate,
        p25,
        p50,
        p75,
        reject_reasons: rejects,
    }
}

/// The promotion decision for a report (REL-18): promote iff the gate passed.
pub fn decide(report: &EvaluationReport) -> PromotionDecision {
    if report.gate_passed {
        PromotionDecision::pass(format!(
            "gate passed at horizon {}s: n={}, net_expectancy={:.6}, win_rate={:.3}",
            report.horizon_ns / 1_000_000_000,
            report.n,
            report.net_expectancy,
            report.win_rate
        ))
    } else {
        PromotionDecision::refuse(report.reject_reasons.clone())
    }
}

/// Build a catalog `GradeSnapshot` from a passed report (REL-19) — the bridge
/// between the research evaluation and the existing signal-catalog lifecycle.
/// Refuses to build from a failed gate (a `None`, PD-5). `identity_fingerprint`
/// must be the signal record's CURRENT identity fingerprint (REL-5).
pub fn grade_from_report(
    identity_fingerprint: &str,
    run_id: impl Into<String>,
    created_ts_ns: i64,
    report: &EvaluationReport,
) -> Option<crate::signal_catalog::GradeSnapshot> {
    if !report.gate_passed {
        return None;
    }
    Some(crate::signal_catalog::GradeSnapshot {
        run_id: run_id.into(),
        created_ts_ns,
        horizon_ns: report.horizon_ns,
        n: report.n,
        win_rate: report.win_rate,
        avg_excess: report.net_expectancy,
        identity: identity_fingerprint.to_string(),
    })
}

/// Nearest-rank percentiles of a slice (deterministic, sorted copy).
fn percentiles(vals: &[f64]) -> (f64, f64, f64) {
    if vals.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut s = vals.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let pct = |q: f64| -> f64 {
        let rank = ((q * s.len() as f64).ceil() as usize).clamp(1, s.len());
        s[rank - 1]
    };
    (pct(0.25), pct(0.50), pct(0.75))
}

/// Distribution summary used by reports (kept public for tests/consumers).
pub fn distribution(vals: &[f64]) -> (f64, f64, f64) {
    percentiles(vals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{Direction, ObservationOutcome, SignalObservation};
    use crate::signal_identity::SignalResearchIdentity;
    use mp_core::{SymbolId, Venue};

    fn obs(net_returns: &[f64], horizon: i64) -> Vec<SignalObservation> {
        net_returns
            .iter()
            .enumerate()
            .map(|(i, &nr)| SignalObservation {
                observation_id: i as u64,
                identity: SignalResearchIdentity::new("sig", 1, "p", "c"),
                timestamp_ns: i as i64 * 1_000_000_000,
                symbol: SymbolId(1),
                venue: Venue::Bybit,
                direction: Direction::Long,
                feature_snapshot: Default::default(),
                quality: crate::data_quality::DataQualityState::Healthy,
                created_at_ns: 0,
                outcomes: vec![ObservationOutcome {
                    horizon_ns: horizon,
                    entry_price: 100.0,
                    exit_price: 100.0 * (1.0 + nr + 0.002), // gross = net + cost
                    gross_return: nr + 0.002,
                    net_return: nr,
                    mfe: 0.0,
                    mae: 0.0,
                    hit: nr + 0.002 > 0.0,
                }],
            })
            .collect()
    }

    #[test]
    fn rel_15_sample_size_gate_refuses_with_reason() {
        let report = evaluate(&obs(&[0.001; 10], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(!report.gate_passed);
        assert_eq!(report.n, 10);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.contains("insufficient closed outcomes")));
        assert!(!decide(&report).promote);
        // A matching-size sample passes the sample gate.
        let report = evaluate(&obs(&[0.001; 30], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert_eq!(report.n, 30);
        assert!(report.gate_passed);
    }

    #[test]
    fn rel_16_gross_vs_net_expectancy_and_win_rate() {
        // 30 outcomes; net returns use EXACT binary fractions (0.125 / −0.25)
        // so the mean is exactly 0.0: gross = net + 0.002 cost ⇒ gross exp
        // ≈ +0.002 > 0 but NET is exactly 0 — no positive net edge.
        let mut nr = vec![0.125; 20];
        nr.extend(vec![-0.25; 10]);
        let report = evaluate(&obs(&nr, 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!((report.gross_expectancy - 0.002).abs() < 1e-9);
        assert_eq!(report.net_expectancy, 0.0);
        // Gross > 0 but net ≤ 0 ⇒ refused with the net-edge reason.
        assert!(!report.gate_passed);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.contains("no positive net edge")));
        assert!((report.win_rate - 20.0 / 30.0).abs() < 1e-12);
    }

    #[test]
    fn rel_17_distribution_percentiles() {
        // Nearest-rank convention (rank = ceil(q·N)) on 0..=9 (N=10):
        // p25 → rank 3 → idx 2 = 2; p50 → rank 5 → idx 4 = 4; p75 → rank 8 → idx 7 = 7.
        let vals: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let (p25, p50, p75) = distribution(&vals);
        assert_eq!((p25, p50, p75), (2.0, 4.0, 7.0));
        // Empty input never panics.
        assert_eq!(distribution(&[]), (0.0, 0.0, 0.0));
        // N=1 ⇒ every percentile is the single value.
        assert_eq!(distribution(&[3.5]), (3.5, 3.5, 3.5));
    }

    #[test]
    fn rel_18_promotion_decision_is_structured() {
        let good = evaluate(&obs(&[0.005; 30], 3_600_000_000_000), 3_600_000_000_000, 30);
        let d = decide(&good);
        assert!(d.promote);
        assert!(d.reasons.iter().any(|r| r.contains("gate passed")));
        let bad = evaluate(&obs(&[-0.01; 30], 3_600_000_000_000), 3_600_000_000_000, 30);
        let d = decide(&bad);
        assert!(!d.promote);
        assert!(!d.reasons.is_empty());
    }

    #[test]
    fn rel_19_grade_from_passed_report_only() {
        let good = evaluate(&obs(&[0.005; 30], 3_600_000_000_000), 3_600_000_000_000, 30);
        let g = grade_from_report("fp", "run-1", 0, &good).expect("passed ⇒ grade");
        assert_eq!(g.n, 30);
        assert_eq!(g.identity, "fp");
        assert_eq!(g.avg_excess, good.net_expectancy);
        let bad = evaluate(&obs(&[-0.01; 30], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(grade_from_report("fp", "run-2", 0, &bad).is_none());
    }

    #[test]
    fn rel_15_open_windows_excluded_not_imputed() {
        // 40 observations but only 10 have the matching horizon ⇒ n=10 < 30.
        let mut os = obs(&[0.001; 10], 3_600_000_000_000);
        os.extend(obs(&[0.001; 30], 86_400_000_000_000)); // different horizon
        let report = evaluate(&os, 3_600_000_000_000, 30);
        assert_eq!(report.n, 10);
        assert!(!report.gate_passed);
    }
}