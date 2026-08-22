//! Acceptance tests for spec 005. Test names embed requirement IDs (CONV-21).

use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};
use mp_features::catalog::Cvd;
use mp_features::FeatureEngine;
use mp_sim::{Accountant, Backtester, FillOptimism, SimConfig};
use mp_strategies::CoinFlipStrategy;

const MS: i64 = 1_000_000;

fn trade(recv: i64, price: f64, qty: f64, side: Side) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        recv,
        recv,
        recv as u64,
        MarketEvent::Trade {
            price,
            qty,
            side,
            trade_id: recv as u64,
        },
    )
}

fn feed() -> Vec<EventEnvelope> {
    (0..40)
        .map(|i| {
            trade(
                i * 100 * MS,
                100.0 + (i % 7) as f64 * 0.5,
                1.0,
                if i % 2 == 0 { Side::Buy } else { Side::Sell },
            )
        })
        .collect()
}

fn engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit)));
    e
}

fn run_once(seed: u64) -> (u64, f64) {
    let mut bt = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig {
            latency_ns: 50 * MS,
            ..SimConfig::default()
        },
        seed,
    );
    bt.run(&feed()).unwrap();
    (bt.decision_log().hash(), bt.equity())
}

#[test]
fn sim_1_clock_is_driven_by_events() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig::default(),
        1,
    );
    let events = feed();
    bt.run(&events).unwrap();
    assert_eq!(bt.now_ns(), events.last().unwrap().recv_ts_ns);
}

#[test]
fn sim_7_replay_is_deterministic() {
    let a = run_once(1234);
    let b = run_once(1234);
    assert_eq!(a.0, b.0, "identical inputs ⇒ identical decision-log hash");
    assert_eq!(a.1.to_bits(), b.1.to_bits(), "identical final equity");
    // A different seed changes the coin flips ⇒ different decisions.
    let c = run_once(9999);
    assert_ne!(a.0, c.0);
}

#[test]
fn swg_5_summary_reports_bar_return_sharpe() {
    // SWG-5: the engine samples equity once per bar boundary; with a small
    // bar_tf_ns the 40-event feed crosses many bars, so summary().sharpe is
    // populated (and annualized for the configured timeframe). A single-bar
    // run (warmup, FEA-3) must report None, never NaN.
    let mut bt = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig {
            latency_ns: 50 * MS,
            bar_tf_ns: 10 * MS, // 10ms bars → the 40×100ms feed spans 400 bars
            ..SimConfig::default()
        },
        7,
    );
    bt.run(&feed()).unwrap();
    assert!(
        bt.summary().sharpe.is_some(),
        "multi-bar run must produce a bar-return Sharpe"
    );
    assert!(bt.summary().sharpe.unwrap().is_finite());

    // Warmup: an 8-event feed inside a single bar → no second sample yet.
    let short: Vec<EventEnvelope> = feed().into_iter().take(8).collect();
    let mut bt2 = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig {
            latency_ns: 50 * MS,
            bar_tf_ns: DAY_BAR,
            ..SimConfig::default()
        },
        7,
    );
    bt2.run(&short).unwrap();
    assert!(bt2.summary().sharpe.is_none(), "warmup ⇒ no Sharpe");
}

const DAY_BAR: i64 = 86_400_000_000_000;

#[test]
fn sim_14_golden_hash_is_stable() {
    // Golden fixture: this hash must not change unless the sim semantics change
    // intentionally (CONV-12). If this breaks, a determinism-affecting change
    // slipped in — investigate before updating the constant.
    //
    // Pinned on 2026-08-17 (audit fix-all) after the funding-attribution,
    // maker-fee, and timer fixes changed the decision log. Previously the test
    // compared fresh-vs-fresh, which cannot detect semantic drift — only
    // nondeterminism.
    const GOLDEN: u64 = 2418677452747381422;
    let (hash, _) = run_once(42);
    assert_eq!(hash, GOLDEN, "decision-log hash drifted (CONV-12)");
    assert_eq!(hash, run_once(42).0, "replay must be deterministic");
}

#[test]
fn sim_2_taker_fill_applies_fee_and_slippage() {
    // One buy market order fills at the NEXT trade after latency, at
    // price*(1+slip), paying taker fee.
    let mut acct = Accountant::new(100_000.0);
    acct.mark(SymbolId(0), 100.0);
    // Simulate a buy of 2 @ 100*(1.0001) with fee.
    let px = 100.0 * 1.0001;
    let fee = px * 2.0 * 0.00055;
    acct.apply_fill(SymbolId(0), 2.0, px, fee, FillOptimism::None);
    assert_eq!(acct.position(SymbolId(0)), 2.0);
    // Cash dropped by notional + fee.
    let expected_cash = 100_000.0 - px * 2.0 - fee;
    assert!((acct.equity() - (expected_cash + 2.0 * px)).abs() < 1e-6);
}

