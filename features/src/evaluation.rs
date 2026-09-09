//! Evaluation & promotion (research-lab hardening Phase 5, REL-15..REL-19,
//! and Phase 7 anti-randomness gates, REL-24..REL-29 — 2026-09-07).
//!
//! Turns recorded observations with attached forward outcomes into a
//! structured promotion decision with EXPLICIT, machine-readable reject
//! reasons (PD-5: a refused promotion is a valid, valuable result). Builds
//! on the existing catalog lifecycle (`SignalRecord::apply_grade`) rather
//! than replacing it: this module produces the evidence (`GradeSnapshot` via
//! [`grade_from_report`]) and the catalog still enforces stages, human
//! clicks, staleness and — since the hardening — identity.
//!
//! Philosophy (R-8): most ideas must die. Every gate below is kill-biased —
//! absent evidence REFUSES, it never silently passes:
//! - R-4/REL-16: promotion requires positive NET (post-cost) expectancy;
//! - R-5/REL-24: sample-size TIERS — promotion requires the Research tier;
//! - R-6/REL-25: regime tagging — a signal that only works in one regime is
//!   refused (`ONLY_WORKS_IN_*`) or flagged unproven
//!   (`REGIME_COVERAGE_SINGLE_*` / `REGIME_SAMPLE_TOO_SMALL_*`);
//! - R-7/REL-26: sustained decay — two consecutive recent windows with
//!   non-positive net expectancy refuse promotion (`DECAY_SUSPECT`).
//!
//! Pure: no I/O, no wall clock (PD-3). All math is deterministic.

use crate::observation::SignalObservation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default minimum sample size for a promotion-grade evaluation (REL-15).
pub const DEFAULT_MIN_N: u64 = 30;

/// Minimum sample size for the Research tier (REL-24/R-5): promotion
/// requires at least this many closed outcomes.
pub const RESEARCH_MIN_N: u64 = 100;

/// Default number of equal chronological windows for decay detection
/// (REL-26/R-7).
pub const DEFAULT_DECAY_WINDOWS: usize = 3;

/// Minimum closed outcomes per decay window for decay to be judged at all
/// (REL-26: tiny windows prove nothing in either direction).
pub const DECAY_MIN_PER_WINDOW: u64 = 5;

/// The fire-snapshot feature consulted for regime tagging (REL-25/R-6).
/// This is the existing `TrendRegime` feature family: 0.0 = TREND,
/// 1.0 = CHOP (spec 045 §Sub-Signal Definitions).
pub const DEFAULT_REGIME_FEATURE: &str = "regime.trend";

/// Minimum tagged observations per regime before the regime's own stats are
/// trusted for the `ONLY_WORKS_IN_*` refusal (REL-25).
pub const REGIME_MIN_TAGGED: u64 = 10;

/// Sample-size tier (REL-24/R-5): the mandatory vocabulary for how much
/// evidence an evaluation rests on. `Research` is the promotion floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SampleTier {
    /// n < min_n — no evaluation is possible (REL-15).
    Insufficient,
    /// min_n ≤ n < RESEARCH_MIN_N — indicative only, never promotable.
    Preliminary,
    /// n ≥ RESEARCH_MIN_N — the only tier that may promote.
    Research,
}

impl SampleTier {
    /// Classify a sample size against the two thresholds.
    pub fn classify(n: u64, min_n: u64) -> SampleTier {
        if n < min_n {
            SampleTier::Insufficient
        } else if n < RESEARCH_MIN_N {
            SampleTier::Preliminary
        } else {
            SampleTier::Research
        }
    }

    /// Machine-readable label (REL-29).
    pub fn label(self) -> &'static str {
        match self {
            SampleTier::Insufficient => "INSUFFICIENT",
            SampleTier::Preliminary => "PRELIMINARY",
            SampleTier::Research => "RESEARCH",
        }
    }
}

/// One regime bucket of the tagged sample (REL-25/R-6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegimeBucket {
    /// Regime label (e.g. `TREND`, `CHOP`, or the sanitized feature value).
    pub regime: String,
    /// Tagged observations with a closed outcome at the horizon.
    pub n: u64,
    /// Mean net return within the regime.
    pub net_expectancy: f64,
    /// Fraction of the regime's outcomes with `gross > 0`.
    pub win_rate: f64,
}

