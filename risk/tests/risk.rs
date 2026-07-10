//! Acceptance tests for spec 008. Test names embed requirement IDs (CONV-21).

use mp_risk::allocator::{allocate, shrink_only, AllocParams, StrategyInput};
use mp_risk::kelly::{kelly_cap, KellyParams, KellyStats};
use mp_risk::sizing::{size, SizingInputs, SizingParams};
use mp_risk::{dd_governor, full_kelly};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn base_inputs() -> SizingInputs {
    SizingInputs {
        risk_units: 1.0,
        equity: 100_000.0,
        alloc_weight: 0.5,
        instrument_vol_frac: 0.02, // 2% per-horizon vol
        mark_price: 100.0,
        k_stop: 1.5,
        step_size: 0.001,
        min_notional: 5.0,
    }
}

#[test]
fn rsk_1_sizing_matches_hand_computation() {
    let p = SizingParams {
        per_trade_risk_pct: 0.005,
    };
    let s = size(&p, &base_inputs());
    // risk_capital = 100k * 0.5 = 50k
    // per_unit_risk = 50k * 0.005 = 250
    // dollar_vol = 0.02 * 100 = 2.0
    // raw = 1 * 250 / (1.5 * 2.0) = 250 / 3 = 83.333...
    assert!((s.trace.risk_capital - 50_000.0).abs() < 1e-9);
    assert!((s.trace.per_unit_risk - 250.0).abs() < 1e-9);
    assert!((s.trace.dollar_vol_per_contract - 2.0).abs() < 1e-9);
    assert!((s.trace.raw_contracts - (250.0 / 3.0)).abs() < 1e-9);
    // rounded down to 0.001: 83.333
    assert!((s.qty_contracts - 83.333).abs() < 1e-3);
}

#[test]
fn rsk_1_min_notional_floor_zeroes_tiny_trades() {
    let p = SizingParams::default();
    let mut inp = base_inputs();
    inp.risk_units = 0.00001; // microscopic → below min_notional
    let s = size(&p, &inp);
    assert!(s.trace.floored_to_zero);
    assert_eq!(s.qty_contracts, 0.0);
}

#[test]
fn rsk_1_zero_vol_is_no_trade_not_nan() {
    let p = SizingParams::default();
    let mut inp = base_inputs();
    inp.instrument_vol_frac = 0.0;
    let s = size(&p, &inp);
    assert_eq!(s.qty_contracts, 0.0);
    assert!(s.trace.raw_contracts.is_finite());
}

#[test]
fn rsk_2_dd_governor_shape() {
    assert_eq!(dd_governor(0.0, 0.1, 1.0), 1.0); // no DD → full
    assert!((dd_governor(0.05, 0.1, 1.0) - 0.5).abs() < 1e-9); // half budget → 0.5
    assert_eq!(dd_governor(0.1, 0.1, 1.0), 0.0); // at budget → 0
    assert_eq!(dd_governor(0.2, 0.1, 1.0), 0.0); // beyond → clamped 0
    assert_eq!(dd_governor(0.05, 0.0, 1.0), 0.0); // bad budget → fail closed
}

#[test]
fn rsk_3_kelly_cap_pins_below_min_trades_and_estimates_above() {
    let params = KellyParams::default(); // fraction 0.25, min 30, floor 0.02
                                         // Below min_trades ⇒ pinned to floor.
    let pinned = kelly_cap(
        &params,
        &KellyStats {
            trades: 5,
            p: 0.6,
            b: 2.0,
        },
    );
    assert_eq!(pinned, 0.02);

    // Above min_trades ⇒ 0.25 * f*, f* = 0.6 - 0.4/2 = 0.4 ⇒ cap 0.1.
    let stats = KellyStats {
        trades: 100,
        p: 0.6,
        b: 2.0,
    };
    assert!((full_kelly(&stats) - 0.4).abs() < 1e-9);
    assert!((kelly_cap(&params, &stats) - 0.1).abs() < 1e-9);

    // Negative-edge estimate clamps to 0.
    let bad = KellyStats {
        trades: 100,
        p: 0.3,
        b: 1.0,
    };
    assert_eq!(kelly_cap(&params, &bad), 0.0);
}

