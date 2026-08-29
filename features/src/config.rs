//! Catalog configuration (FEA-7): every feature's params live in one
//! `features.toml`, parsed with `deny_unknown_fields` (a typo'd key is an
//! error, not a silent default), and hashed into materialization metadata so a
//! params change forces a new `ver=N` feature-store directory (FEA-6).
//!
//! Pure: parses from a `&str` the caller read at the binary edge — no I/O and
//! no wall clock here (PD-3).

use mp_core::{fnv1a_absorb, FNV1A_OFFSET};
use serde::{Deserialize, Serialize};

use crate::accumulation::AccumulationConfig;
use crate::climax_variants::{V1Config, V2Config, V3Config, V4Config, V5Config, V6Config};
use crate::cohort::CohortConfig;
use crate::ibit_cross::IbitCrossParams;
use crate::netflow_flow::NetflowFlowConfig as NetflowFlowConfigInner;

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

/// Params for the microstructure family (`microprice.{venue}`,
/// `spread.bp.{venue}`, `spread.regime.{venue}`): top-of-book microprice,
/// quoted spread in bps, and the wide-spread regime gate. Book-based, so
/// only venues with a book stream make sense (default: hyperliquid).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MicrostructureParams {
    /// Venues to compute the book microstructure features for.
    #[serde(default = "default_whale_venues")]
    pub venues: Vec<String>,
    /// `spread.regime.{venue}` = 1 when the quoted spread is ≥ this many bps
    /// of mid (wide-spread regimes destroy short-horizon predictability).
    #[serde(default = "default_wide_spread_bps")]
    pub wide_spread_bps: f64,
}

fn default_wide_spread_bps() -> f64 {
    2.0
}