/// Per-horizon rollup (`per_horizon`, Phase 7 2026-09-07).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HorizonSummary {
    pub horizon_ns: i64,
    pub n: u64,
    pub net_expectancy: f64,
    pub win_rate: f64,
}

/// Structured evaluation report (REL-16/REL-17 + Phase 7 additions).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationReport {
    /// Horizon the outcomes were measured at.
    pub horizon_ns: i64,
    /// Observations with a closed outcome at this horizon.
    pub n: u64,
    /// Sample-size gate (REL-15).
    pub min_n: u64,
    /// Sample-size tier (REL-24/R-5).
    pub tier: SampleTier,
    /// Whether the evidence passes ALL gates (sample tier, finite, positive
    /// net expectancy, decay, regime).
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
    /// Regime buckets over the tagged sample (REL-25/R-6).
    pub regimes: Vec<RegimeBucket>,
    /// Rollup for every horizon with at least one closed outcome.
    pub per_horizon: Vec<HorizonSummary>,
    /// Explicit reasons a promotion was refused; empty when `gate_passed`.
    pub reject_reasons: Vec<String>,
}

/// Structured promotion decision (REL-18): `promote` + the reasons, so a
/// refusal is an artifact, not a bare error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// `PROMOTE` / `REJECT` label (REL-29 machine-readable form).
    pub fn decision_label(&self) -> &'static str {
        if self.promote {
            "PROMOTE"
        } else {
            "REJECT"
        }
    }

    /// The task's structured decision artifact as JSON (REL-29):
    /// `{"decision":"REJECT","reasons":["INSUFFICIENT_SAMPLE",…]}`.
    pub fn to_json(&self) -> String {
        #[derive(Serialize)]
        struct DecisionJson<'a> {
            decision: &'a str,
            reasons: &'a [String],
        }
        serde_json::to_string(&DecisionJson {
            decision: self.decision_label(),
            reasons: &self.reasons,
        })
        .unwrap_or_else(|_| format!("{{\"decision\":\"{}\"}}", self.decision_label()))
    }
}

/// Evaluate observations with attached outcomes at one horizon
/// (REL-15..17 + Phase 7 gates). Observations without a closed outcome at
/// `horizon_ns` are excluded from the sample (an open window is never
/// imputed). Uses the defaults: 3 decay windows, `regime.trend` tagging.
pub fn evaluate(
    observations: &[SignalObservation],
    horizon_ns: i64,
    min_n: u64,
) -> EvaluationReport {
    evaluate_full(
        observations,
        horizon_ns,
        min_n,
        DEFAULT_DECAY_WINDOWS,
        DEFAULT_REGIME_FEATURE,
    )
}

