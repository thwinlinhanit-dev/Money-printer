//! `risk.toml` (RSK-6): ALL risk parameters live in one file, parsed with
//! `deny_unknown_fields` (a typo'd limit is an error, not a silent default),
//! and every change is journaled `old→new, ts, actor`. Changing DEFAULTS is
//! owner-only (CLAUDE.md safety table); this module only parses and diffs.

use crate::gate::RiskLimits;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("risk.toml parse error: {0}")]
    Parse(String),
}

/// The on-disk shape of `risk.toml` (RSK-6). Field names match [`RiskLimits`]
/// one-to-one so the mapping is mechanical.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskConfig {
    pub max_order_notional: f64,
    pub max_position_notional: f64,
    pub max_gross_portfolio: f64,
    pub max_px_dev_frac: f64,
    pub max_orders_per_min: u32,
    pub strategy_daily_loss_budget: f64,
    pub portfolio_daily_loss_budget: f64,
    pub max_concurrent_positions: u32,
    pub max_corr_adjusted_portfolio: f64,
}

impl RiskConfig {
    pub fn from_toml(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    pub fn to_limits(&self) -> RiskLimits {
        RiskLimits {
            max_order_notional: self.max_order_notional,
            max_position_notional: self.max_position_notional,
            max_gross_portfolio: self.max_gross_portfolio,
            max_px_dev_frac: self.max_px_dev_frac,
            max_orders_per_min: self.max_orders_per_min,
            strategy_daily_loss_budget: self.strategy_daily_loss_budget,
            portfolio_daily_loss_budget: self.portfolio_daily_loss_budget,
            max_concurrent_positions: self.max_concurrent_positions,
            max_corr_adjusted_portfolio: self.max_corr_adjusted_portfolio,
        }
    }

    /// RSK-6 change journal: one line per changed field, `field: old -> new`,
    /// stamped with the injected ts and the actor. Empty when nothing changed —
    /// a limit change that isn't journaled didn't happen.
    pub fn journal_change(&self, new: &RiskConfig, ts_ns: i64, actor: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut line = |field: &str, old: f64, newv: f64| {
            if old != newv {
                out.push(format!("{ts_ns}|{actor}|{field}: {old} -> {newv}"));
            }
        };
        line(
            "max_order_notional",
            self.max_order_notional,
            new.max_order_notional,
        );
        line(
            "max_position_notional",
            self.max_position_notional,
            new.max_position_notional,
        );
        line(
            "max_gross_portfolio",
            self.max_gross_portfolio,
            new.max_gross_portfolio,
        );
        line("max_px_dev_frac", self.max_px_dev_frac, new.max_px_dev_frac);
        line(
            "max_orders_per_min",
            self.max_orders_per_min as f64,
            new.max_orders_per_min as f64,
        );
        line(
            "strategy_daily_loss_budget",
            self.strategy_daily_loss_budget,
            new.strategy_daily_loss_budget,
        );
        line(
            "portfolio_daily_loss_budget",
            self.portfolio_daily_loss_budget,
            new.portfolio_daily_loss_budget,
        );
        line(
            "max_corr_adjusted_portfolio",
            self.max_corr_adjusted_portfolio,
            new.max_corr_adjusted_portfolio,
        );
        // The `line` closure above is dead here (NLL) — safe to push directly.
        if self.max_concurrent_positions != new.max_concurrent_positions {
            out.push(format!(
                "{ts_ns}|{actor}|max_concurrent_positions: {} -> {}",
                self.max_concurrent_positions, new.max_concurrent_positions
            ));
        }
        out
    }
}

/// RSK-7: regime fit computed from LIVE regime feature values — never a human
/// opinion field. Inputs are the FEA catalog encodings: `regime.vol` ∈
/// {0=low, 1=mid, 2=high}, `regime.trend` ∈ {0=chop, 1=trend}. The declared
/// mask holds labels like "trend", "chop", "low_vol", "mid_vol", "high_vol";
/// an empty mask means any regime. Returns 1.0 on match, `penalty` otherwise.
///
/// Fail-closed (CONV-8): the labels must be EXACTLY 0, 1, or 2. A silent
/// `f64 as i64` cast would map e.g. 2.7 → 2 ("high_vol") — a malformed feature
/// passing as a real regime. Any non-{0,1,2}, fractional, or non-finite label
/// logs a WARN and falls back to `penalty` (de-weighted, never full fit).
pub fn regime_fit_from_features(
    declared: &[String],
    regime_vol: f64,
    regime_trend: f64,
    penalty: f64,
) -> f64 {
    let fallback = || penalty.clamp(0.0, 1.0);
    if declared.is_empty() {
        return 1.0;
    }
    // Valid only when finite AND exactly one of the catalog encodings.
    let vol_label = if regime_vol == 0.0 {
        "low_vol"
    } else if regime_vol == 1.0 {
        "mid_vol"
    } else if regime_vol == 2.0 {
        "high_vol"
    } else {
        tracing::warn!(
            regime_vol,
            "invalid regime.vol label (not 0/1/2) → fail-closed penalty (RSK-7)"
        );
        return fallback();
    };
    let trend_label = if regime_trend == 0.0 {
        "chop"
    } else if regime_trend == 1.0 {
        "trend"
    } else {
        tracing::warn!(
            regime_trend,
            "invalid regime.trend label (not 0/1) → fail-closed penalty (RSK-7)"
        );
        return fallback();
    };
    let matches = declared.iter().any(|l| l == vol_label || l == trend_label);
    if matches {
        1.0
    } else {
        fallback()
    }
}
