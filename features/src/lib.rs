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

pub mod accumulation;
pub mod bar;
pub mod catalog;
pub mod cohort;
pub mod config;
pub mod engine;
pub mod hit_journal;
pub mod ibit_cross;
pub mod leverage;
pub mod liquidation;
pub mod netflow_flow;
pub mod oi_regime;
pub mod options_flow;
pub mod options_greeks;
pub mod options_iv;
pub mod screener;
pub mod signal_catalog;
pub mod swing;
pub mod whale;

use mp_core::Venue;

pub use accumulation::{AccumulationConfig, AccumulationDetector, SubSignal};
pub use bar::{Bar, BarBuilder};
pub use catalog::{
    BookDepth, BookDepthKind, LiqDist, LiqRate, LiqVol, Microprice, SpreadBp, SpreadRegime,
    TapeBpsDelta, TapeTps,
};
pub use cohort::{
    journal_changes, load_snapshot, save_snapshot, Cohort, CohortConfig, CohortFeature,
    CohortField, CohortSnapshot, WalletMetrics, WalletScorer,
};
pub use config::{
    BookDepthParams, ConfigError, FeaturesConfig, LiqDeltaParams, LiqFlowParams,
    MicrostructureParams, TapeParams,
};
pub use engine::{BarFeature, FeatureEngine, FeatureUpdate, Locality, TickFeature};
pub use hit_journal::{HitJournal, HitRecord};
pub use ibit_cross::{CrossDayClose, CrossField, IbitCrossDaily, IbitCrossParams, IbitDerivCross};
pub use leverage::{calibrate_leverage_weights, tier_leverages, LeverageTierCalibration};
pub use liquidation::{
    band_accuracy, BandAccuracy, BandObservation, LiqAgg, LiqDelta, LiqEstBands, WhaleBandStudy,
};
pub use oi_regime::{OiLevelDelta, OiQuadrant, OiRegimeConfig, TrendRegime};
pub use options_flow::{
    bs_delta, flow_tenor, moneyness_bucket, window_label, FlowFeature, FlowMetric, FlowParams,
    MoneynessBucket,
};
pub use options_greeks::{
    gex_at, ChainMap, ChainScalar, ChainScalarFeature, ContractKey, GreeksAggregator,
    HigherOrderGreek, TickerSnap,
};
pub use options_iv::{
    vrp, IvAtm, IvIndex, IvPercentileFeature, IvSkew, IvSurfaceAggregator, IvTerm, VolRegime,
};
pub use screener::{Cond, Op, Rule, Screener, ScreenerHit};
pub use swing::{
    atr, compressed_range, realized_vol, sweep_of, trend_strength, value_area, volume_levels,
    AdKind, AdState, LevelKind, LevelSide, PocFlipField, PocSide, RangeField, RollingVwap,
    SweepEvent, SweepField, SweepSide, SwingAd, SwingAtr, SwingClose, SwingNearestLevel,
    SwingPocFlip, SwingRange, SwingRealizedVol, SwingRollingVwap, SwingSweep, SwingTrendStrength,
    SwingValueArea, ValueArea, ValueAreaField, VolumeLevels,
};
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
/// the `[footprint]` section's buckets + `bar_tf_ns`, and the swing family
/// (spec 035 SWG-2) is registered through its own `[swing]` section.
pub fn engine_from_config(cfg: &FeaturesConfig) -> Result<FeatureEngine, ConfigError> {
    let mut e = FeatureEngine::new(cfg.bar_tf_ns);
    for slug in &cfg.cvd.venues {
        let venue = Venue::from_slug(slug).ok_or_else(|| {
            ConfigError::Parse(format!("cvd.venues: unknown venue slug '{slug}'"))
        })?;
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
    let (wc, mn) = (
        cfg.liq_cluster.window_ns,
        cfg.liq_cluster.min_cluster_notional,
    );
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
            Box::new(crate::catalog::FootprintDelta::new(
                tf, &name, min_usd, max_usd,
            ))
        });
        let name = b.name.clone();
        e.register_tick(move || {
            Box::new(crate::catalog::FootprintImbalance::new(
                tf, &name, min_usd, max_usd,
            ))
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
    // Book microstructure (research-driven edge): microprice, quoted spread
    // in bps, and the wide-spread regime gate — one instance per configured
    // venue (book-based; no-op on venues without a book stream).
    let wide_bps = cfg.microstructure.wide_spread_bps;
    for slug in &cfg.microstructure.venues {
        let venue = Venue::from_slug(slug).ok_or_else(|| {
            ConfigError::Parse(format!(
                "microstructure.venues: unknown venue slug '{slug}'"
            ))
        })?;
        e.register_tick(move || Box::new(Microprice::for_venue(venue)));
        e.register_tick(move || Box::new(SpreadBp::for_venue(venue)));
        e.register_tick(move || Box::new(SpreadRegime::for_venue(venue, wide_bps)));
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
    // Swing HTF regime/structure (spec 035 SWG-2): bar-only realized vol,
    // signed trend strength, value area (POC/high/low) and rolling VWAP —
    // none depend on L2 order book or trade-tape (SWG-2 MUST NOT require
    // tick inputs). Windows are in bar counts; each feature warms per FEA-3.
    let rv_window = cfg.swing.realized_vol_window;
    let sqrt_bars_per_year = cfg.swing.sqrt_bars_per_year.unwrap_or_else(|| {
        // Derive from bar_tf_ns: bars/year = 365.25d × 86400s / tf_secs.
        (31_557_600.0 / (cfg.bar_tf_ns as f64 / 1_000_000_000.0)).sqrt()
    });
    e.register_bar(move || Box::new(SwingRealizedVol::new(rv_window, sqrt_bars_per_year)));
    let trend_lookback = cfg.swing.trend_lookback;
    e.register_bar(move || Box::new(SwingTrendStrength::new(trend_lookback)));
    let (va_window, va_bucket) = (cfg.swing.value_area_window, cfg.swing.value_area_bucket);
    e.register_bar(move || {
        Box::new(SwingValueArea::new(
            va_window,
            va_bucket,
            ValueAreaField::Poc,
        ))
    });
    e.register_bar(move || {
        Box::new(SwingValueArea::new(
            va_window,
            va_bucket,
            ValueAreaField::High,
        ))
    });
    e.register_bar(move || {
        Box::new(SwingValueArea::new(
            va_window,
            va_bucket,
            ValueAreaField::Low,
        ))
    });
    let rv_bars = cfg.swing.rolling_vwap_bars;
    e.register_bar(move || Box::new(SwingRollingVwap::new(rv_bars)));
    // Liquidity-structure family (spec 036 SLQ): ATR, compressed-range
    // boundaries, the sweep-reclaim event pair (extreme + companion stop,
    // per side), and nearest HVN/LVN levels — all bar-only (SWG-2).
    let atr_n = cfg.swing.sweep_atr_n;
    e.register_bar(move || Box::new(SwingAtr::new(atr_n)));
    e.register_bar(|| Box::new(crate::swing::SwingClose));
    let (range_n, compress_frac) = (cfg.swing.sweep_range_n, cfg.swing.sweep_compress_frac);
    e.register_bar(move || {
        Box::new(SwingRange::new(
            range_n,
            compress_frac,
            atr_n,
            RangeField::High,
        ))
    });
    e.register_bar(move || {
        Box::new(SwingRange::new(
            range_n,
            compress_frac,
            atr_n,
            RangeField::Low,
        ))
    });
    let (atr_mult, reclaim_z, vol_mult, stop_buf) = (
        cfg.swing.sweep_atr_mult,
        cfg.swing.sweep_reclaim_z,
        cfg.swing.sweep_vol_mult,
        cfg.swing.sweep_stop_buffer_atr,
    );
    for side in [Some(SweepSide::Low), Some(SweepSide::High)] {
        e.register_bar(move || {
            Box::new(SwingSweep::new(
                range_n,
                compress_frac,
                atr_n,
                atr_mult,
                reclaim_z,
                vol_mult,
                stop_buf,
                side,
                SweepField::Extreme,
            ))
        });
        e.register_bar(move || {
            Box::new(SwingSweep::new(
                range_n,
                compress_frac,
                atr_n,
                atr_mult,
                reclaim_z,
                vol_mult,
                stop_buf,
                side,
                SweepField::Stop,
            ))
        });
    }
    let (prof_win, prof_bucket, hvn_frac, lvn_frac) = (
        cfg.swing.profile_window,
        cfg.swing.value_area_bucket,
        cfg.swing.profile_hvn_frac,
        cfg.swing.profile_lvn_frac,
    );
    for kind in [LevelKind::Hvn, LevelKind::Lvn] {
        for side in [LevelSide::Above, LevelSide::Below] {
            e.register_bar(move || {
                Box::new(SwingNearestLevel::new(
                    prof_win,
                    prof_bucket,
                    hvn_frac,
                    lvn_frac,
                    kind,
                    side,
                ))
            });
        }
    }
    // Spec 036 §7: POC-flip state machine + A/D classifier.
    let poc_n = cfg.swing.poc_flip_n;
    for field in [PocFlipField::Up, PocFlipField::Down] {
        e.register_bar(move || Box::new(SwingPocFlip::new(prof_win, prof_bucket, poc_n, field)));
    }
    let ad_k = cfg.swing.ad_k_windows;
    let ad_atr_n = cfg.swing.sweep_atr_n;
    for kind in [AdKind::Accumulating, AdKind::Distributing] {
        e.register_bar(move || {
            Box::new(SwingAd::new(
                prof_win,
                prof_bucket,
                hvn_frac,
                lvn_frac,
                ad_k,
                ad_atr_n,
                kind,
            ))
        });
    }
    register_options_families(&mut e, cfg)?;
    // Spec 045 sub-signal inputs — oi.delta.{w}, oi.quadrant.{w} and
    // regime.trend, the feature families the accumulation detector consumes.
    // Disabled when `[oi_regime] enabled` is false (the default).
    if cfg.oi_regime.enabled {
        use crate::oi_regime::{OiLevelDelta, OiQuadrant, TrendRegime};
        let inner = &cfg.oi_regime.inner;
        for &w in &inner.windows_ns {
            e.register_tick(move || Box::new(OiLevelDelta::new(w)));
            e.register_tick(move || Box::new(OiQuadrant::new(w)));
        }
        let (lookback, threshold) = (inner.trend_lookback_bars, inner.trend_threshold);
        e.register_bar(move || Box::new(TrendRegime::new(lookback, threshold)));
    }
    // Spec 042 — wallet cohort grading (WCG-7): FOUR global tick families
    // computed from a pre-scored weekly snapshot. The snapshot loads once at
    // build time from `[cohort].snapshot_path` when set; absent/None ⇒ the
    // fail-closed suppression path (WCG-8) until an operator provides one.
    if cfg.cohort.enabled {
        use crate::cohort::{Cohort, CohortField};
        let inner = &cfg.cohort.inner;
        let max_age = inner.snapshot_max_age_ns;
        let snap = inner
            .snapshot_path
            .as_deref()
            .and_then(|p| crate::cohort::load_snapshot(std::path::Path::new(p)));
        let flow_w = inner.smart_flow_window_ns;
        let snap_wr = snap.clone();
        e.register_global_tick(move || {
            Box::new(crate::cohort::CohortFeature::with_windows(
                snap_wr.clone(),
                CohortField::WhaleRatio,
                max_age,
                flow_w,
            ))
        });
        for c in [
            Cohort::SmartMoney,
            Cohort::Whale,
            Cohort::Retail,
            Cohort::Dormant,
        ] {
            let snap_nd = snap.clone();
            e.register_global_tick(move || {
                Box::new(crate::cohort::CohortFeature::with_windows(
                    snap_nd.clone(),
                    CohortField::NetDelta(c),
                    max_age,
                    flow_w,
                ))
            });
        }
        let snap_sf = snap.clone();
        e.register_global_tick(move || {
            Box::new(crate::cohort::CohortFeature::with_windows(
                snap_sf.clone(),
                CohortField::SmartFlow,
                max_age,
                flow_w,
            ))
        });
        let snap_hh = snap.clone();
        e.register_global_tick(move || {
            Box::new(crate::cohort::CohortFeature::with_windows(
                snap_hh.clone(),
                CohortField::Concentration,
                max_age,
                flow_w,
            ))
        });
    }
    // Spec 043 — CEX flow velocity: per-field TickFeature instances
    // consuming NetflowSnapshot events. Enabled when `netflow_flow.enabled`.
    if cfg.netflow_flow.enabled {
        use crate::netflow_flow::{NetflowField, NetflowFlowFeature};
        let nf_cfg = cfg.netflow_flow.inner.clone();
        for (i, _) in nf_cfg.windows_ns.iter().enumerate() {
            let c = nf_cfg.clone();
            e.register_tick(move || {
                Box::new(NetflowFlowFeature::new(
                    c.clone(),
                    NetflowField::Velocity(i),
                ))
            });
            let c = nf_cfg.clone();
            e.register_tick(move || {
                Box::new(NetflowFlowFeature::new(
                    c.clone(),
                    NetflowField::Acceleration(i),
                ))
            });
        }
        {
            let c = nf_cfg.clone();
            e.register_tick(move || {
                Box::new(NetflowFlowFeature::new(c.clone(), NetflowField::Cumulative))
            });
        }
        {
            let c = nf_cfg.clone();
            e.register_tick(move || {
                Box::new(NetflowFlowFeature::new(c.clone(), NetflowField::Regime))
            });
        }
        {
            let c = nf_cfg.clone();
            e.register_tick(move || {
                Box::new(NetflowFlowFeature::new(c.clone(), NetflowField::ZScore))
            });
        }
    }
    // IBIT ↔ Deribit cross-market family (spec 040 IBI-5/6): global tick
    // features, enabled when `deriv_underlying` is set. Four instances —
    // divergence + flow correlations at lags 0..2 — each keeping identical
    // independent state (determinism by construction).
    if !cfg.ibit_cross.deriv_underlying.is_empty() {
        let p = cfg.ibit_cross.clone();
        e.register_global_tick(move || {
            Box::new(IbitDerivCross::new(p.clone(), CrossField::IvDivergence))
        });
        for lag in [0u8, 1, 2] {
            let p = cfg.ibit_cross.clone();
            e.register_global_tick(move || {
                Box::new(IbitDerivCross::new(p.clone(), CrossField::FlowCorr(lag)))
            });
        }
    }
    Ok(e)
}

/// Options analytics families (specs 037/038/039): global tick features
/// (FEA-20 — chain aggregation spans every option symbol of an underlying).
/// Disabled when the family's `underlyings` list is empty (the default), so
/// enabling is an explicit config decision. Public so tests and offline
/// runners share the exact registration path (one-code-path, FEA-4).
pub fn register_options_families(
    e: &mut FeatureEngine,
    cfg: &FeaturesConfig,
) -> Result<(), ConfigError> {
    use crate::options_flow::{FlowFeature, FlowMetric, FlowParams};
    use crate::options_greeks::{ChainScalar, ChainScalarFeature, HigherOrderGreek};
    use crate::options_iv::{IvAtm, IvIndex, IvPercentileFeature, IvSkew, IvTerm};

    // Spec 037 — chain-level Greeks scalars + FD higher-order Greeks.
    if !cfg.options_greeks.underlyings.is_empty() {
        let mult = cfg.options_greeks.contract_multiplier;
        if !mult.is_finite() || mult <= 0.0 {
            return Err(ConfigError::Parse(
                "options_greeks.contract_multiplier must be finite > 0".into(),
            ));
        }
        for u in &cfg.options_greeks.underlyings {
            let un = u.to_ascii_lowercase();
            for kind in [
                ChainScalar::GexNet,
                ChainScalar::MaxPain,
                ChainScalar::NetDelta,
                ChainScalar::NetVega,
                ChainScalar::NetTheta,
            ] {
                let un = un.clone();
                e.register_global_tick(move || Box::new(ChainScalarFeature::new(kind, &un, mult)));
            }
            for maker in [
                HigherOrderGreek::vanna as fn(&str, f64) -> _,
                HigherOrderGreek::volga,
                HigherOrderGreek::charm,
            ] {
                let un = un.clone();
                e.register_global_tick(move || Box::new(maker(&un, mult)));
            }
        }
    }

    // Spec 038 — IV surface analytics.
    if !cfg.options_iv.underlyings.is_empty() {
        for u in &cfg.options_iv.underlyings {
            let un = u.to_ascii_lowercase();
            let un_atm = un.clone();
            e.register_global_tick(move || Box::new(IvAtm::new(&un_atm)));
            let un_idx = un.clone();
            e.register_global_tick(move || Box::new(IvIndex::new(&un_idx)));
            let un_rr = un.clone();
            e.register_global_tick(move || Box::new(IvSkew::risk_reversal(&un_rr)));
            let un_wing = un.clone();
            e.register_global_tick(move || Box::new(IvSkew::wing_richness(&un_wing)));
            for t in &cfg.options_iv.tenors {
                let (un, name, days) = (un.clone(), t.name.clone(), t.days);
                e.register_global_tick(move || Box::new(IvTerm::new(&un, &name, days)));
            }
            let (un_pct, win_pct) = (un.clone(), cfg.options_iv.percentile_window_days);
            e.register_global_tick(move || {
                Box::new(IvPercentileFeature::percentile(&un_pct, win_pct))
            });
            let (un_reg, win_reg) = (un, cfg.options_iv.regime_lookback_days);
            e.register_global_tick(move || Box::new(IvPercentileFeature::regime(&un_reg, win_reg)));
        }
    }

    // Spec 039 — windowed flow metrics (emit on window close).
    if !cfg.options_flow.underlyings.is_empty() {
        if cfg.options_flow.windows_ns.is_empty() {
            return Err(ConfigError::Parse(
                "options_flow.windows_ns must not be empty when underlyings are set".into(),
            ));
        }
        let p = FlowParams {
            contract_multiplier: cfg.options_flow.contract_multiplier,
            block_threshold_usd: cfg.options_flow.block_threshold_usd,
            whale_floor_usd: cfg.options_flow.whale_floor_usd,
            k_whale: cfg.options_flow.k_whale,
            atm_threshold: cfg.options_flow.atm_threshold,
            itm_cap: cfg.options_flow.itm_cap,
            otm_cap: cfg.options_flow.otm_cap,
            tenor_bounds: [
                cfg.options_flow.weekly_max_days,
                cfg.options_flow.monthly_max_days,
                cfg.options_flow.quarterly_max_days,
            ],
        };
        for u in &cfg.options_flow.underlyings {
            let un = u.to_ascii_lowercase();
            for &w in &cfg.options_flow.windows_ns {
                if w <= 0 {
                    return Err(ConfigError::Parse(
                        "options_flow.windows_ns entries must be > 0".into(),
                    ));
                }
                for metric in [
                    FlowMetric::NetPremium,
                    FlowMetric::Block,
                    FlowMetric::NetDelta,
                    FlowMetric::ItmFlow,
                    FlowMetric::AtmFlow,
                    FlowMetric::OtmFlow,
                    FlowMetric::CallPutRatio,
                    FlowMetric::WeeklyFlow,
                    FlowMetric::MonthlyFlow,
                    FlowMetric::QuarterlyFlow,
                    FlowMetric::LeapFlow,
                    FlowMetric::WhaleCount,
                    FlowMetric::WhaleNet,
                    FlowMetric::Acceleration,
                    FlowMetric::CrossVenueDivergence,
                ] {
                    let (un, p) = (un.clone(), p.clone());
                    e.register_global_tick(move || {
                        Box::new(FlowFeature::new(metric, &un, w, p.clone()))
                    });
                }
            }
        }
    }
    Ok(())
}
