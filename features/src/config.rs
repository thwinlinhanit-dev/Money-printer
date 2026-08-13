//! Catalog configuration (FEA-7): every feature's params live in one
//! `features.toml`, parsed with `deny_unknown_fields` (a typo'd key is an
//! error, not a silent default), and hashed into materialization metadata so a
//! params change forces a new `ver=N` feature-store directory (FEA-6).
//!
//! Pure: parses from a `&str` the caller read at the binary edge — no I/O and
//! no wall clock here (PD-3).

use mp_core::{fnv1a_absorb, FNV1A_OFFSET};
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

/// Params for `whale_print.{venue}` (large single-trade detector).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhalePrintParams {
    pub min_notional: f64,
    /// Venues to track (default: hyperliquid only).
    #[serde(default = "default_whale_venues")]
    pub venues: Vec<String>,
}

fn default_whale_venues() -> Vec<String> {
    vec!["hyperliquid".into()]
}

impl Default for WhalePrintParams {
    fn default() -> Self {
        WhalePrintParams {
            min_notional: 250_000.0,
            venues: default_whale_venues(),
        }
    }
}

/// Params for the `whale.net.{venue}` / `whale.delta.{venue}` feature family
/// (spec 028 aggregate whale net positioning + deltas, feature engine 004).
/// Only venues with a recorded `WhalePosition` census make sense.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhaleNetParams {
    /// Venues to aggregate the position census for.
    #[serde(default = "default_whale_venues")]
    pub venues: Vec<String>,
    /// A position not refreshed within this window (ns) is evicted from the
    /// census — a flattened position (no tombstone event) or an address that
    /// dropped off top-N must not linger in the aggregate. 10 min default.
    #[serde(default = "default_whale_stale_ns")]
    pub stale_after_ns: i64,
}

fn default_whale_stale_ns() -> i64 {
    // Single source of truth lives with the feature (whale.rs) so a
    // recalibration can never drift the two apart.
    crate::whale::DEFAULT_WHALE_STALE_NS
}

impl Default for WhaleNetParams {
    fn default() -> Self {
        WhaleNetParams {
            venues: default_whale_venues(),
            stale_after_ns: default_whale_stale_ns(),
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

/// Params for `liq.agg` (cross-venue de-sampled liquidation tape, spec 029
/// LIQ-1 / LIQ-7). Both windows in nanoseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiqAggParams {
    /// Near-simultaneous liquidations reported within this window (ns) across
    /// venues are treated as one event (de-duplicated). 250 ms default.
    #[serde(default = "default_liq_agg_dedup_ns")]
    pub dedup_window_ns: i64,
    /// Rolling window (ns) for the emitted aggregate notional. 1 s default.
    #[serde(default = "default_liq_agg_window_ns")]
    pub agg_window_ns: i64,
}

fn default_liq_agg_dedup_ns() -> i64 {
    250_000_000
}
fn default_liq_agg_window_ns() -> i64 {
    1_000_000_000
}

impl Default for LiqAggParams {
    fn default() -> Self {
        Self {
            dedup_window_ns: default_liq_agg_dedup_ns(),
            agg_window_ns: default_liq_agg_window_ns(),
        }
    }
}

/// One leverage tier and its open-interest weight (spec 029 LIQ-2). Weights are
/// a documented assumption (Σ ≈ 1.0), calibrated later from spec 028 real
/// positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeverageTier {
    pub leverage: f64,
    pub weight: f64,
}

/// Params for `liq.est_bands` (estimated liquidation cascade bands, spec 029
/// LIQ-2 / LIQ-7). The estimate is a MODEL — validate against spec 028 real
/// liq prices (RES-4) before any strategy use (LIQ-6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiqEstBandsParams {
    /// Maintenance-to-initial margin buffer: mmr = 1/(leverage × this). 1.5.
    #[serde(default = "default_maintenance_buffer")]
    pub maintenance_buffer: f64,
    /// Leverage tiers + OI weights; the highest-leverage tier drives the
    /// nearest estimated cascade level.
    #[serde(default = "default_leverage_tiers")]
    pub leverage_tiers: Vec<LeverageTier>,
}

