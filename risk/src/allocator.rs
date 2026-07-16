//! Portfolio allocator (RSK-4). Weights strategies by expectancy rank × regime
//! fit × correlation penalty × drawdown governor, capped by each strategy's
//! Kelly ceiling and renormalized so the deployed total stays within budget.
//! Intraday it may only *shrink* (risk-off is unilateral; risk-on waits for the
//! daily run) — the safety asymmetry.

use std::collections::BTreeMap;

/// Per-strategy allocator inputs for one run.
#[derive(Debug, Clone, Copy)]
pub struct StrategyInput {
    /// Base weight from rolling live expectancy rank (≥ 0; 0 if not positive).
    pub base_w: f64,
    /// 1.0 if the current regime matches the strategy's declared regime, else
    /// `regime_penalty` (RSK-7 — read from live regime features, never opinion).
    pub regime_fit: f64,
    /// Correlation penalty in `(0, 1]` (crowded-together strategies shrink).
    pub corr_penalty: f64,
    /// Drawdown-governor multiplier `g(dd)` in `[0, 1]`.
    pub dd_gov: f64,
    /// Kelly ceiling for this strategy.
    pub kelly_cap: f64,
}

/// Allocator parameters.
#[derive(Debug, Clone, Copy)]
pub struct AllocParams {
    /// Maximum total deployed weight (default 0.8).
    pub max_deployed: f64,
}

impl Default for AllocParams {
    fn default() -> Self {
        Self { max_deployed: 0.8 }
    }
}

/// Compute allocation weights (RSK-4). Deterministic (BTreeMap order).
pub fn allocate(
    params: &AllocParams,
    inputs: &BTreeMap<String, StrategyInput>,
) -> BTreeMap<String, f64> {
    let mut weights: BTreeMap<String, f64> = BTreeMap::new();
    let mut sum = 0.0;
    for (id, i) in inputs {
        let raw = i.base_w.max(0.0)
            * i.regime_fit.clamp(0.0, 1.0)
            * i.corr_penalty.clamp(0.0, 1.0)
            * i.dd_gov.clamp(0.0, 1.0);
        let capped = raw.min(i.kelly_cap.max(0.0));
        let w = if capped.is_finite() { capped } else { 0.0 };
        weights.insert(id.clone(), w);
        sum += w;
    }
    // Renormalize down (never up) to respect the deployed budget.
    if sum > params.max_deployed && sum > 0.0 {
        let scale = params.max_deployed / sum;
        for w in weights.values_mut() {
            *w *= scale;
        }
    }
    weights
}

/// Enforce the intraday shrink-only rule (RSK-4): given previous and freshly
/// computed weights, never let a weight rise until the next daily run.
pub fn shrink_only(
    prev: &BTreeMap<String, f64>,
    proposed: &BTreeMap<String, f64>,
) -> BTreeMap<String, f64> {
    proposed
        .iter()
        .map(|(id, &w)| {
            let capped = prev.get(id).map(|&p| w.min(p)).unwrap_or(w);
            (id.clone(), capped)
        })
        .collect()
}