/// Full-control variant (Phase 7): explicit decay-window count and regime
/// feature. Pass `regime_feature: ""` to disable regime tagging.
pub fn evaluate_full(
    observations: &[SignalObservation],
    horizon_ns: i64,
    min_n: u64,
    decay_windows: usize,
    regime_feature: &str,
) -> EvaluationReport {
    let mut rejects = Vec::new();
    let closed: Vec<&SignalObservation> = observations
        .iter()
        .filter(|o| {
            o.outcomes
                .iter()
                .any(|oc| oc.horizon_ns == horizon_ns)
        })
        .collect();
    let n = closed.len() as u64;
    let tier = SampleTier::classify(n, min_n);

    // REL-24/R-5: tier gates. Below min_n is the REL-15 refusal (same
    // machine code the task mandates); the Preliminary band is its own
    // explicit refusal — indicative evidence never promotes.
    if tier == SampleTier::Insufficient {
        rejects.push(format!(
            "INSUFFICIENT_SAMPLE: {n} closed outcomes < required {min_n} (REL-15 sample-size gate)"
        ));
    } else if tier == SampleTier::Preliminary {
        rejects.push(format!(
            "SAMPLE_TIER_PRELIMINARY: n={n} is below the Research floor ({RESEARCH_MIN_N}) — indicative only, never promotable (REL-24)"
        ));
    }

    // Below min_n there is nothing honest to compute — return early.
    if tier == SampleTier::Insufficient {
        return EvaluationReport {
            horizon_ns,
            n,
            min_n,
            tier,
            gate_passed: false,
            gross_expectancy: 0.0,
            net_expectancy: 0.0,
            win_rate: 0.0,
            p25: 0.0,
            p50: 0.0,
            p75: 0.0,
            regimes: Vec::new(),
            per_horizon: Vec::new(),
            reject_reasons: rejects,
        };
    }

    let gross: Vec<f64> = closed
        .iter()
        .filter_map(|o| {
            o.outcomes
                .iter()
                .find(|oc| oc.horizon_ns == horizon_ns)
                .map(|oc| oc.gross_return)
        })
        .collect();
    let net: Vec<f64> = closed
        .iter()
        .filter_map(|o| {
            o.outcomes
                .iter()
                .find(|oc| oc.horizon_ns == horizon_ns)
                .map(|oc| oc.net_return)
        })
        .collect();
    let gross_expectancy = gross.iter().sum::<f64>() / gross.len() as f64;
    let net_expectancy = net.iter().sum::<f64>() / net.len() as f64;
    if !gross_expectancy.is_finite() || !net_expectancy.is_finite() {
        rejects.push("NON_FINITE: non-finite expectancy — data integrity failure (REL-1)".into());
    } else if net_expectancy <= 0.0 {
        rejects.push(format!(
            "NET_EXPECTANCY_NEGATIVE: no positive net edge after costs — net expectancy {net_expectancy:.6} ≤ 0 (REL-16)"
        ));
    }
    let hits = closed
        .iter()
        .filter(|o| {
            o.outcomes
                .iter()
                .any(|oc| oc.horizon_ns == horizon_ns && oc.hit)
        })
        .count();
    let win_rate = hits as f64 / n as f64;
    let (p25, p50, p75) = percentiles(&net);

    // REL-26/R-7: sustained decay across consecutive chronological windows.
    // Sorted by fire time — observations arrive in fire order, but sort
    // defensively so the windows are unambiguous.
    let mut chrono: Vec<(i64, f64)> = closed
        .iter()
        .filter_map(|o| {
            o.outcomes
                .iter()
                .find(|oc| oc.horizon_ns == horizon_ns)
                .map(|oc| (o.timestamp_ns, oc.net_return))
        })
        .collect();
    chrono.sort_by_key(|&(t, _)| t);
    let decay_windows = decay_windows.max(2);
    if chrono.len() >= (decay_windows as u64 * DECAY_MIN_PER_WINDOW) as usize {
        let chunk = chrono.len() / decay_windows;
        let mut chunk_means: Vec<f64> = Vec::with_capacity(decay_windows);
        for w in 0..decay_windows {
            let start = w * chunk;
            let end = if w == decay_windows - 1 {
                chrono.len()
            } else {
                (w + 1) * chunk
            };
            let slice = &chrono[start..end];
            chunk_means.push(slice.iter().map(|&(_, v)| v).sum::<f64>() / slice.len() as f64);
        }
        let recent = &chunk_means[chunk_means.len() - 2..];
        if recent.iter().all(|&m| m <= 0.0) {
            rejects.push(
                "DECAY_SUSPECT: the two most recent windows have non-positive net expectancy — sustained underperformance forces a retest (REL-26/R-7)"
                    .into(),
            );
        }
    }

    // REL-25/R-6: regime tagging + refusals.
    let regimes = regime_buckets(&closed, horizon_ns, regime_feature);
    let tagged: u64 = regimes.iter().map(|b| b.n).sum();
    if !regimes.is_empty() && tagged > 0 {
        let positives = regimes.iter().filter(|b| b.net_expectancy > 0.0).count();
        let negatives = regimes.len() - positives;
        if positives > 0 && negatives > 0 {
            // At least one regime must carry enough evidence to trust.
            let strong_pos = regimes
                .iter()
                .any(|b| b.net_expectancy > 0.0 && b.n >= REGIME_MIN_TAGGED);
            if !strong_pos {
                rejects.push(format!(
                    "REGIME_SAMPLE_TOO_SMALL_{:?}: positive-regime evidence below {} tagged outcomes (REL-25)",
                    regimes
                        .iter()
                        .filter(|b| b.net_expectancy > 0.0)
                        .map(|b| b.regime.clone())
                        .collect::<Vec<_>>(),
                    REGIME_MIN_TAGGED
                ));
            } else {
                let weak_negs: Vec<String> = regimes
                    .iter()
                    .filter(|b| b.net_expectancy <= 0.0)
                    .map(|b| b.regime.clone())
                    .collect();
                if weak_negs
                    .iter()
                    .any(|r| {
                        regimes
                            .iter()
                            .any(|b| &b.regime == r && b.n >= REGIME_MIN_TAGGED)
                    })
                {
                    for r in weak_negs {
                        rejects.push(format!(
                            "ONLY_WORKS_IN_{r}: the edge is regime-limited — cannot be presented as generally effective (REL-25/R-6)"
                        ));
                    }
                }
            }
        }
        if regimes.len() == 1 {
            rejects.push(format!(
                "REGIME_COVERAGE_SINGLE_{}: every tagged observation falls in one regime — general effectiveness unproven (REL-25/R-6)",
                regimes[0].regime
            ));
        }
    }

    // REL-24: promotion requires the Research tier (checked here so the
    // Preliminary refusal lands even when every other gate passes).
    let gate_passed = rejects.is_empty() && tier == SampleTier::Research;

    // Per-horizon rollup (Phase 7).
    let mut horizons_seen: Vec<i64> = observations
        .iter()
        .flat_map(|o| o.outcomes.iter().map(|oc| oc.horizon_ns))
        .collect();
    horizons_seen.sort_unstable();
    horizons_seen.dedup();
    let per_horizon = horizons_seen
        .iter()
        .filter_map(|&h| {
            let nets: Vec<f64> = observations
                .iter()
                .filter_map(|o| {
                    o.outcomes
                        .iter()
                        .find(|oc| oc.horizon_ns == h)
                        .map(|oc| oc.net_return)
                })
                .collect();
            if nets.is_empty() {
                return None;
            }
            let wins = observations
                .iter()
                .filter(|o| {
                    o.outcomes
                        .iter()
                        .any(|oc| oc.horizon_ns == h && oc.hit)
                })
                .count();
            Some(HorizonSummary {
                horizon_ns: h,
                n: nets.len() as u64,
                net_expectancy: nets.iter().sum::<f64>() / nets.len() as f64,
                win_rate: wins as f64 / nets.len() as f64,
            })
        })
        .collect();

    EvaluationReport {
        horizon_ns,
        n,
        min_n,
        tier,
        gate_passed,
        gross_expectancy,
        net_expectancy,
        win_rate,
        p25,
        p50,
        p75,
        regimes,
        per_horizon,
        reject_reasons: rejects,
    }
}

