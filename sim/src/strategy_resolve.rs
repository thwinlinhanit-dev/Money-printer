//! Strategy-name resolution for the `sim` binaries (SIM-5): maps the CLI
//! strategy string to the SAME production strategy constructors the rest of
//! the workspace uses. Extracted from `sim/src/bin/sim.rs` so the resolution
//! is unit-testable — in particular the PAP-4 kill-latch substitution, where
//! `--zero-intents` resolves the name `"null"` and the session must run but
//! emit zero intents (audit 2026-09-03 A-8: pap_4 used to substitute a
//! hand-built `NullStrategy` instead of exercising this resolver).

use mp_core::EventEnvelope;
use mp_strategies::{
    CarryConfig, CarryV1, CoinFlipStrategy, LiqFadeConfig, LiqFadeV1, NullStrategy,
    OrderflowConfig, OrderflowV1, Strategy, Universe,
};

/// Universe derived from the events a run will replay — venues + symbols seen
/// in the log, capped to a small bound (the strategies only need the venues
/// and symbols they may trade).
pub fn universe_from_events(events: &[EventEnvelope]) -> Universe {
    let mut venues = Vec::new();
    let mut symbols = Vec::new();
    for ev in events {
        if !venues.contains(&ev.venue) {
            venues.push(ev.venue);
        }
        if !symbols.contains(&ev.symbol) {
            symbols.push(ev.symbol);
        }
        if venues.len() > 3 && symbols.len() > 5 {
            break;
        }
    }
    Universe { venues, symbols }
}

/// Resolve a CLI strategy name to its production constructor. `"null"` is
/// the kill-latch target (PAP-4: the binary maps `--zero-intents` to it, so
/// the session still runs — bars built, events consumed — but emits no
/// intents). `entry_threshold`/`exit_threshold` map onto each strategy's
/// config fields (the `sim backtest` carry/orderflow/liq-fade overrides).
pub fn strategy_named(
    name: &str,
    events: &[EventEnvelope],
    entry_threshold: Option<f64>,
    exit_threshold: Option<f64>,
) -> Result<Box<dyn Strategy>, String> {
    match name {
        "coinflip" => Ok(Box::new(CoinFlipStrategy::new())),
        "null" => Ok(Box::new(NullStrategy)),
        "carry-v1" => {
            let uni = universe_from_events(events);
            let mut cfg = CarryConfig::default();
            if let Some(et) = entry_threshold {
                cfg.entry_threshold = et;
            }
            if let Some(xt) = exit_threshold {
                cfg.exit_threshold = xt;
            } else if let Some(et) = entry_threshold {
                cfg.exit_threshold = et * 0.2;
            }
            Ok(Box::new(CarryV1::new(
                mp_core::StrategyId::new("carry-v1"),
                uni,
                cfg,
            )))
        }
        "orderflow-v1" => {
            let uni = universe_from_events(events);
            let mut cfg = OrderflowConfig::default();
            if let Some(et) = entry_threshold {
                cfg.entry_gauge = et;
            }
            if let Some(xt) = exit_threshold {
                cfg.exit_gauge = xt;
            }
            Ok(Box::new(OrderflowV1::new(
                mp_core::StrategyId::new("orderflow-v1"),
                uni,
                cfg,
            )))
        }
        // liq-fade-v1: fade a liquidation cascade after exhaustion (liq.*
        // features). entry/exit thresholds map to entry_dist_bps (stretch
        // floor) and exit_dist_bps (reversion target).
        "liq-fade-v1" => {
            let uni = universe_from_events(events);
            let mut cfg = LiqFadeConfig::default();
            if let Some(et) = entry_threshold {
                cfg.entry_dist_bps = et;
            }
            if let Some(xt) = exit_threshold {
                cfg.exit_dist_bps = xt;
            }
            Ok(Box::new(LiqFadeV1::new(
                mp_core::StrategyId::new("liq-fade-v1"),
                uni,
                cfg,
            )))
        }
        other => Err(format!(
            "unknown strategy: {other} (coinflip|null|carry-v1|orderflow-v1|liq-fade-v1)"
        )),
    }
}
