//! mp-features — streaming feature engine (spec 004).
//!
//! Turns raw events into named, versioned, timestamped features consumed by
//! strategies (live) and research (offline) from the SAME code — the
//! one-code-path pillar. Everything here is a pure function of events: no wall
//! clock, no I/O, no unseeded randomness (PD-3/FEA-2).
//!
//! v1 slice: the engine + bar builder + a representative catalog + screener.
//! The full catalog and offline Parquet materialization are the same pattern,
//! tracked in spec 004 Decisions.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod bar;
pub mod catalog;
pub mod config;
pub mod engine;
pub mod hit_journal;
pub mod leverage;
pub mod liquidation;
pub mod screener;
pub mod signal_catalog;
pub mod whale;

use mp_core::Venue;

pub use bar::{Bar, BarBuilder};
pub use catalog::{BookDepth, BookDepthKind, LiqDist, LiqRate, LiqVol, TapeBpsDelta, TapeTps};
pub use config::{
    BookDepthParams, ConfigError, FeaturesConfig, LiqDeltaParams, LiqFlowParams, TapeParams,
};
pub use engine::{BarFeature, FeatureEngine, FeatureUpdate, Locality, TickFeature};
pub use hit_journal::{HitJournal, HitRecord};
pub use leverage::{calibrate_leverage_weights, tier_leverages, LeverageTierCalibration};
pub use liquidation::{
    band_accuracy, BandAccuracy, BandObservation, LiqAgg, LiqDelta, LiqEstBands, WhaleBandStudy,
};
pub use screener::{Cond, Op, Rule, Screener, ScreenerHit};
pub use whale::{WhaleNet, WhaleNetDelta};