fn default_maintenance_buffer() -> f64 {
    1.5
}
fn default_leverage_tiers() -> Vec<LeverageTier> {
    vec![
        LeverageTier {
            leverage: 1.0,
            weight: 0.05,
        },
        LeverageTier {
            leverage: 2.0,
            weight: 0.10,
        },
        LeverageTier {
            leverage: 5.0,
            weight: 0.15,
        },
        LeverageTier {
            leverage: 10.0,
            weight: 0.25,
        },
        LeverageTier {
            leverage: 20.0,
            weight: 0.25,
        },
        LeverageTier {
            leverage: 50.0,
            weight: 0.20,
        },
    ]
}

impl Default for LiqEstBandsParams {
    fn default() -> Self {
        Self {
            maintenance_buffer: default_maintenance_buffer(),
            leverage_tiers: default_leverage_tiers(),
        }
    }
}

/// One footprint size bucket (notional USD range; trades priced out of the
/// range belong to no bucket). `max_usd` is exclusive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FootprintBucketDef {
    /// Bucket id embedded in feature names (`footprint.delta.{tf}.{name}`).
    pub name: String,
    pub min_usd: f64,
    pub max_usd: f64,
}

/// Params for the `footprint.*` feature family (orderflow delta / imbalance
/// per size bucket, spec 004 §Order flow).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FootprintParams {
    /// Bar timeframe footprint buckets roll on (ns).
    #[serde(default = "default_bar_tf")]
    pub bar_tf_ns: i64,
    #[serde(default = "default_footprint_buckets")]
    pub buckets: Vec<FootprintBucketDef>,
}

fn default_footprint_buckets() -> Vec<FootprintBucketDef> {
    vec![
        FootprintBucketDef {
            name: "small".into(),
            min_usd: 0.0,
            max_usd: 25_000.0,
        },
        FootprintBucketDef {
            name: "mid".into(),
            min_usd: 25_000.0,
            max_usd: 100_000.0,
        },
        FootprintBucketDef {
            name: "whale".into(),
            min_usd: 100_000.0,
            max_usd: f64::MAX,
        },
    ]
}

impl Default for FootprintParams {
    fn default() -> Self {
        FootprintParams {
            bar_tf_ns: default_bar_tf(),
            buckets: default_footprint_buckets(),
        }
    }
}

/// Params for the `book.depth.*` feature family — liquidity within `pct` of
/// mid (Cryexc/OpenMarket depth stats, spec 004 §Liquidity). Each band
/// registers `book.depth.{pct}` (gauge) + `book.depth_total.{pct}` (Σ).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookDepthParams {
    /// Fraction-of-mid bands, e.g. 0.005/0.02/0.1 = 0.5%/2%/10%.
    #[serde(default = "default_book_depth_bands")]
    pub bands: Vec<f64>,
}

fn default_book_depth_bands() -> Vec<f64> {
    vec![0.005, 0.02, 0.1]
}

impl Default for BookDepthParams {
    fn default() -> Self {
        Self {
            bands: default_book_depth_bands(),
        }
    }
}

/// Params for the `tape.*` feature family (OpenMarket tape stats, spec 004
/// §Order flow).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TapeParams {
    /// `tape.bps_delta` only emits when |Δ| ≥ this many bps (noise floor;
    /// OpenMarket hides sub-half-bps ticks).
    #[serde(default = "default_tape_min_bps")]
    pub min_bps_delta: f64,
}

fn default_tape_min_bps() -> f64 {
    0.5
}

impl Default for TapeParams {
    fn default() -> Self {
        Self {
            min_bps_delta: default_tape_min_bps(),
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
    pub whale_net: WhaleNetParams,
    #[serde(default)]
    pub liq_cluster: LiqClusterParams,
    #[serde(default)]
    pub footprint: FootprintParams,
    #[serde(default)]
    pub liq_agg: LiqAggParams,
    #[serde(default)]
    pub liq_est_bands: LiqEstBandsParams,
    #[serde(default)]
    pub book_depth: BookDepthParams,
    #[serde(default)]
    pub tape: TapeParams,
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
            whale_net: WhaleNetParams::default(),
            liq_cluster: LiqClusterParams::default(),
            footprint: FootprintParams::default(),
            liq_agg: LiqAggParams::default(),
            liq_est_bands: LiqEstBandsParams::default(),
            book_depth: BookDepthParams::default(),
            tape: TapeParams::default(),
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
        let h = fnv1a_absorb(FNV1A_OFFSET, canonical.as_bytes());
        Ok(format!("{h:016x}"))
    }
}
