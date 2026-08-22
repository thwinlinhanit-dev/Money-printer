//! Acceptance tests for spec 035 SWG-6 (portfolio-level risk, amending spec
//! 008): correlation-adjusted exposure caps (RG-13), max concurrent positions
//! (RG-12), and funding drag in expected-return calculations. Test names embed
//! requirement IDs (CONV-21): swg_6_*.

use mp_core::{Side, StrategyId, SymbolId, Venue};
use mp_risk::gate::{evaluate, GateInput, Mode, RejectReason, RiskLimits, Verdict};
use mp_risk::killswitch::KillSwitches;
use mp_risk::portfolio::{
    correlation_adjusted_exposure, cumulative_funding_cost, expected_return_net_of_funding,
};

const ALLOWED: &[(Venue, SymbolId)] = &[(Venue::Bybit, SymbolId(0))];

fn base<'a>(allowed: &'a [(Venue, SymbolId)]) -> GateInput<'a> {
    GateInput {
        mode: Mode::Paper,
        venue: Venue::Bybit,
        symbol: SymbolId(0),
        strategy: StrategyId::new("swing-v0"),
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
        open_positions: 0,
        corr_adjusted_exposure_notional: 0.0,
    }
}

// ---- correlation-adjusted exposure (portfolio.rs) --------------------------

#[test]
fn swg_6_correlation_adjusted_exposure_single_position_is_its_notional() {
    // sqrt(1000² × 1) = 1000 — a lone position is its own gross.
    assert_eq!(
        correlation_adjusted_exposure(&[1000.0], &[vec![1.0]]),
        1000.0
    );
}

#[test]
fn swg_6_correlation_adjusted_exposure_perfectly_correlated_is_the_gross_sum() {
    // All ρ = 1 ⇒ sqrt((1000+500)²) = 1500 — the RG-5 gross number.
    let corr = vec![vec![1.0, 1.0], vec![1.0, 1.0]];
    assert!((correlation_adjusted_exposure(&[1000.0, 500.0], &corr) - 1500.0).abs() < 1e-9);
}

#[test]
fn swg_6_correlation_adjusted_exposure_uncorrelated_is_sqrt_of_sum_of_squares() {
    // Identity ρ ⇒ sqrt(3² + 4²) = 5: diversification earns a bigger book.
    let corr = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
    assert!((correlation_adjusted_exposure(&[3.0, 4.0], &corr) - 5.0).abs() < 1e-9);
}

#[test]
fn swg_6_correlation_adjusted_exposure_partial_correlation_in_between() {
    // ρ = 0.5: sqrt(3² + 4² + 2·0.5·3·4) = sqrt(25 + 12) = sqrt(37).
    let corr = vec![vec![1.0, 0.5], vec![0.5, 1.0]];
    assert!((correlation_adjusted_exposure(&[3.0, 4.0], &corr) - 37.0_f64.sqrt()).abs() < 1e-9);
}

#[test]
fn swg_6_correlation_adjusted_exposure_empty_book_is_zero() {
    assert_eq!(correlation_adjusted_exposure(&[], &[]), 0.0);
}

#[test]
fn swg_6_correlation_adjusted_exposure_bad_input_fails_closed() {
    // Ragged matrix → +inf (a cap that always rejects), never a small number.
    assert_eq!(
        correlation_adjusted_exposure(&[1.0, 2.0], &[vec![1.0, 0.0]]),
        f64::INFINITY
    );
    // Wrong matrix length.
    assert_eq!(
        correlation_adjusted_exposure(&[1.0], &[vec![1.0], vec![1.0]]),
        f64::INFINITY
    );
    // Out-of-range / non-finite ρ → +inf.
    assert_eq!(
        correlation_adjusted_exposure(&[1.0, 2.0], &[vec![1.0, 2.0], vec![2.0, 1.0]]),
        f64::INFINITY
    );
    assert_eq!(
        correlation_adjusted_exposure(&[1.0, 2.0], &[vec![1.0, f64::NAN], vec![f64::NAN, 1.0]]),
        f64::INFINITY
    );
}

// ---- funding drag in expected returns (portfolio.rs) -----------------------

#[test]
fn swg_6_cumulative_funding_cost_long_pays_short_receives() {
    // 1000 notional × 0.0001/interval × 3 intervals: long pays 0.30, short gets 0.30.
    assert!((cumulative_funding_cost(1000.0, 0.0001, 3, Side::Buy) - 0.30).abs() < 1e-12);
    assert!((cumulative_funding_cost(1000.0, 0.0001, 3, Side::Sell) + 0.30).abs() < 1e-12);
}

