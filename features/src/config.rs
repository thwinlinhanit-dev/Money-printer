//! Catalog configuration (FEA-7): every feature's params live in one
//! `features.toml`, parsed with `deny_unknown_fields` (a typo'd key is an
//! error, not a silent default), and hashed into materialization metadata so a
//! params change forces a new `ver=N` feature-store directory (FEA-6).
//!
//! Pure: parses from a `&str` the caller read at the binary edge — no I/O and
//! no wall clock here (PD-3).

use serde::{Deserialize, Serialize};

/// Error parsing `features.toml`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("features.toml parse error: {0}")]
    Parse(String),
    #[error("features.toml re-serialize error: {0}")]
    Serialize(String),
}

/// Params for the `cvd.*` feature family.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CvdParams {
    /// Venues to compute per-venue CVD for.
    #[serde(default)]
    pub venues: Vec<String>,
}

impl Default for CvdParams {
    fn default() -> Self {
        CvdParams {
            venues: vec!["bybit".into()],
        }
    }
}

/// Params for `whale_print` (large single-trade detector).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhalePrintParams {
    pub min_notional: f64,
}

impl Default for WhalePrintParams {
    fn default() -> Self {
        WhalePrintParams {
            min_notional: 250_000.0,
        }
    }
}

/// Params for `liq.cluster` (liquidation clustering).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiqClusterParams {
    pub window_ns: i64,
    pub min_cluster_notional: f64,
}

impl Default for LiqClusterParams {
    fn default() -> Self {
        LiqClusterParams {
            window_ns: 60_000_000_000, // 1 minute
            min_cluster_notional: 5_000_000.0,
        }
    }
}

/// The whole catalog config (FEA-7). One file, all params, no unknown keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeaturesConfig {
    /// Bar timeframe for bar-derived features (ns).
    #[serde(default = "default_bar_tf")]
    pub bar_tf_ns: i64,
    #[serde(default)]
    pub cvd: CvdParams,
    #[serde(default)]
    pub whale_print: WhalePrintParams,
    #[serde(default)]
    pub liq_cluster: LiqClusterParams,
}

fn default_bar_tf() -> i64 {
    60_000_000_000
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        FeaturesConfig {
            bar_tf_ns: default_bar_tf(),
            cvd: CvdParams::default(),
            whale_print: WhalePrintParams::default(),
            liq_cluster: LiqClusterParams::default(),
        }
    }
}

impl FeaturesConfig {
    /// Parse and validate `features.toml` text. Unknown keys are rejected
    /// (deny_unknown_fields) so a mistyped param never silently defaults.
    pub fn from_toml(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// Stable content hash of the *canonical* params (FEA-6/7): parse-normalize
    /// then hash, so formatting/whitespace/key-order differences don't change
    /// the hash but any real param change does. Rendered as a hex string for
    /// the Parquet footer and the `ver=N` marker.
    pub fn params_hash(&self) -> Result<String, ConfigError> {
        let canonical = toml::to_string(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in canonical.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(format!("{h:016x}"))
    }
}