impl Default for MicrostructureParams {
    fn default() -> Self {
        Self {
            venues: default_whale_venues(),
            wide_spread_bps: default_wide_spread_bps(),
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

/// Params for the `liq.*` liquidation-flow family (COL-29 real liq source,
/// spec 004 §Liquidation flow): `liq.vol_buy`/`liq.vol_sell` (rolling
/// notional by side), `liq.rate` (rolling event rate) share one rolling
/// window; `liq.dist` (liq price distance from mid, bps) needs no params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiqFlowParams {
    /// Rolling window (ns) for `liq.vol_*` and `liq.rate`.
    #[serde(default = "default_liq_flow_window_ns")]
    pub window_ns: i64,
}

fn default_liq_flow_window_ns() -> i64 {
    300_000_000_000 // 5 minutes
}

impl Default for LiqFlowParams {
    fn default() -> Self {
        Self {
            window_ns: default_liq_flow_window_ns(),
        }
    }
}

/// Params for `liq.delta.{a}_{b}` — cross-venue liquidation-pressure
/// divergence (spec 004 §Liquidation flow, COL-29 cascade detection):
/// `(Σbuy_a − Σsell_a) − (Σbuy_b − Σsell_b)` over a rolling window, one
/// feature instance per configured venue pair. Venue-level (all symbols per
/// venue) — the merged stream has no cross-venue symbol identity (EVT-8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiqDeltaParams {
    /// Rolling window (ns) for the per-venue buy/sell sums.
    #[serde(default = "default_liq_delta_window_ns")]
    pub window_ns: i64,
    /// Venue pairs, e.g. [["bybit", "okx"], ["binance", "bybit"]]. Empty =
    /// no `liq.delta.*` features registered (fail-closed on unknown slugs).
    #[serde(default)]
    pub pairs: Vec<[String; 2]>,
}

fn default_liq_delta_window_ns() -> i64 {
    300_000_000_000 // 5 minutes
}

impl Default for LiqDeltaParams {
    fn default() -> Self {
        Self {
            window_ns: default_liq_delta_window_ns(),
            pairs: Vec::new(),
        }
    }
}

/// Params for the `swing.*` bar-aggregated family (spec 035 SWG-2): HTF
/// regime + value area + structural levels computed from BARS only — never
/// order book or trade-tape (SWG-2 MUST NOT require tick inputs). Each
/// feature runs on the engine's bar timeframe; windows are in bar counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwingParams {
    /// Trailing window (bars) over which `swing.realized_vol` is computed.
    #[serde(default = "default_swing_rv_window")]
    pub realized_vol_window: usize,
    /// Annualization factor (sqrt of bars/year) for `swing.realized_vol`.
    /// Default derives from `bar_tf_ns` (525,960 one-minute bars/yr → ≈725);
    /// override to align with the materialized bar timeframe.
    #[serde(default)]
    pub sqrt_bars_per_year: Option<f64>,
    /// Lookback (bars) for `swing.trend_strength` (signed HTF trend).
    #[serde(default = "default_swing_trend_lookback")]
    pub trend_lookback: usize,
    /// Window (bars) the `swing.value_area.*` levels are computed over.
    #[serde(default = "default_swing_va_window")]
    pub value_area_window: usize,
    /// Bucket width (price units) for `swing.value_area.*` (POC / VA high/low).
    #[serde(default = "default_swing_va_bucket")]
    pub value_area_bucket: f64,
    /// Window (bars) for `swing.rolling_vwap` (structural VWAP band).
    #[serde(default = "default_swing_vwap_bars")]
    pub rolling_vwap_bars: usize,
    /// Range window (bars) for `swing.range.*` / `swing.sweep.*` (spec 036 SLQ-R).
    #[serde(default = "default_swq_range_n")]
    pub sweep_range_n: usize,
    /// Compression gate: mean TR of the range window must be below this
    /// fraction of the pre-window baseline ATR.
    #[serde(default = "default_swq_compress_frac")]
    pub sweep_compress_frac: f64,
    /// Baseline ATR length (bars) computed BEFORE the range window.
    #[serde(default = "default_swq_atr_n")]
    pub sweep_atr_n: usize,
    /// Sweep wick threshold: boundary violation ≥ this multiple of baseline ATR.
    #[serde(default = "default_swq_atr_mult")]
    pub sweep_atr_mult: f64,
    /// Reclaim confirmation: closes-back-inside allowed within this many
    /// subsequent bars (offset-0 same-bar confirm included by construction).
    #[serde(default = "default_swq_reclaim_z")]
    pub sweep_reclaim_z: u32,
    /// Sweep-bar volume filter: volume ≥ this multiple of the range-window mean.
    #[serde(default = "default_swq_vol_mult")]
    pub sweep_vol_mult: f64,
    /// Invalidation buffer carried on `swing.sweep.*.stop.*`: stop =
    /// extreme ∓ this multiple of ATR. Feature-family config, never a
    /// strategy parameter (spec 036 §2.3).
    #[serde(default = "default_swq_stop_buffer")]
    pub sweep_stop_buffer_atr: f64,
    /// Profile window (bars) for HVN/LVN nearest levels (SLQ-V).
    #[serde(default = "default_swq_profile_window")]
    pub profile_window: usize,
    /// HVN threshold: local maxima above this fraction of POC volume.
    #[serde(default = "default_swq_hvn_frac")]
    pub profile_hvn_frac: f64,
    /// LVN threshold: local minima below this fraction of POC volume.
    #[serde(default = "default_swq_lvn_frac")]
    pub profile_lvn_frac: f64,
    /// POC-flip acceptance window (closes): a flip confirms when the trailing
    /// N closes contain ≥ N−1 on the far side of the current POC and the
    /// latest close is on the far side (≤1 close back across within the
    /// window). Spec 036 §2.3/§7.
    #[serde(default = "default_swq_poc_flip_n")]
    pub poc_flip_n: usize,
    /// A/D classifier windows: near-VAL/near-VAH volume ratio compares the
    /// last K bars vs the previous K; compression compares ATR(atr_n) now vs
    /// K bars ago. Spec 036 §2.4/§7.
    #[serde(default = "default_swq_ad_k")]
    pub ad_k_windows: usize,
}