/// Regime buckets over the tagged sample (REL-25). Observations whose fire
/// snapshot carries `regime_feature` are bucketed by a sanitized label of
/// the value (`0.0` → TREND, `1.0` → CHOP, else the value uppercased with
/// non-alphanumerics collapsed to `_`); untagged observations are excluded
/// (covered by the overall gates).
fn regime_buckets(
    closed: &[&SignalObservation],
    horizon_ns: i64,
    regime_feature: &str,
) -> Vec<RegimeBucket> {
    if regime_feature.is_empty() {
        return Vec::new();
    }
    let mut order: Vec<String> = Vec::new();
    let mut acc: BTreeMap<String, (u64, f64, u64)> = BTreeMap::new();
    for o in closed {
        let Some(oc) = o.outcomes.iter().find(|oc| oc.horizon_ns == horizon_ns) else {
            continue;
        };
        let Some(&v) = o.feature_snapshot.get(regime_feature) else {
            continue;
        };
        if !v.is_finite() {
            continue; // a non-finite regime value tags nothing (never guesses)
        }
        let label = regime_label(v);
        let e = acc.entry(label.clone()).or_insert((0, 0.0, 0));
        e.0 += 1;
        e.1 += oc.net_return;
        if oc.hit {
            e.2 += 1;
        }
        if !order.contains(&label) {
            order.push(label);
        }
    }
    order
        .into_iter()
        .filter_map(|regime| {
            acc.remove(&regime).map(|(n, sum, wins)| RegimeBucket {
                regime,
                n,
                net_expectancy: sum / n as f64,
                win_rate: wins as f64 / n as f64,
            })
        })
        .collect()
}

