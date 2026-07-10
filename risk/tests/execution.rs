//! Risk-gate + kill-switch tests (spec 007 EXE-1/7). Names embed IDs (CONV-21).

use mp_core::{Side, StrategyId, SymbolId, Venue};
use mp_risk::gate::{evaluate, GateInput, Mode, RejectReason, RiskLimits, Verdict};
use mp_risk::killswitch::{KillSwitches, ResetRefused, Scope};

const ALLOWED: &[(Venue, SymbolId)] = &[(Venue::Bybit, SymbolId(0))];

fn base<'a>(allowed: &'a [(Venue, SymbolId)]) -> GateInput<'a> {
    GateInput {
        mode: Mode::Paper,
        venue: Venue::Bybit,
        symbol: SymbolId(0),
        strategy: StrategyId::new("s"),
        side: Side::Buy,
        qty: 1.0,
        price: 100.0,
        mark: 100.0,
        current_position_qty: 0.0,
        gross_exposure_notional: 0.0,
        orders_last_min: 0,
        strategy_daily_pnl: 0.0,
        portfolio_daily_pnl: 0.0,
        reconciler_clean: true,
        allowed,
    }
}

#[test]
fn exe_1_gate_passes_a_clean_order() {
    let kills = KillSwitches::new();
    assert_eq!(
        evaluate(&RiskLimits::default(), &kills, &base(ALLOWED)),
        Verdict::Pass
    );
}

#[test]
fn exe_1_gate_rejects_each_check_in_order() {
    let limits = RiskLimits::default();
    let kills = KillSwitches::new();
    let reject = |i: &GateInput| match evaluate(&limits, &kills, i) {
        Verdict::Reject(r) => r,
        Verdict::Pass => panic!("expected reject"),
    };

    let mut i = base(ALLOWED);
    i.mode = Mode::Backtest;
    assert_eq!(reject(&i), RejectReason::ModeDisallows);

    let mut i = base(ALLOWED);
    i.symbol = SymbolId(99);
    assert_eq!(reject(&i), RejectReason::NotAllowlisted);

    let mut i = base(ALLOWED);
    i.qty = 10.0; // notional 1000 > 500
    assert_eq!(reject(&i), RejectReason::OrderTooLarge);

    let mut i = base(ALLOWED);
    i.current_position_qty = 100.0; // resulting 101*100 = 10100 > 2000
    assert_eq!(reject(&i), RejectReason::PositionTooLarge);

    let mut i = base(ALLOWED);
    i.gross_exposure_notional = 299_950.0; // +100 > 300k
    assert_eq!(reject(&i), RejectReason::GrossTooLarge);

    let mut i = base(ALLOWED);
    i.price = 105.0; // 5% from mark > 2%
    assert_eq!(reject(&i), RejectReason::PriceOutOfBand);

    let mut i = base(ALLOWED);
    i.orders_last_min = 30;
    assert_eq!(reject(&i), RejectReason::RateLimited);

    let mut i = base(ALLOWED);
    i.strategy_daily_pnl = -2_000.0;
    assert_eq!(reject(&i), RejectReason::StrategyLossBudget);

    let mut i = base(ALLOWED);
    i.portfolio_daily_pnl = -4_000.0;
    assert_eq!(reject(&i), RejectReason::PortfolioLossBudget);

    let mut i = base(ALLOWED);
    i.reconciler_clean = false;
    assert_eq!(reject(&i), RejectReason::ReconcilerDiverged);
}

#[test]
fn exe_10_kill_switch_blocks_orders() {
    let limits = RiskLimits::default();
    let mut kills = KillSwitches::new();
    kills.trip(Scope::Venue(Venue::Bybit));
    assert_eq!(
        evaluate(&limits, &kills, &base(ALLOWED)),
        Verdict::Reject(RejectReason::KillSwitchTripped)
    );
}

#[test]
fn exe_7_kill_switch_is_one_way_latch() {
    let mut kills = KillSwitches::new();
    let scope = Scope::Strategy(StrategyId::new("s"));
    kills.trip(scope.clone());
    assert!(kills.is_tripped(&scope));
    // An agent (human=false) cannot reset.
    assert_eq!(kills.reset(&scope, false), Err(ResetRefused));
    assert!(kills.is_tripped(&scope));
    // A human can.
    assert!(kills.reset(&scope, true).is_ok());
    assert!(!kills.is_tripped(&scope));
}
