//! Risk-gate + kill-switch tests (spec 007 EXE-1/7). Names embed IDs (CONV-21).

use mp_core::{Side, StrategyId, SymbolId, Venue};
use mp_risk::gate::{
    evaluate, trip_on_breach, GateInput, Mode, RejectReason, RiskLimits, TripRequest, Verdict,
};
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
        reduce_only: false,
        contract_multiplier: 1.0,
        allowed,
    }
}

fn reject(i: &GateInput) -> RejectReason {
    match evaluate(&RiskLimits::default(), &KillSwitches::new(), i) {
        Verdict::Reject(r) => r,
        Verdict::Pass => panic!("expected reject"),
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

// ---- regression_pin: RG-0 / RG-4 / RG-5 / contract-multiplier / trip_on_breach ----

#[test]
fn regression_gate_nan_qty_rejected() {
    let mut i = base(ALLOWED);
    i.qty = f64::NAN;
    assert_eq!(reject(&i), RejectReason::InvalidInput);
}

#[test]
fn regression_gate_nan_price_rejected() {
    let mut i = base(ALLOWED);
    i.price = f64::NAN;
    assert_eq!(reject(&i), RejectReason::InvalidInput);
}

#[test]
fn regression_gate_nan_mark_rejected() {
    let mut i = base(ALLOWED);
    i.mark = f64::NAN;
    assert_eq!(reject(&i), RejectReason::InvalidInput);
}

#[test]
fn regression_gate_reduce_only_close_allowed_despite_exceeding_position() {
    // A reduce-only order that merely CLOSES (no flip) is allowed even though the
    // same qty as an *add* would breach the position-notional cap.
    //
    // Long 21 @ 100 mark → position notional 2100 is ALREADY over the 2000 cap.
    // An *add* (non-reduce_only buy of 1) would push it to 2200 > 2000 → reject.
    // The reduce-only *sell* of 5 shrinks 21 → 16 (1600 ≤ 2000): though 21 (2100)
    // breaches the cap, this close must PASS, never blocked by what it reduces.
    let limits = RiskLimits::default();
    let kills = KillSwitches::new();
    let mut i = base(ALLOWED);
    i.side = Side::Sell;
    i.reduce_only = true;
    i.current_position_qty = 21.0; // long 21, notional 2100 > 2000 cap
    i.qty = 5.0; // 5×100 = 500 ≤ 500 passes RG-3; 21→16 closes the position
    assert_eq!(evaluate(&limits, &kills, &i), Verdict::Pass);
}

#[test]
fn regression_gate_reduce_only_flip_capped() {
    // reduce_only must NOT let an order FLIP the position for free. Once the sell
    // exceeds the open long, the residual is a NEW (growing) position and its full
    // order notional counts against gross (RG-5) — it is no longer a "reduction".
    let mut flip = base(ALLOWED);
    flip.side = Side::Sell;
    flip.reduce_only = true;
    flip.current_position_qty = 1.0; // long 1
    flip.qty = 4.0; // sell 4 → flips to −3 (short); not a reduction
                    // Pre-load gross just under the cap so the FULL flip notional tips it over.
    flip.gross_exposure_notional = 300_000.0 - 100.0; // + 400 (full notional) > 300k
    assert_eq!(reject(&flip), RejectReason::GrossTooLarge);
}

#[test]
fn regression_gate_contract_multiplier_scales_notional() {
    // 1 contract of an inverse-style instrument, multiplier 1000 ⇒ notional
    // 1×100×1000 = 100_000, far above RG-3's 500 — must reject.
    let mut i = base(ALLOWED);
    i.qty = 1.0;
    i.contract_multiplier = 1000.0;
    assert_eq!(reject(&i), RejectReason::OrderTooLarge);
    // With the linear default multiplier the same qty notional is 100 ≤ 500 → pass.
    let lin = base(ALLOWED);
    assert_eq!(
        evaluate(&RiskLimits::default(), &KillSwitches::new(), &lin),
        Verdict::Pass
    );
    // A non-positive multiplier is invalid input (RG-0), not silently linear.
    let mut bad = base(ALLOWED);
    bad.contract_multiplier = 0.0;
    assert_eq!(reject(&bad), RejectReason::InvalidInput);
}

#[test]
fn trip_on_breach_maps_loss_verdicts_to_kill_scopes() {
    let i = base(ALLOWED);
    let strat = Verdict::Reject(RejectReason::StrategyLossBudget);
    assert_eq!(
        trip_on_breach(strat, &i),
        Some(TripRequest::Strategy(StrategyId::new("s")))
    );
    let port = Verdict::Reject(RejectReason::PortfolioLossBudget);
    assert_eq!(trip_on_breach(port, &i), Some(TripRequest::Global));
    // Every other verdict requests no trip.
    assert_eq!(trip_on_breach(Verdict::Pass, &i), None);
    assert_eq!(
        trip_on_breach(Verdict::Reject(RejectReason::OrderTooLarge), &i),
        None
    );
}