#[test]
fn rsk_4_allocator_caps_and_renormalizes() {
    let params = AllocParams { max_deployed: 0.8 };
    let mut inputs = BTreeMap::new();
    inputs.insert(
        "a".to_string(),
        StrategyInput {
            base_w: 1.0,
            regime_fit: 1.0,
            corr_penalty: 1.0,
            dd_gov: 1.0,
            kelly_cap: 0.5,
        },
    );
    inputs.insert(
        "b".to_string(),
        StrategyInput {
            base_w: 1.0,
            regime_fit: 0.5, // regime mismatch penalty (RSK-7)
            corr_penalty: 1.0,
            dd_gov: 1.0,
            kelly_cap: 0.5,
        },
    );
    let w = allocate(&params, &inputs);
    // a raw=1 capped at 0.5; b raw=0.5 capped at 0.5 → 0.5. sum=1.0 > 0.8 →
    // scale by 0.8 ⇒ a=0.4, b=0.4.
    assert!((w["a"] - 0.4).abs() < 1e-9);
    assert!((w["b"] - 0.4).abs() < 1e-9);
    assert!(w.values().sum::<f64>() <= 0.8 + 1e-9);
}

#[test]
fn rsk_4_intraday_shrink_only() {
    let mut prev = BTreeMap::new();
    prev.insert("a".to_string(), 0.3);
    let mut proposed = BTreeMap::new();
    proposed.insert("a".to_string(), 0.6); // wants to grow intraday
    let out = shrink_only(&prev, &proposed);
    assert_eq!(out["a"], 0.3, "intraday may only shrink");

    let mut proposed2 = BTreeMap::new();
    proposed2.insert("a".to_string(), 0.1); // shrink is allowed
    assert_eq!(shrink_only(&prev, &proposed2)["a"], 0.1);
}

// ---- property tests (RSK-9) -------------------------------------------------

proptest! {
    #[test]
    fn rsk_9_sizing_monotonic_in_risk_units(u1 in 0.0f64..10.0, extra in 0.0f64..10.0) {
        let p = SizingParams::default();
        let mut inp = base_inputs();
        inp.step_size = 0.0; // no rounding, isolate monotonicity
        inp.min_notional = 0.0;
        inp.risk_units = u1;
        let s1 = size(&p, &inp).qty_contracts;
        inp.risk_units = u1 + extra;
        let s2 = size(&p, &inp).qty_contracts;
        prop_assert!(s2 + 1e-9 >= s1);
    }

    #[test]
    fn rsk_9_governor_in_unit_interval(dd in 0.0f64..1.0, budget in 0.001f64..1.0, gamma in 0.1f64..3.0) {
        let g = dd_governor(dd, budget, gamma);
        prop_assert!((0.0..=1.0).contains(&g));
        prop_assert!(g.is_finite());
    }

    #[test]
    fn rsk_9_allocator_within_budget_and_finite(
        n in 1usize..6,
        bw in 0.0f64..2.0,
        cap in 0.0f64..1.0,
    ) {
        let params = AllocParams { max_deployed: 0.8 };
        let mut inputs = BTreeMap::new();
        for i in 0..n {
            inputs.insert(format!("s{i}"), StrategyInput {
                base_w: bw, regime_fit: 1.0, corr_penalty: 1.0, dd_gov: 1.0, kelly_cap: cap,
            });
        }
        let w = allocate(&params, &inputs);
        let sum: f64 = w.values().sum();
        prop_assert!(sum <= 0.8 + 1e-9);
        prop_assert!(w.values().all(|v| v.is_finite() && *v >= 0.0));
    }
}