fn default_swing_rv_window() -> usize {
    20
}
fn default_swing_trend_lookback() -> usize {
    20
}
fn default_swing_va_window() -> usize {
    48
}
fn default_swing_va_bucket() -> f64 {
    50.0
}
fn default_swing_vwap_bars() -> usize {
    48
}
fn default_swq_range_n() -> usize {
    20
}
fn default_swq_compress_frac() -> f64 {
    0.6
}
fn default_swq_atr_n() -> usize {
    20
}
fn default_swq_atr_mult() -> f64 {
    0.1
}
fn default_swq_reclaim_z() -> u32 {
    2
}
fn default_swq_vol_mult() -> f64 {
    1.5
}
fn default_swq_stop_buffer() -> f64 {
    0.5
}
fn default_swq_profile_window() -> usize {
    90
}
fn default_swq_hvn_frac() -> f64 {
    0.7
}
fn default_swq_lvn_frac() -> f64 {
    0.3
}
fn default_swq_poc_flip_n() -> usize {
    3
}
fn default_swq_ad_k() -> usize {
    3
}

impl Default for SwingParams {
    fn default() -> Self {
        Self {
            realized_vol_window: default_swing_rv_window(),
            sqrt_bars_per_year: None,
            trend_lookback: default_swing_trend_lookback(),
            value_area_window: default_swing_va_window(),
            value_area_bucket: default_swing_va_bucket(),
            rolling_vwap_bars: default_swing_vwap_bars(),
            sweep_range_n: default_swq_range_n(),
            sweep_compress_frac: default_swq_compress_frac(),
            sweep_atr_n: default_swq_atr_n(),
            sweep_atr_mult: default_swq_atr_mult(),
            sweep_reclaim_z: default_swq_reclaim_z(),
            sweep_vol_mult: default_swq_vol_mult(),
            sweep_stop_buffer_atr: default_swq_stop_buffer(),
            profile_window: default_swq_profile_window(),
            profile_hvn_frac: default_swq_hvn_frac(),
            profile_lvn_frac: default_swq_lvn_frac(),
            poc_flip_n: default_swq_poc_flip_n(),
            ad_k_windows: default_swq_ad_k(),
        }
    }
}

/// The whole catalog config (FEA-7). One file, all params, no unknown keys.
/// One IV term-structure target tenor (spec 038 IVS-3). `days` is the target;
/// emission falls back to the nearest listed expiry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenorDef {
    /// Embedded in the feature id (`iv.term.{u}.{name}`), e.g. `1m`.
    pub name: String,
    /// Target tenor length in days.
    pub days: f64,
}

fn default_iv_tenors() -> Vec<TenorDef> {
    vec![
        TenorDef {
            name: "1w".into(),
            days: 7.0,
        },
        TenorDef {
            name: "1m".into(),
            days: 30.0,
        },
        TenorDef {
            name: "3m".into(),
            days: 90.0,
        },
        TenorDef {
            name: "6m".into(),
            days: 180.0,
        },
    ]
}

fn default_opt_multiplier() -> f64 {
    1.0 // Deribit: 1 BTC / 1 ETH per contract (IBIT would be 100 — spec 040)
}

/// Params for the options Greeks family (spec 037 GRE). Disabled when
/// `underlyings` is empty (the default) — these are global tick features fed
/// every event, so they must be explicitly enabled per underlying.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsGreeksParams {
    #[serde(default)]
    pub underlyings: Vec<String>,
    /// Contract multiplier (GRE: Deribit 1.0; spec 040 sets 100 for IBIT).
    #[serde(default = "default_opt_multiplier")]
    pub contract_multiplier: f64,
}

impl Default for OptionsGreeksParams {
    fn default() -> Self {
        OptionsGreeksParams {
            underlyings: Vec::new(),
            contract_multiplier: default_opt_multiplier(),
        }
    }
}

/// Params for the IV surface family (spec 038 IVS).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsIvParams {
    #[serde(default)]
    pub underlyings: Vec<String>,
    /// Term-structure tenor targets (IVS-3).
    #[serde(default = "default_iv_tenors")]
    pub tenors: Vec<TenorDef>,
    /// Rolling percentile window for `iv.percentile.*` (IVS-11 gap handling:
    /// only observed days count). Default 365.
    #[serde(default = "default_percentile_days")]
    pub percentile_window_days: i64,
    /// Regime classifier lookback (IVS-6). Default 90.
    #[serde(default = "default_regime_days")]
    pub regime_lookback_days: i64,
}

