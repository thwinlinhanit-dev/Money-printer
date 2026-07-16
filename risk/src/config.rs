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
        out
    }
}

/// RSK-7: regime fit computed from LIVE regime feature values — never a human
/// opinion field. Inputs are the FEA catalog encodings: `regime.vol` ∈
/// {0=low, 1=mid, 2=high}, `regime.trend` ∈ {0=chop, 1=trend}. The declared
/// mask holds labels like "trend", "chop", "low_vol", "mid_vol", "high_vol";
/// an empty mask means any regime. Returns 1.0 on match, `penalty` otherwise.
pub fn regime_fit_from_features(
    declared: &[String],
    regime_vol: f64,
    regime_trend: f64,
    penalty: f64,
) -> f64 {
    if declared.is_empty() {
        return 1.0;
    }
    let vol_label = match regime_vol as i64 {
        0 => "low_vol",
        1 => "mid_vol",
        _ => "high_vol",
    };
    let trend_label = if regime_trend as i64 == 1 {
        "trend"
    } else {
        "chop"
    };
    let matches = declared.iter().any(|l| l == vol_label || l == trend_label);
    if matches {
        1.0
    } else {
        penalty.clamp(0.0, 1.0)
    }
}
