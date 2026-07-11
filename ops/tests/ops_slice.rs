//! Ops framework tests (spec 009). Test names embed requirement IDs (CONV-21).
//! Everything is clock-injected and offline — no real timers, no network.

use mp_core::Venue;
use mp_ops::{
    Alert, AlertRouter, Benchmark, Channel, CostBreakdown, DeadMan, FunnelEvent, KillLatch,
    LatchScope, MonthlyReport, QuietHours, RouteOutcome, Severity, StrategyRow, TrackingRow,
};

const S: i64 = 1_000_000_000; // 1s in ns
const MIN: i64 = 60 * S;

// ---- Alert framework (OPS-4, OPS-9) -------------------------------------

#[test]
fn ops4_alert_dedupes_within_window_then_fires_again() {
    let mut r = AlertRouter::new(None);
    let a = Alert::new("stream-gap", Severity::P2, 5 * MIN, "BTCUSDT gap 6m");
    // First fire sends; runbook link is derived.
    match r.route(&a, 0) {
        RouteOutcome::Sent(d) => {
            assert_eq!(d.channel, Channel::Telegram);
            assert_eq!(d.runbook, "ops/runbooks/stream-gap.md");
        }
        other => panic!("expected Sent, got {other:?}"),
    }
    // Inside the 5-minute window ⇒ deduped.
    assert_eq!(r.route(&a, 2 * MIN), RouteOutcome::Deduped);
    // After the window ⇒ sends again.
    assert!(matches!(r.route(&a, 6 * MIN), RouteOutcome::Sent(_)));
}

#[test]
fn ops9_quiet_hours_batch_p3_but_p1_breaks_through() {
    // Quiet 22:00–07:00 UTC.
    let quiet = QuietHours {
        start_min: 22 * 60,
        end_min: 7 * 60,
    };
    let mut r = AlertRouter::new(Some(quiet));
    // 02:00 UTC is inside quiet hours.
    let t_0200 = 2 * 60 * MIN;
    let p3 = Alert::new("funnel-move", Severity::P3, 0, "carry-v1 promoted");
    assert_eq!(r.route(&p3, t_0200), RouteOutcome::Batched);
    assert_eq!(r.batch_len(), 1);

    // A P1 at the same instant breaks through immediately.
    let p1 = Alert::new("recon-diverged", Severity::P1, 0, "position mismatch");
    match r.route(&p1, t_0200) {
        RouteOutcome::Sent(d) => assert_eq!(d.channel, Channel::TelegramPhone),
        other => panic!("expected Sent, got {other:?}"),
    }

    // Daytime P3 (12:00) is sent, not batched.
    let t_1200 = 12 * 60 * MIN;
    assert!(matches!(r.route(&p3, t_1200), RouteOutcome::Sent(_)));

    let digest = r.drain_batch();
    assert_eq!(digest.len(), 1);
    assert_eq!(r.batch_len(), 0);
}

// ---- Dead-man switch (OPS-2) --------------------------------------------

#[test]
fn ops2_deadman_fires_after_three_missed_beats_and_escalates_in_live() {
    // 30s interval, 3 missed ⇒ 90s deadline.
    let mut dm = DeadMan::new(30 * S);
    dm.register("collector-bybit", false, 0);
    dm.register("oms", true, 0);

    // At 60s (< 90s) nothing fires.
    assert!(dm.check(60 * S, false).is_empty());

    // At 100s (> 90s) both are silent.
    let alerts = dm.check(100 * S, false);
    assert_eq!(alerts.len(), 2);
    assert!(alerts.iter().all(|a| a.severity == Severity::P2));

    // A fresh beat for the collector clears it.
    dm.beat("collector-bybit", 100 * S);
    let alerts = dm.check(120 * S, false);
    assert_eq!(alerts.len(), 1); // only oms still silent

    // In live mode the critical oms escalates to P1.
    let live = dm.check(200 * S, true);
    assert!(live.iter().any(|a| a.severity == Severity::P1));
}

// ---- Kill-latch bridge (OPS-3, RG-10) -----------------------------------

