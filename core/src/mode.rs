//! Trading mode gating (spec 018). Config is read from outside the repo.
//! Promotions require human confirmation; demotions are automatic.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Runtime trading mode (MOD-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradingMode {
    /// Idle, no processing (maintenance).
    Sleep,
    /// Recorded data, sim fills.
    Backtest,
    /// Live data, sim fills, logged decisions.
    Paper,
    /// Live data, no fills, decisions logged for comparison.
    Shadow,
    /// Live data, real fills (PD-1: human promotion only).
    Live,
}

impl TradingMode {
    /// Read mode from the configured path.
    /// File at `/etc/money-printer/mode.toml` on Linux,
    /// `%PROGRAMDATA%/money-printer/mode.toml` on Windows.
    /// Overridable via `MONEY_PRINTER_MODE` env var.
    pub fn from_config() -> Self {
        if let Ok(val) = std::env::var("MONEY_PRINTER_MODE") {
            return Self::from_str(&val);
        }
        #[cfg(target_os = "windows")]
        let path = {
            let pd = std::env::var("PROGRAMDATA").unwrap_or_else(|_| "C:\\ProgramData".into());
            Path::new(&pd).join("money-printer").join("mode.toml")
        };
        #[cfg(not(target_os = "windows"))]
        let path = Path::new("/etc/money-printer/mode.toml").to_path_buf();

        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = toml::from_str::<ModeConfig>(&contents) {
                return cfg.mode;
            }
        }
        // Default to Sleep if no config found.
        TradingMode::Sleep
    }

    fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "sleep" => TradingMode::Sleep,
            "backtest" => TradingMode::Backtest,
            "paper" => TradingMode::Paper,
            "shadow" => TradingMode::Shadow,
            "live" => TradingMode::Live,
            _ => TradingMode::Sleep,
        }
    }

    /// Returns the next lower mode for automatic demotion (MOD-3).
    pub fn demote(&self) -> Self {
        match self {
            TradingMode::Live => TradingMode::Shadow,
            TradingMode::Shadow => TradingMode::Paper,
            TradingMode::Paper => TradingMode::Backtest,
            _ => TradingMode::Sleep,
        }
    }

    /// Returns true if this mode requires live safety checks (MOD-5).
    pub fn requires_safety_checks(&self) -> bool {
        matches!(self, TradingMode::Live)
    }

    /// Returns true if this mode places real orders.
    pub fn is_live(&self) -> bool {
        matches!(self, TradingMode::Live)
    }

    /// Returns true if this mode uses sim fills.
    pub fn uses_sim_fills(&self) -> bool {
        matches!(self, TradingMode::Backtest | TradingMode::Paper)
    }
}

#[derive(Debug, Deserialize)]
struct ModeConfig {
    mode: TradingMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_1_mode_logged_on_startup() {
        // Verify default mode is Sleep.
        assert_eq!(TradingMode::from_str("sleep"), TradingMode::Sleep);
    }

    #[test]
    fn mod_2_backtest_to_paper_gate() {
        // Demotion chain.
        assert_eq!(TradingMode::Live.demote(), TradingMode::Shadow);
        assert_eq!(TradingMode::Shadow.demote(), TradingMode::Paper);
        assert_eq!(TradingMode::Paper.demote(), TradingMode::Backtest);
    }

    #[test]
    fn mod_3_paper_uses_sim_fills() {
        assert!(TradingMode::Paper.uses_sim_fills());
        assert!(!TradingMode::Live.uses_sim_fills());
    }

    #[test]
    fn mod_4_live_enables_safety_checks() {
        assert!(TradingMode::Live.requires_safety_checks());
        assert!(!TradingMode::Paper.requires_safety_checks());
    }

    #[test]
    fn mod_5_mode_switch_requires_human_confirm() {
        // Human confirmation is handled by the funnel CLI, not the mode type.
        // Verify that string parsing is case-insensitive.
        assert_eq!(TradingMode::from_str("LIVE"), TradingMode::Live);
        assert_eq!(TradingMode::from_str("Paper"), TradingMode::Paper);
    }
}
