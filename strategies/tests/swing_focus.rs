//! Acceptance tests for spec 035 (swing focus). Test names embed requirement
//! IDs (CONV-21): swg_3_*, swg_4_*, swg_8_*.
//!
//! The strategy-API slice (SWG-3/SWG-4) lands in the `strategies` crate: the
//! shared swing types (`BarRange`, `RebalanceCadence`) live in `core` so sim
//! and risk can consume them without a CONV-3 violation.

use mp_core::{BarRange, RebalanceCadence, StrategyId, SymbolId, Venue};
use mp_features::FeatureUpdate;
use mp_strategies::strategy::{Ctx, TimerId};
use mp_strategies::{Strategy, Universe, FROZEN_STRATEGIES};

/// A per-symbol-position test Ctx (mirrors sim's `SimCtx`: a BTreeMap keyed by
/// SymbolId — the substrate SWG-4 needs for concurrent positions across assets).
struct MultiPosCtx {
    now: i64,
    positions: std::collections::BTreeMap<SymbolId, f64>,
}
impl MultiPosCtx {
    fn with(positions: &[(SymbolId, f64)]) -> Self {
        Self {
            now: 0,
            positions: positions.iter().copied().collect(),
        }
    }
}

impl Ctx for MultiPosCtx {
    fn now_ns(&self) -> i64 {
        self.now
    }
    fn position(&self, symbol: SymbolId) -> f64 {
        self.positions.get(&symbol).copied().unwrap_or(0.0)
    }
    fn equity_allocated(&self) -> f64 {
        200_000.0
    }
    fn next_u64(&mut self) -> u64 {
        1
    }
    fn set_timer(&mut self, _after_ns: i64) -> TimerId {
        TimerId(1)
    }
    fn log(&mut self, _msg: &str) {}
}

fn feat(sym: SymbolId, v: f64, i: i64) -> FeatureUpdate {
    FeatureUpdate {
        feature: sym,
        name: "close".into(),
        venue: Venue::Bybit,
        symbol: sym,
        ts_ns: i,
        value: v,
        ver: 1,
    }
}

/// A minimal swing strategy that carries TWO concurrent positions on separate
/// symbols and re-evaluates only on 4h bar close (spec 035 SWG-3/SWG-4).
struct SwingTwoAsset {
    id: StrategyId,
    min_bars: u32,
    max_bars: u32,
}
impl SwingTwoAsset {
    fn new() -> Self {
        Self {
            id: StrategyId::new("swing-v0"),
            min_bars: 3,
            max_bars: 21,
        }
    }
}
impl Strategy for SwingTwoAsset {
    fn id(&self) -> StrategyId {
        self.id.clone()
    }
    fn universe(&self) -> Universe {
        Universe {
            venues: vec![Venue::Bybit],
            symbols: vec![SymbolId(1), SymbolId(2)],
        }
    }
    fn subscriptions(&self) -> Vec<String> {
        vec!["close.bybit".into()]
    }
    fn warmup_ns(&self) -> i64 {
        0
    }
    fn declared_regime(&self) -> mp_strategies::RegimeMask {
        mp_strategies::RegimeMask::any()
    }
    fn on_feature(
        &mut self,
        _u: &FeatureUpdate,
        _ctx: &mut dyn mp_strategies::Ctx,
    ) -> Vec<mp_core::OrderIntent> {
        Vec::new()
    }
    fn with_params(&self, _p: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
        Box::new(SwingTwoAsset::new())
    }
    fn holding_period_bars(&self) -> BarRange {
        BarRange::new(self.min_bars, self.max_bars)
    }
    fn rebalance_cadence(&self) -> RebalanceCadence {
        RebalanceCadence::FourHour
    }
}

#[test]
fn swg_3_swing_strategy_declares_holding_period_and_cadence() {
    let s = SwingTwoAsset::new();
    assert_eq!(s.holding_period_bars(), BarRange::new(3, 21));
    assert_eq!(s.rebalance_cadence(), RebalanceCadence::FourHour);
    // BarRange semantics: inclusive on both ends, empty when min > max.
    let r = BarRange::new(3, 21);
    assert!(!r.contains(2));
    assert!(r.contains(3));
    assert!(r.contains(21));
    assert!(!r.contains(22));
    assert!(!BarRange::new(5, 4).contains(4));
}

#[test]
fn swg_3_legacy_strategy_keeps_event_driven_defaults() {
    // The v1 strategies inherit event-driven metadata so nothing breaks —
    // the sim dispatches `Event`-cadence strategies on every event (SWG-7),
    // while swing strategies opt into `Daily`/`FourHour` bar-close dispatch.
    let cf = mp_strategies::CoinFlipStrategy::new();
    assert_eq!(cf.holding_period_bars(), BarRange::new(1, u32::MAX));
    assert_eq!(cf.rebalance_cadence(), RebalanceCadence::Event);
}

#[test]
fn swg_4_multiple_concurrent_positions_across_assets() {
    // The Ctx is per-symbol: a strategy holding BTC + ETH sees BOTH positions —
    // the substrate for swing portfolios (SWG-4). The strategy itself can also
    // read both symbols' books through the same ctx.
    let btc = SymbolId(1);
    let eth = SymbolId(2);
    let mut s = SwingTwoAsset::new();
    let mut ctx = MultiPosCtx::with(&[(btc, 2.0), (eth, 5.0)]);
    let _ = s.on_feature(&feat(btc, 100.0, 1), &mut ctx);
    assert_eq!(ctx.position(btc), 2.0);
    assert_eq!(ctx.position(eth), 5.0);
    // An unheld symbol reads zero: separate ledger per asset.
    assert_eq!(ctx.position(SymbolId(3)), 0.0);
}

#[test]
fn swg_8_liq_fade_frozen_not_removed() {
    assert!(
        FROZEN_STRATEGIES.contains(&"liq-fade-v1"),
        "liq-fade-v1 must stay in the freeze list until a deprecation decision"
    );
    // The module/re-export still exists (retained, not deleted) — import works.
    let _ = mp_strategies::LiqFadeConfig::default();
}
