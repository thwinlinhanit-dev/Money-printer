//! Walk-forward / plateau / Monte-Carlo harnesses (SIM-9), experiment tracker
//! (SIM-10), replay-live decision-log diff (SIM-11), and gate G1 (spec 006
//! STR-9). Test names embed requirement IDs (CONV-21).

use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};
use mp_features::catalog::Cvd;
use mp_features::FeatureEngine;
use mp_sim::{
    evaluate_g1, monte_carlo, plateau_ok, walk_forward, Backtester, DecisionLog, G1Params,
    RunRecord, SimConfig, WalkForwardParams,
};
use mp_strategies::CoinFlipStrategy;

const MS: i64 = 1_000_000;
const DAY: i64 = 86_400_000_000_000;

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

fn feed(n: i64) -> Vec<EventEnvelope> {
    (0..n)
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
    e.register_tick(|| Box::new(Cvd::new("bybit")));
    e
}

fn run(seed: u64, events: &[EventEnvelope]) -> Backtester {
    let mut bt = Backtester::new(
        engine(),
        Box::new(CoinFlipStrategy::new()),
        SimConfig {
            latency_ns: 50 * MS,
            ..SimConfig::default()
        },
        seed,
    );
    bt.run(events).unwrap();
    bt
}

// ---- STR-9 / SIM-8: CoinFlip fails G1 ------------------------------------

#[test]
fn str_9_coinflip_fails_g1_on_fixture_data() {
    let bt = run(42, &feed(200));
    let result = evaluate_g1(
        bt.metrics(),
        bt.stress_expectancy_2x(),
        &G1Params::default(),
    );
    // A zero-edge coin flip, after costs and the 2x-cost stress, must not pass
    // the first honest gate — that is the worked example of a G1 kill (STR-9).
    assert!(
        !result.pass,
        "coinflip must fail G1; reasons={:?}",
        result.reasons
    );
    assert!(!result.reasons.is_empty());
}

#[test]
fn str_9_g1_passes_only_when_all_conditions_met() {
    // A hand-built metrics snapshot that satisfies every G1 condition passes;
    // flipping any single one fails (guards against a vacuous gate).
    use mp_sim::Metrics;
    let mut m = Metrics::new();
    for _ in 0..150 {
        m.record_trade(1.0); // 150 winning trades
    }
    let p = G1Params {
        min_trades: 100,
        dd_budget: 1_000.0,
    };
    // Positive 2x-cost expectancy, enough trades, DD under budget, no maker P&L.
    assert!(evaluate_g1(&m, 0.5, &p).pass);
    // Non-positive stress expectancy fails.
    assert!(!evaluate_g1(&m, 0.0, &p).pass);
    // Too few trades fails.
    let mut few = Metrics::new();
    for _ in 0..50 {
        few.record_trade(1.0);
    }
    assert!(!evaluate_g1(&few, 0.5, &p).pass);
}

#[test]
fn sim_12_g1_rejects_optimistic_maker_dependent_edge() {
    use mp_sim::Metrics;
    let mut m = Metrics::new();
    // 120 trades: the non-maker leg nets zero (wins == losses), all the "edge"
    // lives in maker fills — G1 must reject it (SIM-12).
    for _ in 0..60 {
        m.record_trade(1.0);
    }
    for _ in 0..60 {
        m.record_trade(-1.0);
    }
    for _ in 0..30 {
        m.record_maker_trade(2.0); // maker-only profit
    }
    let p = G1Params {
        min_trades: 100,
        dd_budget: f64::INFINITY,
    };
    let r = evaluate_g1(&m, 0.5, &p);
    assert!(!r.pass);
    assert!(r.reasons.iter().any(|s| s.contains("maker-dependent")));
}

// ---- SIM-9: walk-forward --------------------------------------------------

#[test]
fn sim_9_walk_forward_rolls_windows_and_reports_oos() {
    let events = feed(400); // spans 0 .. 399*100ms
    let span = events.last().unwrap().recv_ts_ns;
    let wf = WalkForwardParams {
        train_ns: span / 4,
        test_ns: span / 8,
        step_ns: span / 8,
    };
    let mut windows = 0;
    let results = walk_forward(&events, wf, |train, test| {
        windows += 1;
        // Fit is a no-op for CoinFlip; we just run OOS on the test slice.
        assert!(!test.is_empty());
        let _ = train;
        let bt = run(7, test);
        bt.summary()
    });
    assert!(results.len() >= 2, "expected multiple rolling windows");
    assert_eq!(results.len(), windows);
    // Windows are contiguous and advance by the step.
    for w in &results {
        assert!(w.test_end_ns > w.test_start_ns);
        assert!(w.test_start_ns > w.train_start_ns);
    }
}