#[test]
fn sim_13_accounting_identity_holds_through_run() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig {
            latency_ns: 50 * MS,
            ..SimConfig::default()
        },
        7,
    );
    bt.run(&feed()).unwrap();
    // equity == start + realized + unrealized − fees − funding, at run end.
    assert!(
        bt.identity_residual().abs() < 1e-6,
        "identity residual = {}",
        bt.identity_residual()
    );
    // The CoinFlip actually traded (fills happened).
    assert!(bt.decision_log().fill_count() > 0);
}

#[test]
fn sim_13_realized_and_funding_identity() {
    let mut acct = Accountant::new(1000.0);
    acct.mark(SymbolId(0), 100.0);
    acct.apply_fill(SymbolId(0), 1.0, 100.0, 1.0, FillOptimism::None); // buy 1 @100 fee1
    acct.mark(SymbolId(0), 110.0);
    acct.accrue_funding(SymbolId(0), 0.001); // long pays 0.001*1*110 = 0.11
    acct.apply_fill(SymbolId(0), -1.0, 110.0, 1.0, FillOptimism::None); // sell 1 @110 fee1
    assert!(acct.identity_residual().abs() < 1e-9);
    assert!((acct.realized() - 10.0).abs() < 1e-9);
    assert!((acct.funding_paid() - 0.11).abs() < 1e-9);
}

#[test]
fn sim_13_funding_attributed_to_closed_trade() {
    // Audit fix-all 2026-08-17: funding paid while holding must reach the
    // closed trade's NET P&L (it was invisible to expectancy). A round trip
    // at a flat price that paid funding must net −funding, not zero.
    let mut acct = Accountant::new(1000.0);
    acct.mark(SymbolId(0), 100.0);
    acct.apply_fill(SymbolId(0), 10.0, 100.0, 0.0, FillOptimism::None); // long 10 @100
    acct.accrue_funding(SymbolId(0), 0.001); // pays 0.001*10*100 = 1.0
    let o = acct.apply_fill(SymbolId(0), -10.0, 100.0, 0.0, FillOptimism::None); // close @100
    assert!(
        (o.realized_gross - 0.0).abs() < 1e-9,
        "flat price ⇒ no realized"
    );
    assert!(
        (o.attributed_funding - 1.0).abs() < 1e-9,
        "closing fill must attribute the funding paid while holding"
    );
    assert!(acct.identity_residual().abs() < 1e-9);
    assert!((acct.funding_paid() - 1.0).abs() < 1e-9);
}

#[test]
fn sim_13_trade_tag_keeps_entry_leg_optimism() {
    // Audit fix-all 2026-08-17 (B4): the closed trade's optimism tag must be
    // the worst-case across its legs — a maker-optimistic entry closed by a
    // conservative taker fill must still land in the G1 maker bucket, or the
    // optimism gate would miss the entry-leg assumption.
    let mut acct = Accountant::new(1000.0);
    acct.mark(SymbolId(0), 100.0);
    acct.apply_fill(SymbolId(0), 5.0, 100.0, 0.0, FillOptimism::Maker); // optimistic entry
    let add = acct.apply_fill(SymbolId(0), 3.0, 100.0, 0.0, FillOptimism::None);
    assert!(add.closed_qty == 0.0, "add-on is not a close");
    let o = acct.apply_fill(SymbolId(0), -8.0, 100.0, 0.0, FillOptimism::Tape); // taker exit
    assert!(o.closed_qty == 8.0);
    assert!(
        o.optimism == FillOptimism::Maker,
        "entry-leg maker optimism must survive a taker exit (got {:?})",
        o.optimism
    );
    // A flip closes the old trade (tag from the closed side) and resets the
    // new side's tag to the flip fill's own.
    let _open = acct.apply_fill(SymbolId(0), 2.0, 100.0, 0.0, FillOptimism::Maker);
    let flip = acct.apply_fill(SymbolId(0), -5.0, 100.0, 0.0, FillOptimism::Tape);
    assert!(flip.closed_qty == 2.0);
    assert!(
        flip.optimism == FillOptimism::Maker,
        "flip closes the maker-tagged side (got {:?})",
        flip.optimism
    );
    let o2 = acct.apply_fill(SymbolId(0), 3.0, 100.0, 0.0, FillOptimism::Tape);
    assert!(o2.closed_qty == 3.0);
    assert!(
        o2.optimism == FillOptimism::Tape,
        "flip-side tag starts fresh (got {:?})",
        o2.optimism
    );
}