#[test]
fn ops3_kill_latch_roundtrips_and_trips_kill_switches() {
    let latch = KillLatch::new("manual /kill from phone", 1234)
        .kill(LatchScope::Venue { venue: Venue::Okx })
        .kill(LatchScope::Strategy {
            id: "carry-v1".to_string(),
        });

    // Survives a JSON round-trip (the file the gate reads).
    let json = latch.to_json().unwrap();
    let back = KillLatch::from_json(&json).unwrap();
    assert_eq!(back.scopes.len(), 2);
    assert_eq!(back.reason, "manual /kill from phone");

    // Applies onto KillSwitches: the gate blocks the tripped scopes.
    let kills = back.to_kill_switches();
    let carry = mp_core::StrategyId::new("carry-v1");
    let other = mp_core::StrategyId::new("trend-v1");
    assert!(kills.blocks(Venue::Okx, &carry)); // both venue and strategy tripped
    assert!(kills.blocks(Venue::Okx, &other)); // venue latch alone blocks
    assert!(!kills.blocks(Venue::Bybit, &other)); // untouched scope passes
}

#[test]
fn ops3_flatten_is_global_kill() {
    let latch = KillLatch::global("/flatten double-confirmed", 9);
    let kills = latch.to_kill_switches();
    // Global latch blocks every venue/strategy.
    assert!(kills.blocks(Venue::Bybit, &mp_core::StrategyId::new("anything")));
}

// ---- Monthly report (OPS-6) ---------------------------------------------

fn fixture_report() -> MonthlyReport {
    MonthlyReport {
        month: "2026-06".to_string(),
        blended_return: 0.031,
        blended_max_drawdown: -0.012,
        strategies: vec![StrategyRow {
            strategy: "carry-v1".to_string(),
            net_return: 0.031,
            max_drawdown: -0.012,
            expectancy_r: 0.14,
            trades: 88,
            win_rate: 0.57,
        }],
        tracking: vec![TrackingRow {
            strategy: "carry-v1".to_string(),
            live_return: 0.031,
            paper_return: 0.036,
            backtest_return: 0.040,
            tracking_error: -0.009,
        }],
        costs: CostBreakdown {
            fees: 412.55,
            slippage_vs_model: 88.10,
            funding: -120.00,
            infra: 40.00,
        },
        funnel: vec![FunnelEvent {
            strategy: "liq-fade-v1".to_string(),
            from_stage: "paper".to_string(),
            to_stage: "shadow".to_string(),
            demotion: false,
        }],
        benchmark: Benchmark {
            book_return: 0.031,
            btc_hold_return: 0.088,
            tbill_return: 0.004,
        },
    }
}

#[test]
fn ops6_report_has_all_sections_and_benchmark_row() {
    let md = fixture_report().render_markdown();
    for header in [
        "## Equity & Drawdown",
        "## Expectancy (after costs)",
        "## Tracking Error",
        "## Cost Breakdown",
        "## Funnel Transitions & Kills",
        "## Benchmark",
    ] {
        assert!(md.contains(header), "missing section: {header}");
    }
    // Benchmark row REQUIRED (OPS-6): BTC-hold number present and grounded.
    assert!(md.contains("BTC hold"));
    assert!(md.contains("+8.80%")); // btc_hold_return rendered from input
    assert!(md.contains("carry-v1"));
}

#[test]
fn ops6_report_numbers_are_grounded_not_invented() {
    // Every percent token in the report must trace to an input figure — a
    // spot-check that the renderer computes nothing on its own (PD-5 honesty).
    let r = fixture_report();
    let md = r.render_markdown();
    assert!(md.contains("+3.10%")); // blended/net return
    assert!(md.contains("-1.20%")); // max drawdown
    assert!(md.contains("-0.90%")); // tracking error
                                    // Empty inputs render explicit "no data", never a blank cell.
    let mut empty = fixture_report();
    empty.strategies.clear();
    empty.tracking.clear();
    let md2 = empty.render_markdown();
    assert!(md2.contains("_no data_"));
}

// ---- Registry ↔ runbooks (OPS-4) ----------------------------------------

#[test]
fn ops4_every_catalog_alert_has_a_runbook_file() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for spec in mp_ops::ALERTS {
        let path = root.join("runbooks").join(format!("{}.md", spec.id));
        assert!(
            path.exists(),
            "alert '{}' ({}) has no runbook at {}",
            spec.id,
            spec.severity.as_str(),
            path.display()
        );
    }
}
