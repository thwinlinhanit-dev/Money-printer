//! Leverage-tier weight calibration (spec 029 LIQ-11): turn the recorded
//! spec 028 real leverage distribution into the `liq.est_bands` tier weights
//! that were a documented assumption (LIQ-2 Decision). Pure + deterministic
//! (PD-3/CONV-9): a function of (samples, tier leverages), no I/O, no clock.
//!
//! ## Semantics
//! Weights are the **notional share** of open interest at each tier, because
//! the band model spreads OI across tiers (`notional_at_risk += oi × weight`,
//! [`crate::liquidation::LiqEstBands`]) — a count-weighted histogram would
//! misrepresent where the OI actually sits. A position's notional proxy is
//! `|size| × entry` (coin units × entry price).
//!
//! Samples are bucketed by **geometric midpoint**: tier `i` owns
//! `[boundary_lo, boundary_hi)` with boundaries `sqrt(L_i · L_{i+1})`; a
//! sample exactly on a boundary goes to the HIGHER-leverage tier (closer to
//! liquidation — conservative for a cascade model).
//!
//! Fail-closed (CONV-8): non-finite/non-positive leverage or notional samples
//! are skipped, never counted. The output is order-independent (per-bucket
//! accumulation) and sorted ascending by leverage (CONV-10).

use crate::config::LeverageTier;

/// One calibrated tier bucket: `weight` = the bucket's share of the total
/// sampled notional (`Σ weights ≈ 1.0` for the valid sample set).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeverageTierCalibration {
    pub leverage: f64,
    /// Share of total sampled notional held in this tier's bucket.
    pub weight: f64,
    /// Number of positions in the bucket.
    pub count: u64,
    /// Sampled notional in the bucket (`|size|·entry` per position).
    pub notional: f64,
}

/// Calibrate tier weights from `(leverage, notional)` position samples.
///
/// `tier_leverages` may arrive in any order; non-positive/non-finite entries
/// are dropped, the rest sorted ascending and de-duplicated. Deterministic:
/// identical inputs ⇒ identical output (CONV-9), regardless of sample order
/// (accumulation is per-bucket). Empty samples or tiers ⇒ all-zero weights —
/// the caller decides whether an empty census is a failed run.
pub fn calibrate_leverage_weights(
    samples: &[(f64, f64)],
    tier_leverages: &[f64],
) -> Vec<LeverageTierCalibration> {
    let mut tiers: Vec<f64> = tier_leverages
        .iter()
        .copied()
        .filter(|l| l.is_finite() && *l > 0.0)
        .collect();
    tiers.sort_by(f64::total_cmp);
    tiers.dedup_by(|a, b| a == b);
    if tiers.is_empty() {
        return Vec::new();
    }

    let mut counts = vec![0u64; tiers.len()];
    let mut notionals = vec![0.0_f64; tiers.len()];
    for &(lev, notional) in samples {
        // Fail-closed (CONV-8): a corrupt sample must never look real.
        if !lev.is_finite() || lev <= 0.0 || !notional.is_finite() || notional <= 0.0 {
            continue;
        }
        let i = bucket_index(lev, &tiers);
        counts[i] += 1;
        notionals[i] += notional;
    }

    let total: f64 = notionals.iter().sum();
    tiers
        .into_iter()
        .zip(counts)
        .zip(notionals)
        .map(|((leverage, count), notional)| LeverageTierCalibration {
            leverage,
            weight: if total > 0.0 { notional / total } else { 0.0 },
            count,
            notional,
        })
        .collect()
}

/// Index of the tier bucket containing `x` under geometric-midpoint
/// boundaries. A sample exactly on a boundary `sqrt(L_i·L_{i+1})` fails the
/// `x < boundary` test for tier `i` and lands in the HIGHER tier (closer to
/// liquidation — conservative). `tiers` must be sorted ascending, non-empty;
/// the top tier's boundary is `+∞` so a finite `x` always finds a bucket.
fn bucket_index(x: f64, tiers: &[f64]) -> usize {
    for (i, t) in tiers.iter().enumerate() {
        let hi = match tiers.get(i + 1) {
            Some(next) => (t * next).sqrt(),
            None => f64::INFINITY,
        };
        if x < hi {
            return i;
        }
    }
    unreachable!("top tier's boundary is +∞; a finite x always finds a bucket")
}

/// Convenience for callers that hold the config's tier list: extract the
/// sorted-ascending leverage values to calibrate against.
pub fn tier_leverages(tiers: &[LeverageTier]) -> Vec<f64> {
    let mut v: Vec<f64> = tiers.iter().map(|t| t.leverage).collect();
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| a == b);
    v
}