fn default_percentile_days() -> i64 {
    365
}
fn default_regime_days() -> i64 {
    90
}

impl Default for OptionsIvParams {
    fn default() -> Self {
        OptionsIvParams {
            underlyings: Vec::new(),
            tenors: default_iv_tenors(),
            percentile_window_days: default_percentile_days(),
            regime_lookback_days: default_regime_days(),
        }
    }
}

/// Params for the options flow aggregator family (spec 039 OFI). Defaults per
/// spec; USD thresholds normalize BTC vs ETH vs IBIT contract sizes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsFlowParams {
    #[serde(default)]
    pub underlyings: Vec<String>,
    /// Aggregation windows in ns (features emit on window close).
    #[serde(default = "default_flow_windows_ns")]
    pub windows_ns: Vec<i64>,
    #[serde(default = "default_opt_multiplier")]
    pub contract_multiplier: f64,
    /// Block trade threshold in USD notional (OFI-2). Default $100k.
    #[serde(default = "default_block_threshold")]
    pub block_threshold_usd: f64,
    /// Whale floor (OFI-8). Default $500k.
    #[serde(default = "default_whale_floor")]
    pub whale_floor_usd: f64,
    /// Whale p95 multiplier (OFI-8). Default 3.0.
    #[serde(default = "default_k_whale")]
    pub k_whale: f64,
    /// ±band around spot counted as ATM (OFI-5). Default 0.02.
    #[serde(default = "default_atm_threshold")]
    pub atm_threshold: f64,
    /// ITM distance cap beyond ATM band (OFI-5, kind-aware fix). Default 0.10.
    #[serde(default = "default_itm_cap")]
    pub itm_cap: f64,
    /// OTM distance cap — deeper trades excluded (OFI-5). Default 0.50.
    #[serde(default = "default_otm_cap")]
    pub otm_cap: f64,
    /// Tenor boundaries in days: ≤7 weekly, ≤45 monthly, ≤180 quarterly
    /// (OFI-6).
    #[serde(default = "default_weekly_max")]
    pub weekly_max_days: f64,
    #[serde(default = "default_monthly_max")]
    pub monthly_max_days: f64,
    #[serde(default = "default_quarterly_max")]
    pub quarterly_max_days: f64,
}

fn default_flow_windows_ns() -> Vec<i64> {
    vec![3_600_000_000_000, 14_400_000_000_000, 86_400_000_000_000]
}
fn default_block_threshold() -> f64 {
    100_000.0
}
fn default_whale_floor() -> f64 {
    500_000.0
}
fn default_k_whale() -> f64 {
    3.0
}
fn default_atm_threshold() -> f64 {
    0.02
}
fn default_itm_cap() -> f64 {
    0.10
}
fn default_otm_cap() -> f64 {
    0.50
}
fn default_weekly_max() -> f64 {
    7.0
}
fn default_monthly_max() -> f64 {
    45.0
}
fn default_quarterly_max() -> f64 {
    180.0
}

impl Default for OptionsFlowParams {
    fn default() -> Self {
        OptionsFlowParams {
            underlyings: Vec::new(),
            windows_ns: default_flow_windows_ns(),
            contract_multiplier: default_opt_multiplier(),
            block_threshold_usd: default_block_threshold(),
            whale_floor_usd: default_whale_floor(),
            k_whale: default_k_whale(),
            atm_threshold: default_atm_threshold(),
            itm_cap: default_itm_cap(),
            otm_cap: default_otm_cap(),
            weekly_max_days: default_weekly_max(),
            monthly_max_days: default_monthly_max(),
            quarterly_max_days: default_quarterly_max(),
        }
    }
}

/// Wrapper for spec 042 cohort grading params.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortParams {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub inner: CohortConfig,
}

/// Wrapper for spec 043 netflow flow-velocity params.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetflowFlowParams {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub inner: NetflowFlowConfigInner,
}

/// Wrapper for the accumulation-detector OI/price regime inputs (spec 045
/// sub-signal features: `oi.delta.{w}`, `oi.quadrant.{w}`, `regime.trend`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OiRegimeParams {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub inner: crate::oi_regime::OiRegimeConfig,
}