/// Build a [`FeatureEngine`] from a [`FeaturesConfig`] — the one-code-path
/// registration (FEA-4): every family the config can express is registered
/// with its configured params, plus the always-on pure passthroughs
/// (`funding.rate`, `oi.delta`, `imbalance.top`). A live runner and the
/// offline materializer both call this, so recorded features are the live
/// features. Fail-closed (CONV-8): an unknown venue slug in the config is an
/// error, never a silently-skipped feature.
///
/// Bar features without config params (`delta.bar`, `vol.rv`, `breakout`)
/// are not registered here — footprint covers order-flow bar logic through
/// the `[footprint]` section's buckets + `bar_tf_ns`.
pub fn engine_from_config(cfg: &FeaturesConfig) -> Result<FeatureEngine, ConfigError> {
    let mut e = FeatureEngine::new(cfg.bar_tf_ns);
    for slug in &cfg.cvd.venues {
        let venue = Venue::from_slug(slug)
            .ok_or_else(|| ConfigError::Parse(format!("cvd.venues: unknown venue slug '{slug}'")))?;
        e.register_tick(move || Box::new(crate::catalog::Cvd::new(venue)));
    }
    for slug in &cfg.whale_print.venues {
        let venue = Venue::from_slug(slug).ok_or_else(|| {
            ConfigError::Parse(format!("whale_print.venues: unknown venue slug '{slug}'"))
        })?;
        let floor = cfg.whale_print.min_notional;
        e.register_tick(move || Box::new(crate::catalog::WhalePrint::for_venue(floor, venue)));
    }
    for slug in &cfg.whale_net.venues {
        let venue = Venue::from_slug(slug).ok_or_else(|| {
            ConfigError::Parse(format!("whale_net.venues: unknown venue slug '{slug}'"))
        })?;
        let stale = cfg.whale_net.stale_after_ns;
        e.register_tick(move || Box::new(WhaleNet::with_stale_after(venue, stale)));
        e.register_tick(move || Box::new(WhaleNetDelta::with_stale_after(venue, stale)));
    }
    let (wc, mn) = (cfg.liq_cluster.window_ns, cfg.liq_cluster.min_cluster_notional);
    e.register_tick(move || Box::new(crate::catalog::LiqCluster::new(wc, mn)));
    let (dw, aw) = (cfg.liq_agg.dedup_window_ns, cfg.liq_agg.agg_window_ns);
    e.register_tick(move || Box::new(LiqAgg::new(dw, aw)));
    let (mb, tiers) = (
        cfg.liq_est_bands.maintenance_buffer,
        cfg.liq_est_bands.leverage_tiers.clone(),
    );
    // Fn (not FnOnce): called once per symbol, so captured Vec must be cloned
    // inside the closure body.
    e.register_tick(move || Box::new(LiqEstBands::new(mb, tiers.clone())));
    for b in &cfg.footprint.buckets {
        let tf = cfg.footprint.bar_tf_ns;
        let (min_usd, max_usd) = (b.min_usd, b.max_usd);
        let name = b.name.clone();
        e.register_tick(move || {
            Box::new(crate::catalog::FootprintDelta::new(tf, &name, min_usd, max_usd))
        });
        let name = b.name.clone();
        e.register_tick(move || {
            Box::new(crate::catalog::FootprintImbalance::new(tf, &name, min_usd, max_usd))
        });
    }
    // Always-on pure passthroughs (no config params):
    e.register_tick(|| Box::new(crate::catalog::FundingRate::new()));
    e.register_tick(|| Box::new(crate::catalog::OiDelta::new()));
    e.register_tick(|| Box::new(crate::catalog::BookImbalance::new()));
    // Liquidity-band depth stats (Cryexc/OpenMarket): one gauge + one total
    // feature per configured band.
    for &pct in &cfg.book_depth.bands {
        e.register_tick(move || Box::new(BookDepth::new(pct, BookDepthKind::Gauge)));
        e.register_tick(move || Box::new(BookDepth::new(pct, BookDepthKind::Total)));
    }
    // Liquidation flow (COL-29 real liq source): rolling notional by side +
    // rolling event rate over the shared window, plus liq price-distance
    // from mid. No-op on venues without a liquidation stream (hyperliquid
    // today) until one with a native liq source joins the required set.
    let liq_w = cfg.liq_flow.window_ns;
    e.register_tick(move || Box::new(LiqVol::new(liq_w, mp_core::Side::Buy)));
    e.register_tick(move || Box::new(LiqVol::new(liq_w, mp_core::Side::Sell)));
    e.register_tick(move || Box::new(LiqRate::new(liq_w)));
    e.register_tick(|| Box::new(LiqDist::default()));
    // Cross-venue liquidation-pressure divergence: one instance per pair.
    for pair in &cfg.liq_delta.pairs {
        let a = Venue::from_slug(&pair[0]).ok_or_else(|| {
            ConfigError::Parse(format!("liq_delta.pairs: unknown venue slug '{}'", pair[0]))
        })?;
        let b = Venue::from_slug(&pair[1]).ok_or_else(|| {
            ConfigError::Parse(format!("liq_delta.pairs: unknown venue slug '{}'", pair[1]))
        })?;
        if a == b {
            return Err(ConfigError::Parse(format!(
                "liq_delta.pairs: identical venues {a:?} — a divergence needs two distinct venues"
            )));
        }
        let w = cfg.liq_delta.window_ns;
        // Global: ONE instance must see both venues (spec 004 FEA-20 — the
        // per-symbol model cannot hold cross-venue state).
        e.register_global_tick(move || Box::new(LiqDelta::new(w, a, b)));
    }
    // Tape micro-stats: per-trade bps delta (tick) + per-bar TPS (bar).
    let min_bps = cfg.tape.min_bps_delta;
    e.register_tick(move || Box::new(TapeBpsDelta::new(min_bps)));
    let (tf_secs, tf) = (cfg.bar_tf_ns / 1_000_000_000, cfg.bar_tf_ns / 1_000_000_000);
    let tf_secs = tf_secs as f64;
    let tf = format!("{tf}s");
    e.register_bar(move || Box::new(TapeTps::new(&tf, tf_secs)));
    Ok(e)
}