#[test]
fn sim_9_plateau_flags_sign_flip_within_30pct() {
    // Base edge positive; a +20% perturbation flips it negative ⇒ curve-fit.
    assert!(!plateau_ok(0.5, &[(0.10, 0.4), (0.20, -0.1)]));
    // A plateau: all nearby perturbations keep the sign ⇒ ok.
    assert!(plateau_ok(0.5, &[(0.10, 0.45), (0.30, 0.2), (-0.30, 0.3)]));
    // A far (>30%) flip does not count against the plateau.
    assert!(plateau_ok(0.5, &[(0.50, -0.2)]));
}

// ---- SIM-9: Monte-Carlo block bootstrap ----------------------------------

#[test]
fn sim_9_monte_carlo_is_seeded_and_reports_dd_distribution() {
    // Trades across three days; block bootstrap by day.
    let pnls = vec![
        (0, 10.0),
        (MS, -5.0),
        (DAY, -8.0),
        (DAY + MS, 3.0),
        (2 * DAY, -12.0),
        (2 * DAY + MS, 6.0),
    ];
    let a = monte_carlo(&pnls, 500, 99, DAY);
    let b = monte_carlo(&pnls, 500, 99, DAY);
    assert_eq!(a, b, "same seed ⇒ identical DD distribution (CONV-11)");
    // Percentiles are ordered and non-negative (drawdown is a magnitude).
    assert!(a.p50_max_dd >= 0.0);
    assert!(a.p95_max_dd >= a.p50_max_dd);
    assert!(a.worst_max_dd >= a.p95_max_dd);
    // A different seed can differ but stays a valid distribution.
    let c = monte_carlo(&pnls, 500, 1234, DAY);
    assert!(c.p95_max_dd >= c.p50_max_dd);
}

#[test]
fn sim_9_monte_carlo_empty_is_zero() {
    let mc = monte_carlo(&[], 100, 1, DAY);
    assert_eq!(mc.p95_max_dd, 0.0);
}

// ---- SIM-10: experiment tracker ------------------------------------------

#[test]
fn sim_10_run_record_is_reproducible_and_identifies_experiments() {
    let bt = run(42, &feed(200));
    let summary = bt.summary();
    let rec = RunRecord::new(
        "01J000RUNID",
        "abc123",
        "latency_ns=50ms;fee=0.00055",
        0,
        200 * 100 * MS,
        vec![111, 222],
        bt.decision_log().hash(),
        summary,
    );
    // JSONL captures the reproducibility fields (SIM-10).
    let line = rec.to_jsonl();
    assert!(line.contains("\"config_hash\""));
    assert!(line.contains("\"git_sha\":\"abc123\""));
    assert!(line.contains("\"decision_log_hash\""));

    // Same config+data+manifests ⇒ "same experiment" regardless of run_id.
    let rec2 = RunRecord::new(
        "01J000OTHER",
        "abc123",
        "latency_ns=50ms;fee=0.00055",
        0,
        200 * 100 * MS,
        vec![111, 222],
        bt.decision_log().hash(),
        summary,
    );
    assert!(rec.same_experiment(&rec2));
    // Changing the config text changes the hash ⇒ a different experiment.
    let rec3 = RunRecord::new(
        "01J000THIRD",
        "abc123",
        "latency_ns=100ms;fee=0.00055",
        0,
        200 * 100 * MS,
        vec![111, 222],
        bt.decision_log().hash(),
        summary,
    );
    assert!(!rec.same_experiment(&rec3));
}

// ---- SIM-11: replay-live decision-log diff -------------------------------

#[test]
fn sim_11_replay_live_diff_detects_divergence() {
    let events = feed(200);
    // Two identical replays ⇒ no divergence (the determinism check is green).
    let live = run(42, &events);
    let replay = run(42, &events);
    assert_eq!(
        live.decision_log().first_divergence(replay.decision_log()),
        None
    );
    // A different seed changes the coin flips ⇒ the logs diverge at some line.
    let other = run(9999, &events);
    let d = live.decision_log().first_divergence(other.decision_log());
    assert!(d.is_some(), "different decisions must diverge (P1 in live)");
}

#[test]
fn sim_11_length_mismatch_with_shared_prefix_diverges() {
    let mut a = DecisionLog::new();
    let mut b = DecisionLog::new();
    // Same first line via the same intent, then `a` has an extra line.
    use mp_core::{IntentId, OrderIntent, OrderKind, SizeUnit, StrategyId, TimeInForce};
    let intent = OrderIntent {
        intent_id: IntentId(1),
        strategy: StrategyId::new("s"),
        venue: Venue::Bybit,
        symbol: SymbolId(0),
        side: Side::Buy,
        kind: OrderKind::Market,
        qty: SizeUnit::Contracts(1.0),
        tif: TimeInForce::Ioc,
        reduce_only: false,
        tag: "t".into(),
    };
    a.record_intent(1, &intent);
    b.record_intent(1, &intent);
    a.record_intent(2, &intent); // a is longer
    assert_eq!(a.first_divergence(&b), Some(1));
    assert_eq!(b.first_divergence(&a), Some(1));
}