/// Wrapper for spec 045 accumulation detector params.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccumulationParams {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub inner: AccumulationConfig,
}

/// Params for the climax variant pattern family (6 non-standard exhaustion/
/// expansion signals from BTC daily analysis 2017-2026).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClimaxVariantsParams {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_v1")]
    pub v1: V1Config,
    #[serde(default = "default_v2")]
    pub v2: V2Config,
    #[serde(default = "default_v3")]
    pub v3: V3Config,
    #[serde(default = "default_v4")]
    pub v4: V4Config,
    #[serde(default = "default_v5")]
    pub v5: V5Config,
    #[serde(default = "default_v6")]
    pub v6: V6Config,
}

fn default_v1() -> V1Config { V1Config::default() }
fn default_v2() -> V2Config { V2Config::default() }
fn default_v3() -> V3Config { V3Config::default() }
fn default_v4() -> V4Config { V4Config::default() }
fn default_v5() -> V5Config { V5Config::default() }
fn default_v6() -> V6Config { V6Config::default() }

impl Default for ClimaxVariantsParams {
    fn default() -> Self {
        Self {
            enabled: false,
            v1: V1Config::default(),
            v2: V2Config::default(),
            v3: V3Config::default(),
            v4: V4Config::default(),
            v5: V5Config::default(),
            v6: V6Config::default(),
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
    pub microstructure: MicrostructureParams,
    #[serde(default)]
    pub tape: TapeParams,
    #[serde(default)]
    pub liq_flow: LiqFlowParams,
    #[serde(default)]
    pub liq_delta: LiqDeltaParams,
    #[serde(default)]
    pub swing: SwingParams,
    /// Options Greeks family (spec 037) — disabled when `underlyings` empty.
    #[serde(default)]
    pub options_greeks: OptionsGreeksParams,
    /// IV surface family (spec 038) — disabled when `underlyings` empty.
    #[serde(default)]
    pub options_iv: OptionsIvParams,
    /// Options flow family (spec 039) — disabled when `underlyings` empty.
    #[serde(default)]
    pub options_flow: OptionsFlowParams,
    /// IBIT ↔ Deribit cross-market family (spec 040) — disabled when
    /// `deriv_underlying` empty.
    #[serde(default)]
    pub ibit_cross: IbitCrossParams,
    /// Wallet cohort grading (spec 042) — disabled when `enabled` is false.
    #[serde(default)]
    pub cohort: CohortParams,
    /// CEX flow velocity (spec 043) — disabled when `enabled` is false.
    #[serde(default)]
    pub netflow_flow: NetflowFlowParams,
    /// OI/price regime inputs for the accumulation detector (spec 045) —
    /// disabled when `enabled` is false.
    #[serde(default)]
    pub oi_regime: OiRegimeParams,
    /// Accumulation detector (spec 045) — disabled when `enabled` is false.
    #[serde(default)]
    pub accumulation: AccumulationParams,
    /// Cross-asset correlation family (spec 048) — disabled when `enabled`
    /// is false.
    #[serde(default)]
    pub corr: crate::corr::CorrConfig,
    /// Climax variant patterns (6 non-standard exhaustion/expansion signals
    /// discovered from BTC daily analysis 2017–2026).
    #[serde(default)]
    pub climax_variants: ClimaxVariantsParams,
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
            microstructure: MicrostructureParams::default(),
            tape: TapeParams::default(),
            liq_flow: LiqFlowParams::default(),
            liq_delta: LiqDeltaParams::default(),
            swing: SwingParams::default(),
            options_greeks: OptionsGreeksParams::default(),
            options_iv: OptionsIvParams::default(),
            options_flow: OptionsFlowParams::default(),
            ibit_cross: IbitCrossParams::default(),
            cohort: CohortParams::default(),
            netflow_flow: NetflowFlowParams::default(),
            oi_regime: OiRegimeParams::default(),
            accumulation: AccumulationParams::default(),
            corr: crate::corr::CorrConfig::default(),
            climax_variants: ClimaxVariantsParams::default(),
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