#[test]
fn swg_6_cumulative_funding_cost_negative_rate_reverses() {
    // Inverted funding: the long earns, the short pays.
    assert!((cumulative_funding_cost(1000.0, -0.0001, 3, Side::Buy) + 0.30).abs() < 1e-12);
    assert!((cumulative_funding_cost(1000.0, -0.0001, 3, Side::Sell) - 0.30).abs() < 1e-12);
}

#[test]
fn swg_6_expected_return_net_of_funding() {
    // Gross spot P&L 150 on a long: minus 0.30 funding over 3 intervals.
    assert!(
        (expected_return_net_of_funding(150.0, 1000.0, 0.0001, 3, Side::Buy) - 149.70).abs() < 1e-9
    );
    // A short receives funding: net is gross PLUS the drag back.
    assert!(
        (expected_return_net_of_funding(150.0, 1000.0, 0.0001, 3, Side::Sell) - 150.30).abs()
            < 1e-9
    );
    // Non-finite inputs → NaN, never an optimistic number.
    assert!(cumulative_funding_cost(1000.0, f64::NAN, 3, Side::Buy).is_nan());
    assert!(expected_return_net_of_funding(f64::NAN, 1000.0, 0.0001, 3, Side::Buy).is_nan());
    assert!(expected_return_net_of_funding(150.0, 1000.0, f64::INFINITY, 3, Side::Buy).is_nan());
}

// ---- RG-12 max concurrent positions (gate) --------------------------------

#[test]
fn swg_6_gate_rejects_new_position_above_max_concurrent() {
    let limits = RiskLimits {
        max_concurrent_positions: 2,
        ..RiskLimits::default()
    };
    let kills = KillSwitches::new();

    // 2 slots already open; a buy opening a NEW symbol slot → RG-12 reject.
    let mut i = base(ALLOWED);
    i.open_positions = 2;
    assert_eq!(
        evaluate(&limits, &kills, &i),
        Verdict::Reject(RejectReason::TooManyPositions)
    );

    // 1 open slot and 2 allowed → passes RG-12 (and everything else).
    let mut i = base(ALLOWED);
    i.open_positions = 1;
    assert_eq!(evaluate(&limits, &kills, &i), Verdict::Pass);
}

#[test]
fn swg_6_gate_concurrent_count_ignores_adds_and_reduces() {
    let limits = RiskLimits {
        max_concurrent_positions: 2,
        ..RiskLimits::default()
    };
    let kills = KillSwitches::new();

    // Adding to an ALREADY held symbol consumes no new slot → passes.
    let mut add = base(ALLOWED);
    add.current_position_qty = 5.0;
    add.open_positions = 2;
    assert_eq!(evaluate(&limits, &kills, &add), Verdict::Pass);

    // A reduce-only order also never consumes a slot → passes.
    let mut red = base(ALLOWED);
    red.side = Side::Sell;
    red.reduce_only = true;
    red.open_positions = 2;
    assert_eq!(evaluate(&limits, &kills, &red), Verdict::Pass);
}

// ---- RG-13 correlation-adjusted exposure cap (gate) ------------------------

#[test]
fn swg_6_gate_rejects_above_corr_adjusted_cap() {
    let limits = RiskLimits {
        max_corr_adjusted_portfolio: 1_000.0,
        ..RiskLimits::default()
    };
    let kills = KillSwitches::new();

    let mut over = base(ALLOWED);
    over.corr_adjusted_exposure_notional = 1_001.0;
    assert_eq!(
        evaluate(&limits, &kills, &over),
        Verdict::Reject(RejectReason::CorrAdjustedTooLarge)
    );

    let mut at = base(ALLOWED);
    at.corr_adjusted_exposure_notional = 1_000.0;
    assert_eq!(evaluate(&limits, &kills, &at), Verdict::Pass);
}

#[test]
fn swg_6_gate_fail_closed_on_non_finite_corr_adjusted_input() {
    let mut i = base(ALLOWED);
    i.corr_adjusted_exposure_notional = f64::NAN;
    assert_eq!(reject(&i), RejectReason::InvalidInput);

    let mut i = base(ALLOWED);
    i.corr_adjusted_exposure_notional = -1.0;
    assert_eq!(reject(&i), RejectReason::InvalidInput);
}

fn reject(i: &GateInput) -> RejectReason {
    match evaluate(&RiskLimits::default(), &KillSwitches::new(), i) {
        Verdict::Reject(r) => r,
        Verdict::Pass => panic!("expected reject"),
    }
}