/// Deterministic regime label for a snapshot value (REL-25).
fn regime_label(v: f64) -> String {
    // The existing TrendRegime encoding (spec 045): 0.0 = TREND, 1.0 = CHOP.
    if v == 0.0 {
        "TREND".to_string()
    } else if v == 1.0 {
        "CHOP".to_string()
    } else {
        let s = format!("{v}");
        let mut out = String::new();
        for ch in s.chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch.to_ascii_uppercase());
            } else if !out.ends_with('_') {
                out.push('_');
            }
        }
        let t = out.trim_matches('_');
        if t.is_empty() {
            "UNKNOWN".to_string()
        } else {
            t.to_string()
        }
    }
}

/// The promotion decision for a report (REL-18): promote iff the gate passed.
pub fn decide(report: &EvaluationReport) -> PromotionDecision {
    if report.gate_passed {
        PromotionDecision::pass(format!(
            "gate passed at horizon {}s: n={}, tier={}, net_expectancy={:.6}, win_rate={:.3}",
            report.horizon_ns / 1_000_000_000,
            report.n,
            report.tier.label(),
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
    use std::collections::BTreeMap;

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

    /// Observations tagged with a `regime.trend` value in the fire snapshot.
    fn obs_regime(net_returns: &[f64], horizon: i64, regime: f64) -> Vec<SignalObservation> {
        net_returns
            .iter()
            .enumerate()
            .map(|(i, &nr)| {
                let mut v = obs(&[nr], horizon);
                let o = &mut v[0];
                o.observation_id = i as u64;
                o.timestamp_ns = i as i64 * 1_000_000_000;
                o.feature_snapshot =
                    BTreeMap::from([(DEFAULT_REGIME_FEATURE.to_string(), regime)]);
                v.remove(0)
            })
            .collect()
    }

    #[test]
    fn rel_15_sample_size_gate_refuses_with_reason() {
        let report = evaluate(&obs(&[0.001; 10], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(!report.gate_passed);
        assert_eq!(report.n, 10);
        assert_eq!(report.tier, SampleTier::Insufficient);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("INSUFFICIENT_SAMPLE")));
        assert!(!decide(&report).promote);
    }

    #[test]
    fn rel_24_preliminary_tier_refuses_promotion() {
        // 30..99 outcomes: enough to evaluate, never enough to promote.
        let report = evaluate(&obs(&[0.005; 30], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert_eq!(report.tier, SampleTier::Preliminary);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("SAMPLE_TIER_PRELIMINARY")));
        assert!(!report.gate_passed);
        assert!(!decide(&report).promote);
        // 99 is still Preliminary.
        let report = evaluate(&obs(&[0.005; 99], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert_eq!(report.tier, SampleTier::Preliminary);
        // 100 crosses the Research floor.
        let report = evaluate(&obs(&[0.005; 100], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert_eq!(report.tier, SampleTier::Research);
        assert!(report.gate_passed);
    }

    #[test]
    fn rel_16_gross_vs_net_expectancy_and_win_rate() {
        // 100 outcomes; net returns use EXACT binary fractions (0.375 / −0.875
        // — exact 3:7 ratio for the 70/30 split, so 26.25 − 26.25 = 0) making
        // the mean exactly 0.0: gross = net + 0.002 cost ⇒ gross exp ≈ +0.002
        // > 0 but NET is exactly 0 — no positive net edge.
        let mut nr = vec![0.375; 70];
        nr.extend(vec![-0.875; 30]);
        let report = evaluate(&obs(&nr, 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!((report.gross_expectancy - 0.002).abs() < 1e-9);
        assert_eq!(report.net_expectancy, 0.0);
        // Gross > 0 but net ≤ 0 ⇒ refused with the net-edge reason.
        assert!(!report.gate_passed);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("NET_EXPECTANCY_NEGATIVE")));
        assert!((report.win_rate - 70.0 / 100.0).abs() < 1e-12);
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
        // REL-24: promotion now requires the Research tier (≥ 100).
        let good = evaluate(&obs(&[0.005; 120], 3_600_000_000_000), 3_600_000_000_000, 30);
        let d = decide(&good);
        assert!(d.promote);
        assert!(d.reasons.iter().any(|r| r.contains("gate passed")));
        assert_eq!(d.decision_label(), "PROMOTE");
        let bad = evaluate(&obs(&[-0.01; 120], 3_600_000_000_000), 3_600_000_000_000, 30);
        let d = decide(&bad);
        assert!(!d.promote);
        assert!(!d.reasons.is_empty());
        assert_eq!(d.decision_label(), "REJECT");
        // REL-29: the JSON artifact matches the task's structured format.
        let json = d.to_json();
        assert!(json.starts_with("{\"decision\":\"REJECT\""));
        assert!(json.contains("NET_EXPECTANCY_NEGATIVE"));
    }

    #[test]
    fn rel_19_grade_from_passed_report_only() {
        let good = evaluate(&obs(&[0.005; 120], 3_600_000_000_000), 3_600_000_000_000, 30);
        let g = grade_from_report("fp", "run-1", 0, &good).expect("passed ⇒ grade");
        assert_eq!(g.n, 120);
        assert_eq!(g.identity, "fp");
        assert_eq!(g.avg_excess, good.net_expectancy);
        // A Preliminary-tier report passes every other gate but MUST NOT
        // grade (REL-24: promotion requires Research).
        let prelim = evaluate(&obs(&[0.005; 30], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(grade_from_report("fp", "run-2", 0, &prelim).is_none());
        let bad = evaluate(&obs(&[-0.01; 120], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(grade_from_report("fp", "run-3", 0, &bad).is_none());
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

    #[test]
    fn rel_26_sustained_decay_refuses_promotion() {
        // 120 outcomes in 3 equal windows of 40: early windows strongly
        // positive, the TWO most recent ≤ 0 ⇒ DECAY_SUSPECT.
        let mut nr: Vec<f64> = vec![0.01; 40];
        nr.extend(vec![-0.001; 40]);
        nr.extend(vec![-0.002; 40]);
        let report = evaluate(&obs(&nr, 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("DECAY_SUSPECT")));
        assert!(!report.gate_passed);
        // An edge that DIED but was positive early is still refused even
        // though the all-time net expectancy is positive (0.01 − 0.0015 ≫ 0).
        assert!(report.net_expectancy > 0.0);
    }

    #[test]
    fn rel_26_improving_edge_is_not_decay() {
        // Early windows negative, recent windows positive: that is an edge
        // appearing, not decaying — no DECAY_SUSPECT.
        let mut nr: Vec<f64> = vec![-0.002; 40];
        nr.extend(vec![-0.001; 40]);
        nr.extend(vec![0.01; 40]);
        let report = evaluate(&obs(&nr, 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(!report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("DECAY_SUSPECT")));
        assert!(report.gate_passed);
    }

    #[test]
    fn rel_26_tiny_windows_are_not_judged() {
        // 3 windows of 4 (< DECAY_MIN_PER_WINDOW): decay is not judged even
        // though the recent windows are negative.
        let mut nr: Vec<f64> = vec![0.01; 12];
        nr.extend(vec![-0.01; 12]);
        let report = evaluate(&obs(&nr, 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(!report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("DECAY_SUSPECT")));
    }

    #[test]
    fn rel_25_only_works_in_one_regime_is_refused() {
        // TREND strongly positive, CHOP strongly negative — both regimes
        // well-tagged (≥ 10 each): general effectiveness unproven.
        let mut os = obs_regime(&[0.01; 60], 3_600_000_000_000, 0.0);
        os.extend(obs_regime(&[-0.01; 60], 3_600_000_000_000, 1.0));
        let report = evaluate(&os, 3_600_000_000_000, 30);
        assert!(report.regimes.len() == 2);
        let trend = report.regimes.iter().find(|b| b.regime == "TREND").unwrap();
        let chop = report.regimes.iter().find(|b| b.regime == "CHOP").unwrap();
        assert!(trend.net_expectancy > 0.0 && chop.net_expectancy < 0.0);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("ONLY_WORKS_IN_CHOP")));
        assert!(!report.gate_passed);
        assert!(!decide(&report).promote);
    }

    #[test]
    fn rel_25_single_regime_coverage_is_refused() {
        // Every tagged observation in ONE regime: the edge may exist only
        // there — general effectiveness unproven.
        let os = obs_regime(&[0.01; 120], 3_600_000_000_000, 0.0);
        let report = evaluate(&os, 3_600_000_000_000, 30);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("REGIME_COVERAGE_SINGLE_TREND")));
        assert!(!report.gate_passed);
    }

    #[test]
    fn rel_25_positive_regime_sample_too_small_is_refused() {
        // TREND positive but only 9 tagged (< REGIME_MIN_TAGGED), CHOP
        // negative and well-tagged: the positive-regime claim is unproven.
        let mut os = obs_regime(&[0.01; 9], 3_600_000_000_000, 0.0);
        os.extend(obs_regime(&[-0.01; 100], 3_600_000_000_000, 1.0));
        let report = evaluate(&os, 3_600_000_000_000, 30);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("REGIME_SAMPLE_TOO_SMALL_")));
        assert!(!report.gate_passed);
    }

    #[test]
    fn rel_25_balanced_regimes_pass() {
        // Both regimes positive and well-tagged: no regime refusal.
        let mut os = obs_regime(&[0.01; 60], 3_600_000_000_000, 0.0);
        os.extend(obs_regime(&[0.008; 60], 3_600_000_000_000, 1.0));
        let report = evaluate(&os, 3_600_000_000_000, 30);
        assert!(report
            .reject_reasons
            .iter()
            .all(|r| !r.starts_with("ONLY_WORKS_IN")
                && !r.starts_with("REGIME_SAMPLE_TOO_SMALL")
                && !r.starts_with("REGIME_COVERAGE_SINGLE")));
        assert!(report.gate_passed);
    }

    #[test]
    fn rel_25_untagged_observations_skip_regime_gates() {
        // No regime feature in the snapshots ⇒ no regime buckets, no regime
        // refusals (the overall gates still apply).
        let report = evaluate(&obs(&[0.005; 120], 3_600_000_000_000), 3_600_000_000_000, 30);
        assert!(report.regimes.is_empty());
        assert!(report.gate_passed);
    }

    #[test]
    fn rel_25_non_finite_regime_value_tags_nothing() {
        let mut os = obs_regime(&[0.01; 60], 3_600_000_000_000, 0.0);
        os.extend(obs_regime(&[0.008; 60], 3_600_000_000_000, f64::NAN));
        let report = evaluate(&os, 3_600_000_000_000, 30);
        // Only the TREND bucket exists; the NaN-valued observations are
        // untagged, so the single-regime refusal fires (never a guess).
        assert_eq!(report.regimes.len(), 1);
        assert!(report
            .reject_reasons
            .iter()
            .any(|r| r.starts_with("REGIME_COVERAGE_SINGLE_TREND")));
    }

    #[test]
    fn rel_29_reason_codes_are_machine_readable() {
        // The task's structured format: {"decision": "REJECT", "reasons": [
        //   "INSUFFICIENT_SAMPLE", "NET_EXPECTANCY_NEGATIVE", …]}
        let tiny = evaluate(&obs(&[-0.01; 5], 3_600_000_000_000), 3_600_000_000_000, 30);
        let json = decide(&tiny).to_json();
        assert!(json.contains("\"INSUFFICIENT_SAMPLE"));
        let full_bad = evaluate(&obs(&[-0.01; 120], 3_600_000_000_000), 3_600_000_000_000, 30);
        let codes: Vec<&str> = full_bad
            .reject_reasons
            .iter()
            .filter_map(|r| r.split(':').next())
            .collect();
        assert!(codes.contains(&"NET_EXPECTANCY_NEGATIVE"));
        assert!(codes.contains(&"DECAY_SUSPECT"));
    }

    #[test]
    fn rel_24_per_horizon_rollup_covers_all_horizons() {
        let mut os = obs(&[0.005; 60], 3_600_000_000_000);
        os.extend(obs(&[0.004; 60], 86_400_000_000_000));
        let report = evaluate(&os, 3_600_000_000_000, 30);
        assert_eq!(report.per_horizon.len(), 2);
        let h1 = report
            .per_horizon
            .iter()
            .find(|h| h.horizon_ns == 3_600_000_000_000)
            .unwrap();
        assert_eq!(h1.n, 60);
        assert!((h1.net_expectancy - 0.005).abs() < 1e-12);
    }
}
