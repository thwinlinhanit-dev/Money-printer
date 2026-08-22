//! Ops framework tests (spec 009). Test names embed requirement IDs (CONV-21).
//! Everything is clock-injected and offline — no real timers, no network.

use mp_core::Venue;
use mp_ops::{
    Alert, AlertRouter, BandAccuracyRow, Benchmark, Channel, CostBreakdown, DeadMan, FunnelEvent,
    KillLatch, LatchScope, MonthlyReport, QuietHours, RouteOutcome, Severity, StrategyRow,
    TelegramDeliveredRow, TelegramPendingRow, TrackingRow,
};

const S: i64 = 1_000_000_000; // 1s in ns
const MIN: i64 = 60 * S;

/// Convert a Windows path to a WSL path via `wslpath -a`. Falls back to the
/// raw path on native Linux (e.g. CI runners) where `wslpath` is absent.
fn wsl(p: &std::path::Path) -> String {
    let has_wslpath = std::process::Command::new("bash")
        .args(["-c", "command -v wslpath"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_wslpath {
        let out = std::process::Command::new("bash")
            .args(["-c", &format!("wslpath -a '{}'", p.display())])
            .output()
            .expect("wslpath");
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    } else {
        p.display().to_string()
    }
}

// ---- Alert framework (OPS-4, OPS-9) -------------------------------------

#[test]
fn ops_4_alert_dedupes_within_window_then_fires_again() {
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
fn ops_9_quiet_hours_batch_p3_but_p1_breaks_through() {
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

#[test]
fn ops_9_quiet_hours_minutes_until_end() {
    // The `telegram-flush --wait` sleep: minutes until the wrap-around window
    // (22:00–07:00) ends, 0 when now is OUTSIDE it (a late flush drains
    // immediately, never sleeping ~24h).
    let quiet = QuietHours {
        start_min: 22 * 60,
        end_min: 7 * 60,
    };
    assert_eq!(quiet.minutes_until_end(23 * 60 * MIN), 8 * 60); // 23:00 → 08:00
    assert_eq!(quiet.minutes_until_end(60 * MIN), 6 * 60); // 01:00 → 06:00
    assert_eq!(quiet.minutes_until_end(12 * 60 * MIN), 0); // 12:00 outside
    assert_eq!(quiet.minutes_until_end(7 * 60 * MIN), 0); // exactly at end ⇒ no longer quiet

    // A non-wrapping window counts down to its own end_min.
    let day = QuietHours {
        start_min: 2 * 60,
        end_min: 6 * 60,
    };
    assert_eq!(day.minutes_until_end(3 * 60 * MIN), 3 * 60);
    assert_eq!(day.minutes_until_end(60 * MIN), 0); // before start
}

// ---- Dead-man switch (OPS-2) --------------------------------------------

#[test]
fn ops_2_deadman_fires_after_three_missed_beats_and_escalates_in_live() {
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
fn ops_3_kill_latch_roundtrips_and_trips_kill_switches() {
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
fn ops_3_flatten_is_global_kill() {
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
        band_accuracy: vec![
            BandAccuracyRow {
                week: "2026-W25".to_string(),
                observations: 142,
                mean_relative_error: 0.0214,
                coverage: 0.936,
                run_id: None,
            },
            BandAccuracyRow {
                week: "2026-W26".to_string(),
                observations: 98,
                mean_relative_error: 0.0198,
                coverage: 0.948,
                run_id: None,
            },
        ],
        telegram_pending: vec![],
        // One flushed delivery in the delivery log — the fixture shows both
        // sides of delivery accountability: pending (empty) AND delivered.
        telegram_delivered: vec![TelegramDeliveredRow {
            id: "band-accuracy-decay".to_string(),
            delivered_ts_ns: 102 * 86_400 * S,
            delivered_days_ago: 3,
        }],
        benchmark: Benchmark {
            book_return: 0.031,
            btc_hold_return: 0.088,
            tbill_return: 0.004,
        },
    }
}

#[test]
fn ops_6_report_has_all_sections_and_benchmark_row() {
    let md = fixture_report().render_markdown();
    for header in [
        "## Equity & Drawdown",
        "## Expectancy (after costs)",
        "## Tracking Error",
        "## Cost Breakdown",
        "## Funnel Transitions & Kills",
        "## Whale Band Accuracy (RES-4)",
        "## Delivery Accountability (Telegram queue)",
        "### Delivered this month (Telegram delivery log)",
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
fn ops_6_report_numbers_are_grounded_not_invented() {
    // Every percent token in the report must trace to an input figure — a
    // spot-check that the renderer computes nothing on its own (PD-5 honesty).
    let r = fixture_report();
    let md = r.render_markdown();
    assert!(md.contains("+3.10%")); // blended/net return
    assert!(md.contains("-1.20%")); // max drawdown
    assert!(md.contains("-0.90%")); // tracking error
                                    // RES-4 trend numbers render exactly as loaded from band_accuracy.jsonl.
    assert!(md.contains("| 2026-W25 | 142 | 2.14% | 93.6% |"));
    assert!(md.contains("| 2026-W26 | 98 | 1.98% | 94.8% |"));
    // Empty inputs render explicit "no data", never a blank cell.
    let mut empty = fixture_report();
    empty.strategies.clear();
    empty.tracking.clear();
    empty.band_accuracy.clear();
    let md2 = empty.render_markdown();
    assert!(md2.contains("_no data_"));
    assert!(md2.contains("## Whale Band Accuracy (RES-4)"));
}

#[test]
fn ops_6_band_accuracy_trend_loads_from_jsonl_sorted_and_grounded() {
    // The EXACT journal shape `run_band_accuracy.py` appends (sort_keys=True:
    // week/run_id/n/mean_relative_error/coverage/config_hash) — written out
    // of order on disk, so the loader must sort by ISO week.
    let dir = std::env::temp_dir().join(format!("mpba6-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("band_accuracy.jsonl");
    std::fs::write(
        &path,
        concat!(
            "{\"week\":\"2026-W31\",\"run_id\":\"01JBA6TEST0000000000000000\",\"n\":98,\"mean_relative_error\":0.0198,\"coverage\":0.948,\"config_hash\":\"abc123\"}\n",
            "{\"week\":\"2026-W30\",\"run_id\":\"01JBA5TEST0000000000000000\",\"n\":142,\"mean_relative_error\":0.0214,\"coverage\":0.936,\"config_hash\":\"abc123\"}\n",
        ),
    )
    .unwrap();

    let rows = mp_ops::load_band_accuracy_trend(&path).expect("load trend");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].week, "2026-W30", "rows sorted by ISO week");
    assert_eq!(rows[0].observations, 142);
    assert!((rows[0].mean_relative_error - 0.0214).abs() < 1e-12);
    assert!((rows[0].coverage - 0.936).abs() < 1e-12);
    assert_eq!(rows[1].week, "2026-W31");
    assert_eq!(rows[1].observations, 98);
    assert!((rows[1].mean_relative_error - 0.0198).abs() < 1e-12);
    // The run_id echo survives the parse — the join key to the whale_study
    // run's SIM-10 record in runs/index.jsonl (RES-4 tracker).
    assert_eq!(
        rows[0].run_id.as_deref(),
        Some("01JBA5TEST0000000000000000")
    );
    assert_eq!(
        rows[1].run_id.as_deref(),
        Some("01JBA6TEST0000000000000000")
    );

    // Rendered into the report, the numbers appear exactly as loaded.
    let mut r = fixture_report();
    r.band_accuracy = rows;
    let md = r.render_markdown();
    assert!(md.contains("## Whale Band Accuracy (RES-4)"));
    assert!(md.contains("| 2026-W30 | 142 | 2.14% | 93.6% |"));
    assert!(md.contains("| 2026-W31 | 98 | 1.98% | 94.8% |"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_6_band_accuracy_trend_fails_closed_on_corruption_and_missing_is_no_data() {
    // A corrupt line fails the whole load (CONV-8), naming the line — evidence
    // corruption must never become a silently dropped graded week.
    let dir = std::env::temp_dir().join(format!("mpba6b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("band_accuracy.jsonl");
    std::fs::write(
        &path,
        concat!(
            "{\"week\":\"2026-W30\",\"n\":142,\"mean_relative_error\":0.0214,\"coverage\":0.936}\n",
            "not json\n",
        ),
    )
    .unwrap();
    let err = mp_ops::load_band_accuracy_trend(&path).expect_err("corrupt line must fail");
    assert!(
        err.contains("line 2"),
        "error names the corrupt line: {err}"
    );

    // A present-but-type-wrong week also fails closed (coverage out of [0,1]).
    std::fs::write(
        &path,
        "{\"week\":\"2026-W30\",\"n\":142,\"mean_relative_error\":0.0214,\"coverage\":1.7}\n",
    )
    .unwrap();
    assert!(mp_ops::load_band_accuracy_trend(&path).is_err());

    // A missing journal (study not run yet) is a "no data" month, not an
    // error (RES-5) — and the section renders the honest empty line.
    let missing = dir.join("nope.jsonl");
    assert!(mp_ops::load_band_accuracy_trend(&missing)
        .expect("missing journal is a no-data month")
        .is_empty());
    let mut empty = fixture_report();
    empty.band_accuracy.clear();
    assert!(empty
        .render_markdown()
        .contains("| _no data_ | — | — | — |"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_6_telegram_batch_loads_pending_rows_fail_closed() {
    // The delivery-accountability loader reads the quiet-hours batch ledger
    // (`journal/telegram/batch.jsonl`) — the exact shape `append_batch`
    // writes — as pending (un-flushed) dispatches with injected clock age.
    let dir = std::env::temp_dir().join(format!("mptg6-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ledger = dir.join("journal").join("telegram");
    std::fs::create_dir_all(&ledger).unwrap();
    let day = 86_400 * S;
    // Appended OUT of queue order — the loader must sort oldest-first.
    let newer = Alert::new("stream-gap", Severity::P2, MIN, "BTCUSDT gap 6m");
    mp_ops::append_batch(&ledger, &mp_ops::Dispatch::from_alert(&newer, 102 * day)).unwrap();
    let older = Alert::new("band-accuracy-decay", Severity::P3, 0, "RES-4 decay");
    mp_ops::append_batch(&ledger, &mp_ops::Dispatch::from_alert(&older, 100 * day)).unwrap();

    let rows = mp_ops::load_telegram_batch(&ledger, 105 * day).expect("load batch");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "band-accuracy-decay", "sorted oldest-first");
    assert_eq!(rows[0].severity, Severity::P3);
    assert_eq!(rows[0].queued_days, 5); // 105d − 100d
    assert_eq!(rows[1].id, "stream-gap");
    assert_eq!(rows[1].severity, Severity::P2);
    assert_eq!(rows[1].queued_days, 3); // 105d − 102d

    // A corrupt line is evidence corruption: fail closed naming the line
    // (CONV-8) — a queued alert is never silently dropped from the report.
    std::fs::write(ledger.join("batch.jsonl"), "not a dispatch\n").unwrap();
    let err = mp_ops::load_telegram_batch(&ledger, 105 * day).expect_err("corrupt line fails");
    assert!(err.contains("line 1"), "error names the line: {err}");

    // A missing ledger is the healthy "nothing pending" state, not an error.
    let missing = dir.join("nope");
    assert!(mp_ops::load_telegram_batch(&missing, 105 * day)
        .expect("missing ledger is empty")
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_6_telegram_delivered_loads_rows_fail_closed() {
    // The delivery-log loader reads `journal/telegram/delivered.jsonl` — the
    // exact shape `telegram-flush` appends per successful send — as flushed
    // records with injected-clock age (PD-3).
    let dir = std::env::temp_dir().join(format!("mptgd6-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ledger = dir.join("journal").join("telegram");
    std::fs::create_dir_all(&ledger).unwrap();
    let day = 86_400 * S;
    // Appended OUT of delivery order — the loader must sort oldest-first.
    mp_ops::append_delivered(&ledger, "stream-gap", 102 * day).unwrap();
    mp_ops::append_delivered(&ledger, "band-accuracy-decay", 100 * day).unwrap();

    let rows = mp_ops::load_telegram_delivered(&ledger, 105 * day).expect("load delivered");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "band-accuracy-decay", "sorted oldest-first");
    assert_eq!(rows[0].delivered_ts_ns, 100 * day);
    assert_eq!(rows[0].delivered_days_ago, 5); // 105d − 100d
    assert_eq!(rows[1].id, "stream-gap");
    assert_eq!(rows[1].delivered_ts_ns, 102 * day);
    assert_eq!(rows[1].delivered_days_ago, 3); // 105d − 102d

    // A corrupt line is evidence corruption: fail closed naming the line
    // (CONV-8) — a delivery is never silently dropped from the report.
    std::fs::write(ledger.join("delivered.jsonl"), "not a record\n").unwrap();
    let err = mp_ops::load_telegram_delivered(&ledger, 105 * day).expect_err("corrupt line fails");
    assert!(err.contains("line 1"), "error names the line: {err}");

    // A missing log is the healthy "no deliveries recorded" state (the flush
    // has not run, or had nothing to send), not an error.
    let missing = dir.join("nope");
    assert!(mp_ops::load_telegram_delivered(&missing, 105 * day)
        .expect("missing log is empty")
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_6_report_delivery_accountability_section_renders() {
    // A pending queue item AND a flushed delivery render grounded in both
    // formats, markdown cells match the HTML (the OPS-6 parity contract), and
    // the empty ledgers are the explicitly-rendered healthy states.
    let mut r = fixture_report();
    r.telegram_pending = vec![TelegramPendingRow {
        id: "band-accuracy-decay".to_string(),
        severity: Severity::P3,
        detail: "RES-4 band accuracy decaying: coverage trailing 4-wk mean 0.350".to_string(),
        runbook: "ops/runbooks/band-accuracy-decay.md".to_string(),
        ts_ns: 3 * 86_400 * S,
        queued_days: 3,
    }];
    // A same-day delivery exercises the `today` branch of the age renderer.
    r.telegram_delivered.push(TelegramDeliveredRow {
        id: "funnel-move".to_string(),
        delivered_ts_ns: 105 * 86_400 * S,
        delivered_days_ago: 0,
    });
    let md = r.render_markdown();
    assert!(md.contains("## Delivery Accountability (Telegram queue)"));
    assert!(md.contains(
        "| band-accuracy-decay | P3 | 3d | RES-4 band accuracy decaying: coverage trailing 4-wk mean 0.350 | ops/runbooks/band-accuracy-decay.md |"
    ));
    let html = r.render_html();
    assert!(html.contains("<h2>Delivery Accountability (Telegram queue)</h2>"));
    assert!(html.contains("<td>band-accuracy-decay</td>"));
    assert!(html.contains("<td class=\"num\">3d</td>"));
    assert!(html.contains("<td>P3</td>"));
    // Parity: every markdown cell from this section appears in the HTML
    // (including the runbook link — the remediation path for an undelivered
    // alert).
    assert!(html.contains("RES-4 band accuracy decaying: coverage trailing 4-wk mean 0.350"));
    assert!(html.contains("<td>ops/runbooks/band-accuracy-decay.md</td>"));

    // Delivery log — what WAS delivered (OPS-6): the fixture's flushed
    // record renders grounded in both formats, the age from the loader's
    // injected clock (3d ago: delivered_ts_ns 102d, now 105d).
    assert!(md.contains("### Delivered this month (Telegram delivery log)"));
    assert!(md.contains("| band-accuracy-decay | 3d ago |"));
    assert!(md.contains("| funnel-move | today |"));
    assert!(html.contains("<h3>Delivered this month (Telegram delivery log)</h3>"));
    assert!(html.contains("<td>3d ago</td>"));
    assert!(html.contains("<td>today</td>"));

    // Empty ledgers, rendered strictly grounded: nothing pending NOW and no
    // deliveries recorded (RES-5) — never an inference about the past.
    let mut e = fixture_report();
    e.telegram_pending.clear();
    e.telegram_delivered.clear();
    assert!(e
        .render_markdown()
        .contains("_No pending alerts in the quiet-hours Telegram queue._"));
    assert!(e
        .render_html()
        .contains("No pending alerts in the quiet-hours Telegram queue."));
    assert!(e
        .render_markdown()
        .contains("_No deliveries recorded in the Telegram delivery log._"));
    assert!(e
        .render_html()
        .contains("No deliveries recorded in the Telegram delivery log."));

    // A hostile detail/runbook/id cannot break the HTML (5-entity escaping,
    // same as the other dynamic strings).
    let mut hostile = fixture_report();
    hostile.telegram_pending = vec![TelegramPendingRow {
        id: "x".to_string(),
        severity: Severity::P3,
        detail: "<script>alert(1)</script>".to_string(),
        runbook: "<b>r</b>".to_string(),
        ts_ns: 0,
        queued_days: 1,
    }];
    hostile.telegram_delivered = vec![TelegramDeliveredRow {
        id: "<script>alert(2)</script>".to_string(),
        delivered_ts_ns: 0,
        delivered_days_ago: 1,
    }];
    let html = hostile.render_html();
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("&lt;script&gt;alert(2)&lt;/script&gt;"));
    assert!(
        html.contains("&lt;b&gt;r&lt;/b&gt;"),
        "runbook is escaped too"
    );
    assert!(
        !html.contains("<script>"),
        "raw markup must never reach the HTML"
    );
}

/// Audit bug 3: all dead-man alerts share the id "process-deadman", and the
/// router deduped per id — so process B dying inside process A's dedupe
/// window was silently suppressed. Dedupe is now per entity key.
#[test]
fn regression_audit3_deadman_alerts_not_cross_deduped() {
    let mut dm = DeadMan::new(30 * S);
    dm.register("collector-bybit", false, 0);
    dm.register("oms", true, 0);
    let alerts = dm.check(100 * S, false);
    assert_eq!(alerts.len(), 2);

    let mut r = AlertRouter::new(None);
    // Both silent processes must alert — the second is a different entity,
    // not a duplicate of the first.
    assert!(matches!(r.route(&alerts[0], 0), RouteOutcome::Sent(_)));
    assert!(matches!(r.route(&alerts[1], 0), RouteOutcome::Sent(_)));
    // The SAME process re-alerting inside the window is still deduped.
    assert_eq!(r.route(&alerts[0], 10 * S), RouteOutcome::Deduped);
}

// ---- OPS-3 end-to-end: /kill latch → real risk gate RG-10 verdict --------

#[test]
fn ops_3_kill_latch_makes_the_real_gate_reject_with_rg10() {
    use mp_core::{Side, StrategyId, SymbolId};
    use mp_risk::{evaluate, GateInput, Mode, RejectReason, RiskLimits, Verdict};

    let sym = SymbolId(0);
    let strat = StrategyId::new("carry-v1");
    let allowed = [(Venue::Bybit, sym)];
    let base = GateInput {
        mode: Mode::Paper,
        venue: Venue::Bybit,
        symbol: sym,
        strategy: strat.clone(),
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
        allowed: &allowed,
        open_positions: 0,
        corr_adjusted_exposure_notional: 0.0,
    };
    let limits = RiskLimits::default();

    // Before the latch: a normal order passes the gate.
    let no_kills = mp_risk::KillSwitches::new();
    assert_eq!(evaluate(&limits, &no_kills, &base), Verdict::Pass);

    // The phone writes a GLOBAL /kill latch; the gate loads it and now rejects
    // the very next intent with the RG-10 verdict — no RPC to oms involved.
    let latch = KillLatch::global("phone /flatten", 1);
    let kills = latch.to_kill_switches();
    assert_eq!(
        evaluate(&limits, &kills, &base),
        Verdict::Reject(RejectReason::KillSwitchTripped)
    );

    // A venue-scoped latch blocks that venue but not another.
    let venue_latch = KillLatch::new("kill bybit", 2).kill(LatchScope::Venue {
        venue: Venue::Bybit,
    });
    let vkills = venue_latch.to_kill_switches();
    assert_eq!(
        evaluate(&limits, &vkills, &base),
        Verdict::Reject(RejectReason::KillSwitchTripped)
    );
}

// ---- Registry ↔ runbooks (OPS-4) ----------------------------------------

#[test]
fn ops_4_every_catalog_alert_has_a_runbook_file() {
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

// ---- OPS-7: host-health watch checks --------------------------------------

#[test]
fn ops_7_disk_clock_and_keyfile_checks_alert_past_thresholds() {
    use mp_ops::{clock_skew_alert, disk_alert, keyfile_perms_alert};
    // Disk: 86% > 85% budget fires disk-high; 84% does not. Alert only — the
    // checker can never delete data (W-6).
    let a = disk_alert(0.86, 0.85, MIN).expect("fires past budget");
    assert_eq!(a.id, "disk-high");
    assert_eq!(a.severity, Severity::P2);
    assert!(disk_alert(0.84, 0.85, MIN).is_none());

    // Clock: 150ms skew fires; 50ms does not; sign is irrelevant.
    assert!(clock_skew_alert(150_000_000, MIN).is_some());
    assert!(clock_skew_alert(-150_000_000, MIN).is_some());
    assert!(clock_skew_alert(50_000_000, MIN).is_none());

    // Key files: group/other-readable fires per-file; 0600 passes.
    let k = keyfile_perms_alert("/etc/mp/ops.env", 0o644, MIN).expect("fires");
    assert_eq!(k.id, "keyfile-perms");
    assert_eq!(k.dedupe_key, "keyfile-perms//etc/mp/ops.env");
    assert!(keyfile_perms_alert("/etc/mp/ops.env", 0o600, MIN).is_none());
}

// ---- OPS-1/5/8/10: deployment artifacts are present and well-formed --------

#[test]
fn ops_1_systemd_units_pin_restart_and_resource_limits() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for unit in ["systemd/collector@.service", "systemd/opsd.service"] {
        let text = std::fs::read_to_string(root.join(unit)).expect(unit);
        assert!(
            text.contains("Restart=always"),
            "{unit}: restart=always (OPS-1)"
        );
        assert!(text.contains("MemoryMax="), "{unit}: memory limit (OPS-1)");
        assert!(text.contains("CPUQuota="), "{unit}: cpu limit (OPS-1)");
    }
}

#[test]
fn ops_5_restore_drill_script_exists_and_refuses_without_backup() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        root.join("restore-drill.sh").exists(),
        "ops/restore-drill.sh must exist (OPS-5)"
    );
    // No backup argument ⇒ usage error (exit 2), never a fake PASS.
    let out = std::process::Command::new("bash")
        .args(["-c", &format!("'{}'", wsl(&root.join("restore-drill.sh")))])
        .output()
        .expect("run script");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ops_8_deploy_doc_and_compose_are_checked_in() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(root.join("deploy.md").exists(), "ops/deploy.md (OPS-8)");
    let compose = std::fs::read_to_string(root.join("compose.yaml")).expect("compose");
    assert!(
        compose.contains("restart: always"),
        "compose restart policy (OPS-1)"
    );
}

#[test]
fn ops_10_process_log_rotation_is_configured() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let compose = std::fs::read_to_string(root.join("compose.yaml")).expect("compose");
    // Process logs are bounded/rotated (journals are append-only forever, W-6).
    assert!(compose.contains("max-size"), "log rotation bound (OPS-10)");
    assert!(compose.contains("max-file"), "log rotation count (OPS-10)");
}

// ---- OPS-3: bot command surface, allowlist, confirm flows, journaling ------

#[test]
fn ops_3_bot_allowlists_owner_and_journals_every_command() {
    use mp_ops::Bot;
    let mut bot = Bot::new(777);
    // A non-owner cannot command — and the attempt is journaled (evidence).
    let r = bot.handle(666, "/kill GLOBAL", 1);
    assert_eq!(r.text, "not authorized");
    assert!(r.latch.is_none());
    // Owner read-only commands are acknowledged and journaled.
    for cmd in ["/status", "/positions", "/funnel", "/report"] {
        assert!(bot.handle(777, cmd, 2).latch.is_none());
    }
    assert!(bot
        .journal()
        .iter()
        .any(|l| l.contains("REFUSED non-owner")));
    assert!(bot.journal().len() >= 5, "every command journaled (OPS-3)");
}

#[test]
fn ops_3_kill_needs_confirm_and_flatten_needs_double_confirm() {
    use mp_core::{Side, StrategyId, SymbolId};
    use mp_ops::Bot;
    use mp_risk::{evaluate, GateInput, Mode, RejectReason, RiskLimits, Verdict};

    let mut bot = Bot::new(1);
    // /kill GLOBAL: no latch until "yes".
    assert!(bot.handle(1, "/kill GLOBAL", 1).latch.is_none());
    let confirmed = bot.handle(1, "yes", 2);
    let latch = confirmed.latch.expect("latch after confirm");

    // The latch reaches the REAL gate: next intent rejected with RG-10.
    let sym = SymbolId(0);
    let allowed = [(Venue::Bybit, sym)];
    let verdict = evaluate(
        &RiskLimits::default(),
        &latch.to_kill_switches(),
        &GateInput {
            mode: Mode::Paper,
            venue: Venue::Bybit,
            symbol: sym,
            strategy: StrategyId::new("carry-v1"),
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
            allowed: &allowed,
            open_positions: 0,
            corr_adjusted_exposure_notional: 0.0,
        },
    );
    assert_eq!(verdict, Verdict::Reject(RejectReason::KillSwitchTripped));

    // /flatten needs TWO yes replies; a decline aborts.
    let mut bot2 = mp_ops::Bot::new(1);
    assert!(bot2.handle(1, "/flatten", 1).latch.is_none());
    assert!(bot2.handle(1, "yes", 2).latch.is_none()); // 1/2
    let done = bot2.handle(1, "yes", 3);
    assert!(done.latch.is_some()); // 2/2 ⇒ global latch
    let mut bot3 = mp_ops::Bot::new(1);
    bot3.handle(1, "/flatten", 1);
    assert!(bot3.handle(1, "no", 2).latch.is_none()); // aborted
}

#[test]
fn ops_5_restore_drill_restores_a_backup_and_verifies() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("mpdrill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("stage/journal")).unwrap();
    std::fs::create_dir_all(dir.join("stage/runs")).unwrap();
    std::fs::write(dir.join("stage/journal/briefs.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.join("stage/runs/index.jsonl"), "{}\n").unwrap();
    let tarball = dir.join("backup.tar.gz");
    let tar = std::process::Command::new("tar")
        .args([
            "-czf",
            tarball.to_str().unwrap(),
            "-C",
            dir.join("stage").to_str().unwrap(),
            "journal",
            "runs",
        ])
        .status()
        .expect("tar");
    assert!(tar.success());

    // Full restore path with an injected verifier (the default verifier is the
    // sim golden fixture; injecting avoids nesting cargo inside cargo test).
    let ok = std::process::Command::new("bash")
        .args([
            "-c",
            &format!(
                "MP_DRILL_VERIFY_CMD=true '{}' '{}'",
                wsl(&root.join("restore-drill.sh")),
                wsl(&tarball)
            ),
        ])
        .output()
        .expect("run drill");
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );

    // A backup missing the business records must FAIL the drill.
    let bad = dir.join("bad.tar.gz");
    std::process::Command::new("tar")
        .args([
            "-czf",
            bad.to_str().unwrap(),
            "-C",
            dir.join("stage").to_str().unwrap(),
            "runs",
        ])
        .status()
        .expect("tar bad");
    let fail = std::process::Command::new("bash")
        .args([
            "-c",
            &format!(
                "MP_DRILL_VERIFY_CMD=true '{}' '{}'",
                wsl(&root.join("restore-drill.sh")),
                wsl(&bad)
            ),
        ])
        .output()
        .expect("run drill bad");
    assert!(
        !fail.status.success(),
        "missing journal/ must fail the drill"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_6_report_html_has_all_sections_and_grounded_numbers() {
    let html = fixture_report().render_html();
    for heading in [
        "Equity &amp; Drawdown",
        "Expectancy (after costs)",
        "Tracking Error (live vs paper vs backtest)",
        "Cost Breakdown",
        "Funnel Transitions &amp; Kills",
        "Whale Band Accuracy (RES-4)",
        "Delivery Accountability (Telegram queue)",
        "Benchmark",
    ] {
        assert!(
            html.contains(&format!("<h2>{heading}</h2>")),
            "missing HTML section: {heading}"
        );
    }
    // The numbers are grounded — the same tokens the markdown render emits.
    assert!(html.contains("+3.10%")); // blended/net return
    assert!(html.contains("-1.20%")); // max drawdown
    assert!(html.contains("2.14%") && html.contains("93.6%")); // band accuracy rows
    assert!(html.contains("BTC hold")); // benchmark row (REQUIRED)
    assert!(html.contains("<table>"));
    assert!(html.contains("</html>"));
    // The delivery log renders what WAS delivered (OPS-6) — an h3 sub-block
    // inside the delivery-accountability section.
    assert!(html.contains("<h3>Delivered this month (Telegram delivery log)</h3>"));
    // A full fixture has no explicit no-data table rows (the delivery
    // section's empty state is a healthy-state paragraph, not a row).
    assert!(!html.contains("<tr class=\"nodata\""));
}

#[test]
fn ops_6_report_html_renders_no_data_and_escapes_dynamic_strings() {
    let mut empty = fixture_report();
    empty.strategies.clear();
    empty.tracking.clear();
    empty.funnel.clear();
    empty.band_accuracy.clear();
    let html = empty.render_html();
    assert!(
        html.contains("class=\"nodata\""),
        "explicit no-data rows (RES-5)"
    );
    assert!(html.contains("No transitions this month.")); // A hostile strategy name cannot break out of the document — all five
                                                          // entities (`& < > " '`) are escaped, and raw markup never reaches the HTML.
    let mut r = fixture_report();
    r.strategies[0].strategy = "<script>alert(1)</script>&\"'".to_string();
    let html = r.render_html();
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;&amp;&quot;&#39;"));
    assert!(
        !html.contains("<script>"),
        "raw markup must never reach the HTML"
    );
    assert!(
        !html.contains("&\"'"),
        "raw ampersand/quote entities must never reach the HTML"
    );
}

#[test]
fn ops_6_report_html_matches_markdown_cells() {
    // Drift guard between the two renderers (OPS-6: HTML shows the SAME
    // grounded numbers as the markdown): every non-placeholder cell in the
    // markdown's tables must also appear in the HTML — a one-sided edit to a
    // section (new column, renamed header, changed format) fails here.
    let r = fixture_report();
    let md = r.render_markdown();
    let html = r.render_html();
    for line in md.lines().filter(|l| l.starts_with('|')) {
        for cell in line.split('|').skip(1) {
            let cell = cell.trim();
            if cell.is_empty() || cell == "—" || cell.starts_with("---") {
                continue;
            }
            assert!(
                html.contains(cell),
                "markdown cell {cell:?} missing from the HTML render"
            );
        }
    }
}

#[test]
fn ops_6_report_writes_markdown_and_html_to_month_dir() {
    // spec 009: rendered to markdown + HTML in journal/reports/{YYYY-MM}/.
    let dir = std::env::temp_dir().join(format!("mpreport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let month_dir = dir.join("journal").join("reports").join("2026-06");
    let (md, html) =
        mp_ops::write_monthly_report(&month_dir, &fixture_report()).expect("write report");
    assert_eq!(md.file_name().unwrap(), "report.md");
    assert_eq!(html.file_name().unwrap(), "report.html");
    let md_text = std::fs::read_to_string(&md).unwrap();
    let html_text = std::fs::read_to_string(&html).unwrap();
    assert!(md_text.starts_with("# Monthly Report — 2026-06"));
    assert!(html_text.contains("<!DOCTYPE html>"));
    assert!(html_text.contains("<h1>Monthly Report — 2026-06</h1>"));
    assert!(md_text.contains("## Whale Band Accuracy (RES-4)"));
    assert!(html_text.contains("<h2>Whale Band Accuracy (RES-4)</h2>"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- OPS-13: weekly band-accuracy drift/decay watch ------------------------

/// A flat trend of `weeks` graded rows numbered 2026-W01.. — the caller
/// overrides the trailing weeks for a decay signature.
fn trend_rows(weeks: usize, coverage: f64, mre: f64) -> Vec<BandAccuracyRow> {
    (0..weeks)
        .map(|i| BandAccuracyRow {
            week: format!("2026-W{:02}", i + 1),
            observations: 100,
            mean_relative_error: mre,
            coverage,
            run_id: None,
        })
        .collect()
}

#[test]
fn ops_13_band_accuracy_decay_alerts_on_sustained_quality_loss() {
    // 8 healthy weeks (94% coverage, 2.0% MRE) then 4 degraded weeks: coverage
    // 0.45/0.40/0.30/0.25 (trailing mean 0.350 < half of 0.940), MRE
    // 0.06/0.08/0.10/0.12 (trailing mean 0.090 > double 0.020) — both metrics
    // flag under the RES-3 trailing-window semantics (OPS-13).
    let mut rows = trend_rows(8, 0.94, 0.020);
    for (i, (cov, mre)) in [(0.45, 0.06), (0.40, 0.08), (0.30, 0.10), (0.25, 0.12)]
        .into_iter()
        .enumerate()
    {
        rows.push(BandAccuracyRow {
            week: format!("2026-W{:02}", 9 + i),
            observations: 100,
            mean_relative_error: mre,
            coverage: cov,
            run_id: None,
        });
    }
    let alert =
        mp_ops::band_accuracy_decay_alert(&rows, 7 * 24 * 3600 * S).expect("decay must fire");
    assert_eq!(alert.id, "band-accuracy-decay");
    assert_eq!(alert.severity, Severity::P3);
    assert_eq!(alert.runbook, "ops/runbooks/band-accuracy-decay.md");
    assert!(alert.detail.contains("coverage trailing 4-wk mean 0.350"));
    assert!(alert.detail.contains("MRE trailing 4-wk mean 0.090"));

    // A P3 routes through the router (daytime ⇒ sent on the quiet channel).
    let mut router = AlertRouter::new(None);
    assert!(matches!(
        router.route(&alert, 12 * 60 * MIN),
        RouteOutcome::Sent(_)
    ));
}

#[test]
fn ops_13_band_accuracy_decay_ignores_healthy_and_young_trends() {
    // Steady healthy trend ⇒ no flag.
    assert!(mp_ops::band_accuracy_decay_alert(&trend_rows(12, 0.94, 0.020), MIN).is_none());
    // < 12 graded weeks ⇒ not enough history (RES-3).
    assert!(mp_ops::band_accuracy_decay_alert(&trend_rows(11, 0.94, 0.020), MIN).is_none());
    // A grade that was never good (baseline coverage 0.40 < 0.5) is not
    // "decay" — even when the trailing weeks collapse further.
    let mut never_good = trend_rows(8, 0.40, 0.020);
    for i in 0..4 {
        never_good.push(BandAccuracyRow {
            week: format!("2026-W{:02}", 9 + i),
            observations: 100,
            mean_relative_error: 0.02,
            coverage: 0.10,
            run_id: None,
        });
    }
    assert!(mp_ops::band_accuracy_decay_alert(&never_good, MIN).is_none());
    // Empty/absent trend ⇒ no flag (nothing to decay).
    assert!(mp_ops::band_accuracy_decay_alert(&[], MIN).is_none());
}

#[test]
fn ops_13_weekly_wrapper_invokes_decay_check_after_study() {
    // After the study, `run_whale_study_weekly.sh` runs `mp-ops
    // band-accuracy-decay --trend <out>/band_accuracy.jsonl` on the freshly
    // updated journal (OPS-13). A stub MP_OPS_CMD proves the wiring; a
    // missing binary skips gracefully — best-effort P3, never a study failure.
    let data = std::env::temp_dir().join(format!("mpops13d-{}", std::process::id()));
    let out = std::env::temp_dir().join(format!("mpops13o-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("20260803_hyperliquid_positions.log"), "").unwrap();
    std::fs::write(data.join("20260803_hyperliquid_BTC.log"), "").unwrap();
    let runs = data.join("runs");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/run_whale_study_weekly.sh");

    // Hook present: the wrapper invokes the check with the trend path.
    let hook = std::process::Command::new("bash")
        .args(["-c", &format!(
            "MP_DATA_DIR='{}' MP_OUT_DIR='{}' MP_RUNS_DIR='{}' MP_PYTHON=/bin/echo MP_OPS_CMD=/bin/echo MP_REPO_DIR='{}' '{}'",
            wsl(&data), wsl(&out), wsl(&runs), wsl(&data), wsl(&script)
        )])
        .output()
        .expect("run wrapper (decay hook)");
    assert_eq!(
        hook.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let stdout = String::from_utf8_lossy(&hook.stdout);
    assert!(
        stdout.contains("band-accuracy-decay") && stdout.contains("--trend"),
        "decay check invoked: {stdout}"
    );
    assert!(
        stdout.contains("band_accuracy.jsonl"),
        "trend journal wired: {stdout}"
    );
    assert!(
        stdout.contains("--runs-dir"),
        "the decay check journals its verdict to runs/index.jsonl: {stdout}"
    );

    // Hook absent: best-effort skip, the study's exit 0 still stands.
    let skip = std::process::Command::new("bash")
        .args(["-c", &format!(
            "MP_DATA_DIR='{}' MP_OUT_DIR='{}' MP_RUNS_DIR='{}' MP_PYTHON=/bin/echo MP_OPS_CMD=/opt/mp/does-not-exist MP_REPO_DIR='{}' '{}'",
            wsl(&data), wsl(&out), wsl(&runs), wsl(&data), wsl(&script)
        )])
        .output()
        .expect("run wrapper (decay skip)");
    assert_eq!(
        skip.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&skip.stderr)
    );
    assert!(
        String::from_utf8_lossy(&skip.stdout).contains("skipping the band-accuracy decay check")
    );
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&out);
}

// ---- OPS-9: band-accuracy-decay Telegram edge (Bot API + quiet hours) -----

/// A 12-row decayed `band_accuracy.jsonl` (8 healthy weeks, then 4 collapsed)
/// — enough to fire `band-accuracy-decay` (OPS-13) for the edge tests.
fn decayed_trend(dir: &std::path::Path) -> std::path::PathBuf {
    let mut lines = Vec::new();
    for w in 1..=8 {
        lines.push(format!(
            "{{\"week\":\"2026-W{w:02}\",\"n\":100,\"mean_relative_error\":0.02,\"coverage\":0.94}}"
        ));
    }
    for w in 9..=12 {
        lines.push(format!(
            "{{\"week\":\"2026-W{w:02}\",\"n\":100,\"mean_relative_error\":0.12,\"coverage\":0.25}}"
        ));
    }
    let path = dir.join("band_accuracy.jsonl");
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

/// The Telegram edge shells out to `curl` (the host's TLS stack). On a host
/// without curl the spawned binary would fail with a confusing "curl spawn
/// failed" — skip the e2e send tests instead, so CI without curl still sees
/// the batching/ledger logic covered by the in-process tests above.
fn curl_available() -> bool {
    std::process::Command::new("curl")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A local TCP stub standing in for the Telegram Bot API. Returns the base
/// URL and a handle to the captured HTTP request (exactly one is served).
fn stub_telegram_server() -> (String, std::thread::JoinHandle<String>) {
    stub_telegram_server_with_body(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
}

/// A stub serving `n` Telegram API requests, one ok response each. Returns
/// the base URL and the concatenated captured requests — used when a flush
/// sends more than one dispatch (each is its own curl POST).
fn stub_telegram_server_n(n: usize) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut seen = String::new();
        for _ in 0..n {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 16 * 1024];
            let got = stream.read(&mut buf).unwrap_or(0);
            seen.push_str(&String::from_utf8_lossy(&buf[..got]));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}");
            let _ = stream.flush();
        }
        seen
    });
    (format!("http://{addr}"), handle)
}

/// A stub that answers non-ok (a failed delivery) — same shape, "ok" absent.
fn stub_telegram_server_with_body(
    response: &'static [u8],
) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 16 * 1024];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let _ = stream.write_all(response);
        let _ = stream.flush();
        req
    });
    (format!("http://{addr}"), handle)
}

#[test]
fn ops_9_mp_ops_decay_telegram_sends_via_bot_api_edge() {
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let dir = std::env::temp_dir().join(format!("mptg9-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let trend = decayed_trend(&dir);
    let (url, handle) = stub_telegram_server();

    // Outside quiet hours (start == end ⇒ never quiet) the P3 is sent now.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args([
            "band-accuracy-decay",
            "--trend",
            trend.to_str().unwrap(),
            "--telegram",
        ])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("MP_OPS_TELEGRAM_URL", &url)
        .env("MP_OPS_QUIET_START_MIN", "0")
        .env("MP_OPS_QUIET_END_MIN", "0")
        .output()
        .expect("run mp-ops");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"telegram\":\"sent\""), "{stdout}");

    let req = handle.join().unwrap();
    assert!(req.contains("/botTESTTOKEN/sendMessage"), "{req}");
    assert!(req.contains("chat_id=12345"), "{req}");
    assert!(req.contains("disable_notification=true"), "{req}");
    assert!(req.contains("band-accuracy-decay"), "{req}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_9_mp_ops_decay_telegram_batches_during_quiet_hours_then_flushes() {
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let dir = std::env::temp_dir().join(format!("mptg9b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let trend = decayed_trend(&dir);
    let tg_dir = dir.join("journal").join("telegram");

    // All-day quiet (0..1440) ⇒ the P3 is batched, not sent.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args([
            "band-accuracy-decay",
            "--trend",
            trend.to_str().unwrap(),
            "--telegram",
        ])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("MP_OPS_QUIET_START_MIN", "0")
        .env("MP_OPS_QUIET_END_MIN", "1440")
        .env("MP_OPS_TELEGRAM_DIR", tg_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (batched)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"telegram\":\"batched\""), "{stdout}");
    let batch = tg_dir.join("batch.jsonl");
    let ledger = std::fs::read_to_string(&batch).expect("batch ledger written");
    assert!(ledger.contains("band-accuracy-decay"), "{ledger}");

    // telegram-flush drains the ledger to the Bot API and removes it.
    let (url, handle) = stub_telegram_server();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args(["telegram-flush"])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("MP_OPS_TELEGRAM_URL", &url)
        .env("MP_OPS_TELEGRAM_DIR", tg_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (flush)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"flushed\":1"), "{stdout}");
    assert!(!batch.exists(), "batch ledger removed after flush");
    let req = handle.join().unwrap();
    assert!(req.contains("disable_notification=true"), "{req}");
    // The delivery log records what WAS delivered (id + when) — the flush's
    // proof of delivery, the report's delivery log (OPS-6).
    let delivered = tg_dir.join("delivered.jsonl");
    let log = std::fs::read_to_string(&delivered).expect("delivery log written");
    let rec: serde_json::Value =
        serde_json::from_str(log.lines().next().expect("one delivered line")).unwrap();
    assert_eq!(rec["id"], "band-accuracy-decay");
    assert!(rec["delivered_ts_ns"].as_i64().is_some(), "{rec}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_9_telegram_flush_wait_holds_until_quiet_end_then_drains() {
    // The weekly wrapper's quiet-hours wait, moved into the binary: with
    // --wait inside the window, telegram-flush sleeps until quiet end, then
    // drains. MP_OPS_SLEEP=/bin/echo stubs the sleeper so the test never
    // blocks on a real multi-hour wait.
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let dir = std::env::temp_dir().join(format!("mptg9w2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tg_dir = dir.join("journal").join("telegram");
    std::fs::create_dir_all(&tg_dir).unwrap();
    // Two batched P3 dispatches in the ledger, written through `append_batch`
    // (the exact shape `band-accuracy-decay --telegram` persists) — two
    // physical lines proves the JSONL newline contract: one concatenated blob
    // would corrupt every later reader.
    let batch = tg_dir.join("batch.jsonl");
    for detail in ["RES-4 band accuracy decaying", "funnel: carry-v1 promoted"] {
        let alert = Alert::new("band-accuracy-decay", Severity::P3, 0, detail);
        mp_ops::append_batch(&tg_dir, &mp_ops::Dispatch::from_alert(&alert, 0)).unwrap();
    }

    // All-day quiet (0..1440) ⇒ now is inside ⇒ --wait computes a wait; the
    // MP_OPS_SLEEP seam swallows it; then the ledger drains to the stub (two
    // dispatches ⇒ two POSTs, so the stub serves two connections).
    let (url, handle) = stub_telegram_server_n(2);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args(["telegram-flush", "--wait"])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("MP_OPS_TELEGRAM_URL", &url)
        .env("MP_OPS_TELEGRAM_DIR", tg_dir.to_str().unwrap())
        .env("MP_OPS_QUIET_START_MIN", "0")
        .env("MP_OPS_QUIET_END_MIN", "1440")
        .env("MP_OPS_SLEEP", "/bin/echo")
        .output()
        .expect("run mp-ops (flush --wait)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("flushing in"),
        "the wait is announced before sleeping: {stdout}"
    );
    assert!(stdout.contains("\"flushed\":2"), "{stdout}");
    assert!(!batch.exists(), "batch ledger removed after flush");
    let req = handle.join().unwrap();
    assert_eq!(
        req.matches("/sendMessage").count(),
        2,
        "both dispatches delivered: {req}"
    );
    assert!(req.contains("chat_id=12345"), "{req}");
    // Both deliveries are in the delivery log — one flushed record each.
    let delivered = tg_dir.join("delivered.jsonl");
    let log = std::fs::read_to_string(&delivered).expect("delivery log written");
    assert_eq!(log.lines().count(), 2, "each delivery logged: {log}");
    assert!(log.contains("band-accuracy-decay"), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_9_mp_ops_telegram_send_delivers_immediately_even_in_quiet_hours() {
    // `telegram-send` is the one-shot wrapper-verdict edge (the daily
    // pipeline's promotion-gate verdict): NO quiet-hours batching — a verdict
    // produced at 00:05 UTC (inside 22:00-07:00) must land now, not sit in
    // the ledger until the next flush. All-day quiet (0..1440) must still
    // deliver immediately.
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let (url, handle) = stub_telegram_server();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args([
            "telegram-send",
            "--id",
            "daily-pipeline",
            "--detail",
            "day 2026-08-09: 2 recording(s) | streak 0/7",
            "--severity",
            "p2",
        ])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("MP_OPS_TELEGRAM_URL", &url)
        .env("MP_OPS_QUIET_START_MIN", "0")
        .env("MP_OPS_QUIET_END_MIN", "1440")
        .output()
        .expect("run mp-ops telegram-send");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"sent\":true"), "{stdout}");
    assert!(stdout.contains("\"severity\":\"P2\""), "{stdout}");
    // P2's channel is Telegram (immediate); the point is it is NOT batched
    // despite the all-day quiet env — the dispatch is sent now.
    assert!(stdout.contains("\"channel\":\"telegram\""), "{stdout}");

    let req = handle.join().unwrap();
    assert!(req.contains("/botTESTTOKEN/sendMessage"), "{req}");
    assert!(req.contains("chat_id=12345"), "{req}");
    assert!(req.contains("disable_notification=true"), "{req}");
    // Body is URL-encoded by curl --data-urlencode: spaces => '+', '/' => %2F.
    assert!(req.contains("day+2026-08-09"), "{req}");
    assert!(req.contains("streak+0%2F7"), "{req}");
    assert!(req.contains("daily-pipeline"), "{req}");
}

#[test]
fn ops_9_mp_ops_telegram_send_fails_closed_without_credentials() {
    // Unset credentials ⇒ exit 2 with a named reason (CONV-8) — never a
    // silent drop and never a fake "sent". Same posture as telegram-flush.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args([
            "telegram-send",
            "--id",
            "daily-pipeline",
            "--detail",
            "day 2026-08-09",
        ])
        .env_remove("TELEGRAM_BOT_TOKEN")
        .env_remove("TELEGRAM_CHAT_ID")
        .output()
        .expect("run mp-ops telegram-send (no creds)");
    assert_eq!(
        out.status.code(),
        Some(2),
        "missing creds must fail the job: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID"),
        "failure names the missing vars"
    );
}

#[test]
fn ops_9_mp_ops_telegram_send_rejects_bad_severity() {
    // An unknown --severity is a hard error, never silently defaulted.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args([
            "telegram-send",
            "--id",
            "daily-pipeline",
            "--detail",
            "x",
            "--severity",
            "p9",
        ])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .output()
        .expect("run mp-ops telegram-send (bad severity)");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown --severity"),
        "{} ",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn ops_9_telegram_flush_failure_keeps_batch_and_never_logs_delivered() {
    // A failed delivery is NEVER recorded in the delivery log: the dispatch
    // stays in the batch ledger (pending — visible in the report's queue)
    // and the command exits 2 (CONV-8). At-most-once per attempt, evidence
    // never lost, and the log never lies about what was delivered.
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let dir = std::env::temp_dir().join(format!("mptg9e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tg_dir = dir.join("journal").join("telegram");
    std::fs::create_dir_all(&tg_dir).unwrap();
    let batch = tg_dir.join("batch.jsonl");
    let alert = Alert::new(
        "band-accuracy-decay",
        Severity::P3,
        0,
        "RES-4 band accuracy decaying",
    );
    mp_ops::append_batch(&tg_dir, &mp_ops::Dispatch::from_alert(&alert, 0)).unwrap();

    // The API answers non-ok (`{"ok":false}` is 12 bytes — correct
    // Content-Length, so the failure is the API's verdict, not a transfer
    // truncation).
    let (url, handle) = stub_telegram_server_with_body(
        b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n{\"ok\":false}",
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args(["telegram-flush"])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("MP_OPS_TELEGRAM_URL", &url)
        .env("MP_OPS_TELEGRAM_DIR", tg_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (flush failure)");
    assert_eq!(
        out.status.code(),
        Some(2),
        "a failed send is a failed job: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("telegram api non-ok"),
        "failure surfaces in stderr"
    );
    let _ = handle.join().unwrap();

    // The dispatch stays pending in the batch ledger and is NOT in the
    // delivery log — the report shows it as a gap, never as delivered.
    assert!(batch.exists(), "failed dispatch stays in the batch ledger");
    assert!(std::fs::read_to_string(&batch)
        .unwrap()
        .contains("band-accuracy-decay"));
    assert!(
        !tg_dir.join("delivered.jsonl").exists(),
        "a failed delivery is never logged as delivered"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- OPS-15: storage-budget growth watch -----------------------------------

#[test]
fn ops_15_storage_budget_fires_when_growth_trends_to_cap() {
    // A corpus adding a constant 1 GB/day over 6 days (cumulative 6 GB),
    // budget 10 GB: 4 GB remaining / 1 GB/day ⇒ 4 days ≤ the 14-day horizon
    // ⇒ storage-budget P2. A CONSTANT rate is the case a trend-of-the-rate
    // (slope) model would miss — the disk still fills in a predictable time.
    let day = 20_700; // arbitrary epoch days
    let samples: Vec<(i64, u64)> = (0..6).map(|i| (day + i, 1_000_000_000u64)).collect();
    let p = mp_ops::project_storage(&samples, 10_000_000_000, 7).expect("samples project");
    assert_eq!(p.current_bytes, 6_000_000_000);
    assert_eq!(p.growth_bytes_per_day, Some(1_000_000_000.0));
    assert_eq!(p.days_to_cap, Some(4.0));

    let a = mp_ops::storage_budget_alert(
        &samples,
        10_000_000_000,
        7,
        14.0,
        86_400_000_000_000,
        "data/raw",
    )
    .expect("growing toward the cap within the horizon must alert");
    assert_eq!(a.id, "storage-budget");
    assert_eq!(a.severity, Severity::P2);
    assert_eq!(a.runbook, "ops/runbooks/storage-budget.md");
    assert!(a.detail.contains("6.0 GB used"), "{}", a.detail);
    assert!(a.detail.contains("1.00 GB/day"), "{}", a.detail);
    assert!(a.detail.contains("in 4.0 days"), "{}", a.detail);

    // At/over the cap fires regardless of the growth rate (the corpus is
    // there — the rate no longer matters).
    let over = vec![(day, 25_000_000_000u64), (day + 1, 24_000_000_000)];
    let a = mp_ops::storage_budget_alert(
        &over,
        20_000_000_000,
        7,
        14.0,
        86_400_000_000_000,
        "data/raw",
    )
    .expect("at/over the cap must alert");
    assert!(a.detail.contains("at/over"), "{}", a.detail);
}

#[test]
fn ops_15_storage_budget_silent_on_flat_shrinking_or_young() {
    let day = 20_700;
    let dedupe = 86_400_000_000_000;
    // Nothing being added: rate 0 ⇒ no projection toward the cap, never an
    // alert (a cleaned-up corpus is not heading for the cap).
    let idle: Vec<(i64, u64)> = (0..6).map(|i| (day + i, 0u64)).collect();
    assert!(
        mp_ops::storage_budget_alert(&idle, 20_000_000_000, 7, 14.0, dedupe, "data/raw").is_none()
    );
    // One old bulk load, nothing since: the mean rate overstates the run
    // rate, but the projection (54 days) stays beyond the horizon ⇒ silent.
    let burst: Vec<(i64, u64)> = (0..6)
        .map(|i| (day + i, if i == 0 { 10_000_000_000 } else { 0 }))
        .collect();
    assert!(
        mp_ops::storage_budget_alert(&burst, 100_000_000_000, 7, 14.0, dedupe, "data/raw")
            .is_none()
    );
    // Too young: one sample's rate (1 GB/day) still leaves 99 days to the
    // 100 GB cap ⇒ beyond the horizon, silent.
    assert!(mp_ops::storage_budget_alert(
        &[(day, 1_000_000_000)],
        100_000_000_000,
        7,
        14.0,
        dedupe,
        "data/raw"
    )
    .is_none());
    // Slow fill: 0.1 GB/day ⇒ 194 days > 14 ⇒ silent.
    let slow: Vec<(i64, u64)> = (0..6).map(|i| (day + i, 100_000_000u64)).collect();
    assert!(
        mp_ops::storage_budget_alert(&slow, 20_000_000_000, 7, 14.0, dedupe, "data/raw").is_none()
    );
    // No data at all ⇒ silent.
    assert!(
        mp_ops::storage_budget_alert(&[], 20_000_000_000, 7, 14.0, dedupe, "data/raw").is_none()
    );
    // Month rollover is linear on the day axis: 20260731 → 20260801 is one
    // day, so a trend crossing a month boundary keeps its slope.
    assert_eq!(
        mp_ops::days_from_yyyymmdd(20260801).unwrap()
            - mp_ops::days_from_yyyymmdd(20260731).unwrap(),
        1
    );
}

#[test]
fn ops_15_storage_budget_subcommand_reports_verdict() {
    // storage-budget: JSON verdict over real per-day files — non-conforming
    // names are skipped, the current day's partial file is excluded, a
    // growing corpus inside the horizon alerts (exit 0), a far cap stays
    // silent (alert:null), and an unconfigured budget fails closed (exit 2).
    let dir = std::env::temp_dir().join(format!("mpts15-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_mp-ops");

    // 6 completed days (2026-08-08..13) adding 100..600 B/day; a junk name,
    // a .lock file, and today's (2026-08-15) 1 GB file that MUST be excluded
    // as partial — the clock is pinned via --ts-ns.
    for (i, day) in [
        "20260808", "20260809", "20260810", "20260811", "20260812", "20260813",
    ]
    .iter()
    .enumerate()
    {
        std::fs::write(
            dir.join(format!("{day}_hyperliquid_BTC.log")),
            vec![0u8; 100 + i * 100],
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("trace_20260806_binance_BTCUSDT.log"),
        vec![0u8; 9_999],
    )
    .unwrap();
    std::fs::write(dir.join(".lock_hyperliquid_BTC"), b"held").unwrap();
    std::fs::write(
        dir.join("20260815_hyperliquid_BTC.log"),
        vec![0u8; 1_000_000_000],
    )
    .unwrap();
    let ts_ns = mp_ops::days_from_yyyymmdd(20260815).unwrap() * 86_400_000_000_000;

    // Fires: 2100 B cumulative (sum of 100..600 B/day), rate 350 B/day, cap
    // 1000 B ⇒ already at/over the cap, days_to_cap 0.
    let out = std::process::Command::new(bin)
        .args([
            "storage-budget",
            "--dir",
            dir.to_str().unwrap(),
            "--cap-bytes",
            "1000",
            "--ts-ns",
            &ts_ns.to_string(),
        ])
        .output()
        .expect("run mp-ops (storage-budget fires)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"current_bytes\":2100"), "{stdout}");
    assert!(
        stdout.contains("\"growth_bytes_per_day\":350.0"),
        "{stdout}"
    );
    assert!(stdout.contains("\"days_to_cap\":0.0"), "{stdout}");
    assert!(stdout.contains("\"id\":\"storage-budget\""), "{stdout}");
    assert!(stdout.contains("\"severity\":\"P2\""), "{stdout}");

    // Healthy: a far cap stays silent (alert:null) with the same corpus
    // numbers — current_bytes 2100 also proves today's 1 GB file was
    // excluded as a partial day.
    let out = std::process::Command::new(bin)
        .args([
            "storage-budget",
            "--dir",
            dir.to_str().unwrap(),
            "--cap-bytes",
            "1000000000",
            "--ts-ns",
            &ts_ns.to_string(),
        ])
        .output()
        .expect("run mp-ops (storage-budget healthy)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("\"alert\":null"), "{stdout}");
    assert!(stdout.contains("\"current_bytes\":2100"), "{stdout}");
    assert!(
        stdout.contains("\"growth_bytes_per_day\":350.0"),
        "{stdout}"
    );

    // Unconfigured budget ⇒ fail closed (exit 2), never a silent skip.
    let out = std::process::Command::new(bin)
        .args(["storage-budget", "--dir", dir.to_str().unwrap()])
        .env_remove("MP_STORAGE_BUDGET_BYTES")
        .output()
        .expect("run mp-ops (no budget)");
    assert_eq!(
        out.status.code(),
        Some(2),
        "an unconfigured budget must fail closed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("MP_STORAGE_BUDGET_BYTES"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Bad flag values are rejected, not silently absorbed.
    let out = std::process::Command::new(bin)
        .args([
            "storage-budget",
            "--dir",
            dir.to_str().unwrap(),
            "--cap-bytes",
            "1000",
            "--trend-days",
            "1",
        ])
        .output()
        .expect("run mp-ops (bad trend-days)");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("at least 2"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_15_held_drain_files_flags_landed_but_unreleased() {
    // The drain manifest's "VPS is still holding it" state: action=landed
    // with release NOT in {released, no_release} (ssh_failed / skipped).
    // `kept` (A-B collision — never released by design), `no_release`, and
    // entries predating the release field (release="") are never flagged.
    use mp_ops::{held_drain_files, parse_drain_manifest_line, DrainManifestEntry};
    let e = |ts: &str, file: &str, action: &str, release: &str| DrainManifestEntry {
        ts_utc: ts.to_string(),
        file: file.to_string(),
        action: action.to_string(),
        release: release.to_string(),
    };
    let entries = vec![
        e(
            "2026-08-16T08:21:01Z",
            "raw/20260815_bybit_BTCUSDT.log",
            "landed",
            "released",
        ),
        e(
            "2026-08-16T08:21:01Z",
            "raw/20260815_bybit_ETHUSDT.log",
            "landed",
            "no_release",
        ),
        e(
            "2026-08-16T08:21:01Z",
            "raw/20260815_bybit_SOLUSDT.log",
            "landed",
            "ssh_failed",
        ),
        e(
            "2026-08-16T08:21:01Z",
            "raw/20260815_hyperliquid_BTC.log",
            "collision",
            "kept",
        ),
        e(
            "2026-08-16T08:21:01Z",
            "raw/20260814_bybit_BTCUSDT.log",
            "landed",
            "skipped: hash changed: expected abc got def",
        ),
        e(
            "2026-08-14T16:20:14Z",
            "raw/20260812_hyperliquid_ETH.log",
            "landed",
            "",
        ),
    ];
    let held = held_drain_files(&entries);
    let files: Vec<&str> = held.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(
        files,
        vec![
            "raw/20260814_bybit_BTCUSDT.log",
            "raw/20260815_bybit_SOLUSDT.log"
        ]
    );
    // The reasons carry through (the release value itself).
    let sol = held
        .iter()
        .find(|(f, _)| f == "raw/20260815_bybit_SOLUSDT.log")
        .unwrap();
    assert_eq!(sol.1, "ssh_failed");

    // Latest-entry resolution: the manifest is append-only, so a later
    // successful release clears an earlier failure (and must not keep firing).
    let entries = vec![
        e(
            "2026-08-16T01:20:00Z",
            "raw/20260815_bybit_BTCUSDT.log",
            "landed",
            "ssh_failed",
        ),
        e(
            "2026-08-17T01:20:00Z",
            "raw/20260815_bybit_BTCUSDT.log",
            "landed",
            "released",
        ),
    ];
    assert!(held_drain_files(&entries).is_empty());
    // ...and the reverse order flags it.
    let entries = vec![
        e(
            "2026-08-16T01:20:00Z",
            "raw/20260815_bybit_BTCUSDT.log",
            "landed",
            "released",
        ),
        e(
            "2026-08-17T01:20:00Z",
            "raw/20260815_bybit_BTCUSDT.log",
            "landed",
            "ssh_failed",
        ),
    ];
    assert_eq!(held_drain_files(&entries).len(), 1);

    // Parser round-trip on the real manifest line shape; junk lines parse to
    // None (they never flag anything).
    let line = r#"{"ts_utc":"2026-08-16T08:21:01.2471763Z","vps_host":"34.135.127.147","vps_base":"/opt/money-printer/data","file":"raw/20260810_draintest_A.log","size":25,"sha256":"aabc","action":"landed","release":"released","no_release":false}"#;
    let parsed = parse_drain_manifest_line(line).expect("real line parses");
    assert_eq!(parsed.file, "raw/20260810_draintest_A.log");
    assert_eq!(parsed.release, "released");
    assert!(parse_drain_manifest_line("not json").is_none());
}

#[test]
fn ops_15_storage_budget_flags_held_drain_files() {
    // --manifest: a landed-but-ssh_failed entry makes the storage-budget P2
    // fire even with a far cap (the relay is holding a file — the "VPS never
    // accumulates" contract is broken regardless of growth), and the verdict
    // carries the held list. A clean manifest (released) with a far cap stays
    // silent. An unreadable manifest fails closed (exit 2).
    let dir = std::env::temp_dir().join(format!("mpts15h-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_mp-ops");
    std::fs::write(dir.join("20260814_hyperliquid_BTC.log"), vec![0u8; 100]).unwrap();
    let ts_ns = mp_ops::days_from_yyyymmdd(20260815).unwrap() * 86_400_000_000_000;
    let manifest = dir.join("vps_drain_manifest.jsonl");
    let far_cap = "99999999999";

    // Held: fires the P2 with the held file named.
    std::fs::write(
        &manifest,
        r#"{"ts_utc":"2026-08-16T08:21:01Z","file":"raw/20260815_bybit_BTCUSDT.log","action":"landed","release":"ssh_failed","no_release":false}"#,
    )
    .unwrap();
    let out = std::process::Command::new(bin)
        .args([
            "storage-budget",
            "--dir",
            dir.to_str().unwrap(),
            "--cap-bytes",
            far_cap,
            "--manifest",
            manifest.to_str().unwrap(),
            "--ts-ns",
            &ts_ns.to_string(),
        ])
        .output()
        .expect("run mp-ops (held fires)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"id\":\"storage-budget\""), "{stdout}");
    assert!(stdout.contains("\"held_vps_count\":1"), "{stdout}");
    assert!(stdout.contains("20260815_bybit_BTCUSDT"), "{stdout}");
    assert!(stdout.contains("held VPS file(s)"), "{stdout}");

    // Clean manifest (released) + far cap: silent.
    std::fs::write(
        &manifest,
        r#"{"ts_utc":"2026-08-16T08:21:01Z","file":"raw/20260815_bybit_BTCUSDT.log","action":"landed","release":"released","no_release":false}"#,
    )
    .unwrap();
    let out = std::process::Command::new(bin)
        .args([
            "storage-budget",
            "--dir",
            dir.to_str().unwrap(),
            "--cap-bytes",
            far_cap,
            "--manifest",
            manifest.to_str().unwrap(),
            "--ts-ns",
            &ts_ns.to_string(),
        ])
        .output()
        .expect("run mp-ops (clean silent)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"alert\":null"), "{stdout}");
    assert!(stdout.contains("\"held_vps_count\":0"), "{stdout}");

    // Unreadable manifest: fail closed (exit 2), never a silent skip.
    let out = std::process::Command::new(bin)
        .args([
            "storage-budget",
            "--dir",
            dir.to_str().unwrap(),
            "--cap-bytes",
            far_cap,
            "--manifest",
            dir.join("missing_manifest.jsonl").to_str().unwrap(),
            "--ts-ns",
            &ts_ns.to_string(),
        ])
        .output()
        .expect("run mp-ops (unreadable manifest)");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("read manifest"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_15_storage_budget_timer_runs_daily_and_reads_only_the_corpus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let service = std::fs::read_to_string(root.join("systemd/storage-budget.service"))
        .expect("storage-budget.service");
    assert!(
        service.contains("mp-ops storage-budget --dir /opt/money-printer/data/raw --telegram"),
        "OPS-15: the service runs the daily storage-budget check with delivery"
    );
    assert!(
        service.contains("EnvironmentFile=/etc/money-printer/ops.env"),
        "OPS-15: the budget and Bot API credentials come from ops.env"
    );
    assert!(service.contains("WorkingDirectory=/opt/money-printer"));
    assert!(service.contains("ProtectSystem=strict"));
    assert!(
        service.contains("ReadOnlyPaths=/opt/money-printer/data/raw"),
        "OPS-15: the check is read-only on the corpus (W-6)"
    );
    assert!(service.contains("User=printer"));
    assert!(service.contains("NoNewPrivileges=true"));

    let timer = std::fs::read_to_string(root.join("systemd/storage-budget.timer"))
        .expect("storage-budget.timer");
    assert!(
        timer.contains("OnCalendar=*-*-* 01:30:00"),
        "OPS-15: daily at 01:30 UTC — after the 00:05 pipeline, clear of grading"
    );
    assert!(
        timer.contains("Persistent=true"),
        "OPS-15: missed fires are caught up"
    );
}

// ---- OPS-14: near-real-time stale Telegram batch watch ---------------------

#[test]
fn ops_14_telegram_stale_alert_fires_on_missed_flush() {
    // A dispatch queued ≥ one quiet window (24h) is a missed flush: P2, the
    // OLDEST stale dispatch wins, dedupe is per dispatch id (one stuck alert
    // never suppresses another, regression_audit3 pattern), and the detail
    // names the queued hours + the remediation.
    let h = 3600 * S;
    let rows = vec![
        TelegramPendingRow {
            id: "stream-gap".to_string(),
            severity: Severity::P2,
            detail: "BTCUSDT gap 6m".to_string(),
            runbook: "ops/runbooks/stream-gap.md".to_string(),
            ts_ns: 26 * h,
            queued_days: 1,
        },
        TelegramPendingRow {
            id: "band-accuracy-decay".to_string(),
            severity: Severity::P3,
            detail: "RES-4 decay".to_string(),
            runbook: "ops/runbooks/band-accuracy-decay.md".to_string(),
            ts_ns: 20 * h,
            queued_days: 0,
        },
    ];
    let alert =
        mp_ops::stale_batch_alert(&rows, 50 * h, 24 * h, 24 * h).expect("missed flush fires");
    assert_eq!(alert.id, "telegram-stale");
    assert_eq!(alert.severity, Severity::P2);
    assert_eq!(alert.runbook, "ops/runbooks/telegram-stale.md");
    // 20h-queued is OLDER than 26h-queued ⇒ it is the worst offender.
    assert_eq!(alert.dedupe_key, "band-accuracy-decay");
    assert!(
        alert.detail.contains("2 dispatch"),
        "count: {}",
        alert.detail
    );
    assert!(
        alert.detail.contains("30h"),
        "queued hours: {}",
        alert.detail
    );
    assert!(
        alert.detail.contains("telegram-flush --wait"),
        "remediation: {}",
        alert.detail
    );

    // Below the threshold (freshly batched) ⇒ nothing to alert.
    let fresh = vec![TelegramPendingRow {
        id: "stream-gap".to_string(),
        severity: Severity::P2,
        detail: "x".to_string(),
        runbook: "ops/runbooks/stream-gap.md".to_string(),
        ts_ns: 40 * h,
        queued_days: 0,
    }];
    assert!(mp_ops::stale_batch_alert(&fresh, 50 * h, 24 * h, 24 * h).is_none());

    // Exactly at the threshold (24h queued) ⇒ stale.
    let boundary = vec![TelegramPendingRow {
        id: "stream-gap".to_string(),
        severity: Severity::P2,
        detail: "x".to_string(),
        runbook: "ops/runbooks/stream-gap.md".to_string(),
        ts_ns: 26 * h,
        queued_days: 1,
    }];
    assert!(mp_ops::stale_batch_alert(&boundary, 50 * h, 24 * h, 24 * h).is_some());

    // Empty ledger ⇒ healthy (nothing queued is not a missed flush).
    assert!(mp_ops::stale_batch_alert(&[], 50 * h, 24 * h, 24 * h).is_none());
}

#[test]
fn ops_14_telegram_stale_subcommand_reports_verdict() {
    // telegram-stale: JSON verdict over the batch ledger — a stuck dispatch
    // is flagged (exit 0, stale:true), a fresh one and a missing batch are
    // healthy (stale:false), and a corrupt batch fails closed (exit 2).
    let dir = std::env::temp_dir().join(format!("mpts14-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bin = env!("CARGO_BIN_EXE_mp-ops");
    let h = 3600 * S;

    // Stale: a dispatch whose ts_ns is 30h after the epoch (decades before
    // the real clock) ⇒ queued far beyond any threshold.
    let stale_dir = dir.join("stale");
    std::fs::create_dir_all(&stale_dir).unwrap();
    let a = Alert::new("band-accuracy-decay", Severity::P3, 0, "RES-4 decay");
    mp_ops::append_batch(&stale_dir, &mp_ops::Dispatch::from_alert(&a, 30 * h)).unwrap();
    let out = std::process::Command::new(bin)
        .args(["telegram-stale"])
        .env("MP_OPS_TELEGRAM_DIR", stale_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (stale)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"stale\":true"), "{stdout}");
    assert!(stdout.contains("\"pending\":1"), "{stdout}");
    assert!(stdout.contains("\"id\":\"telegram-stale\""), "{stdout}");
    assert!(stdout.contains("\"severity\":\"P2\""), "{stdout}");

    // Fresh: a dispatch queued 1h ago (ts = real now − 1h) with the default
    // 24h threshold ⇒ healthy; --threshold-hours 1 makes it stale.
    let fresh_dir = dir.join("fresh");
    std::fs::create_dir_all(&fresh_dir).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    let b = Alert::new("stream-gap", Severity::P2, 0, "gap");
    mp_ops::append_batch(&fresh_dir, &mp_ops::Dispatch::from_alert(&b, now - h)).unwrap();
    let out = std::process::Command::new(bin)
        .args(["telegram-stale"])
        .env("MP_OPS_TELEGRAM_DIR", fresh_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (fresh)");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"stale\":false"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let out = std::process::Command::new(bin)
        .args(["telegram-stale", "--threshold-hours", "1"])
        .env("MP_OPS_TELEGRAM_DIR", fresh_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (1h threshold)");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"stale\":true"),
        "a 1h-old dispatch exceeds a 1h threshold: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Missing batch ⇒ healthy "nothing queued", not an error.
    let empty_dir = dir.join("empty");
    let out = std::process::Command::new(bin)
        .args(["telegram-stale"])
        .env("MP_OPS_TELEGRAM_DIR", empty_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (empty)");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"stale\":false"));

    // Corrupt batch ⇒ fail closed (exit 2), never a fabricated verdict.
    let corrupt_dir = dir.join("corrupt");
    std::fs::create_dir_all(&corrupt_dir).unwrap();
    std::fs::write(corrupt_dir.join("batch.jsonl"), "not a dispatch\n").unwrap();
    let out = std::process::Command::new(bin)
        .args(["telegram-stale"])
        .env("MP_OPS_TELEGRAM_DIR", corrupt_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (corrupt)");
    assert_eq!(
        out.status.code(),
        Some(2),
        "corrupt batch must fail closed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Bad --threshold-hours / --dedupe-ns values are rejected, not silently
    // absorbed.
    let bad = std::process::Command::new(bin)
        .args(["telegram-stale", "--threshold-hours", "abc"])
        .env("MP_OPS_TELEGRAM_DIR", stale_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (bad threshold)");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("must be an integer"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    let bad_dedupe = std::process::Command::new(bin)
        .args(["telegram-stale", "--dedupe-ns", "0"])
        .env("MP_OPS_TELEGRAM_DIR", stale_dir.to_str().unwrap())
        .output()
        .expect("run mp-ops (bad dedupe)");
    assert!(
        String::from_utf8_lossy(&bad_dedupe.stderr).contains("must be positive"),
        "{}",
        String::from_utf8_lossy(&bad_dedupe.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_14_telegram_stale_p2_breaks_through_quiet_hours() {
    // The stale alert is P2: even inside quiet hours it is sent immediately
    // (OPS-9) — it escapes the very batch that is stuck instead of being
    // re-queued into it. All-day quiet (0..1440) would batch a P3; the P2
    // still POSTs to the Bot API right away.
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let dir = std::env::temp_dir().join(format!("mpts14t-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let h = 3600 * S;
    let a = Alert::new("band-accuracy-decay", Severity::P3, 0, "RES-4 decay");
    mp_ops::append_batch(&dir, &mp_ops::Dispatch::from_alert(&a, 30 * h)).unwrap();
    let (url, handle) = stub_telegram_server();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args(["telegram-stale", "--telegram"])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("MP_OPS_TELEGRAM_URL", &url)
        .env("MP_OPS_TELEGRAM_DIR", dir.to_str().unwrap())
        .env("MP_OPS_QUIET_START_MIN", "0")
        .env("MP_OPS_QUIET_END_MIN", "1440")
        .output()
        .expect("run mp-ops (stale --telegram)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"stale\":true"), "{stdout}");
    assert!(stdout.contains("\"telegram\":\"sent\""), "{stdout}");
    let req = handle.join().unwrap();
    assert!(req.contains("/botTESTTOKEN/sendMessage"), "{req}");
    assert!(req.contains("telegram-stale"), "{req}");
    assert!(req.contains("chat_id=12345"), "{req}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_14_telegram_stale_timer_runs_hourly_and_reads_only_the_ledger() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let service = std::fs::read_to_string(root.join("systemd/telegram-stale.service"))
        .expect("telegram-stale.service");
    assert!(
        service.contains("mp-ops telegram-stale --telegram"),
        "OPS-14: the service runs the near-real-time check"
    );
    assert!(
        service.contains("EnvironmentFile=/etc/money-printer/ops.env"),
        "OPS-14: Bot API credentials come from ops.env"
    );
    assert!(
        service.contains("WorkingDirectory=/opt/money-printer"),
        "OPS-14: the default journal/telegram resolves under the money-printer tree, not systemd's /"
    );
    assert!(service.contains("ProtectSystem=strict"));
    assert!(
        service.contains("ReadOnlyPaths=/opt/money-printer/journal/telegram"),
        "OPS-14: the check is read-only on the ledger (W-6)"
    );
    assert!(service.contains("User=printer"));
    assert!(service.contains("NoNewPrivileges=true"));

    let timer = std::fs::read_to_string(root.join("systemd/telegram-stale.timer"))
        .expect("telegram-stale.timer");
    assert!(
        timer.contains("OnCalendar=*-*-* *:15:00"),
        "OPS-14: hourly — a missed flush alerts within an hour of the 24h mark"
    );
    assert!(
        timer.contains("Persistent=true"),
        "OPS-14: missed fires are caught up"
    );
}

#[test]
fn ops_13_mp_ops_decay_send_failure_still_journals_verdict() {
    // A failed Telegram delivery must NOT blank the week's verdict in
    // runs/index.jsonl: the record is journaled with "send_failed" and the
    // command still exits 2 (the wrapper warns, never fails the study).
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let dir = std::env::temp_dir().join(format!("mptg9d-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let trend = decayed_trend(&dir);
    let runs = dir.join("runs");
    // `{"ok":false}` is 12 bytes — a correct Content-Length means the
    // failure is the API's non-ok verdict, not a curl transfer truncation.
    let (url, handle) = stub_telegram_server_with_body(
        b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n{\"ok\":false}",
    );

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args([
            "band-accuracy-decay",
            "--trend",
            trend.to_str().unwrap(),
            "--runs-dir",
            runs.to_str().unwrap(),
            "--telegram",
        ])
        .env("TELEGRAM_BOT_TOKEN", "TESTTOKEN")
        .env("TELEGRAM_CHAT_ID", "12345")
        .env("MP_OPS_TELEGRAM_URL", &url)
        .env("MP_OPS_QUIET_START_MIN", "0")
        .env("MP_OPS_QUIET_END_MIN", "0")
        .output()
        .expect("run mp-ops (send failure)");
    assert_eq!(
        out.status.code(),
        Some(2),
        "a failed send is a failed job: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("telegram send failed"),
        "failure surfaces in stderr"
    );
    let _ = handle.join().unwrap();

    // The verdict line exists despite the delivery failure — honest
    // "send_failed", not a blanked week.
    let idx = std::fs::read_to_string(runs.join("index.jsonl")).expect("verdict journaled");
    let rec: serde_json::Value =
        serde_json::from_str(idx.lines().next().expect("one verdict line")).unwrap();
    assert_eq!(rec["study"], "band_accuracy_decay");
    assert_eq!(rec["decayed"], true);
    assert_eq!(rec["telegram"], "send_failed");
    assert_eq!(rec["week"], "2026-W12");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_9_mp_ops_decay_telegram_unconfigured_is_gated() {
    let dir = std::env::temp_dir().join(format!("mptg9c-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let trend = decayed_trend(&dir);
    // No credentials ⇒ exit 0 with "unconfigured": functionality-gated, never
    // a silent failure and never a fake send.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args([
            "band-accuracy-decay",
            "--trend",
            trend.to_str().unwrap(),
            "--telegram",
        ])
        .env_remove("TELEGRAM_BOT_TOKEN")
        .env_remove("TELEGRAM_CHAT_ID")
        .output()
        .expect("run mp-ops (unconfigured)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"telegram\":\"unconfigured\""), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_9_weekly_wrapper_batches_and_flushes_decay_telegram() {
    let dir = std::env::temp_dir().join(format!("mptg9w-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A stub mp-ops: reports a batched decay verdict; for the flush it echoes
    // the args it received so the test can see `--wait` reached it.
    let stub = dir.join("stub-mp-ops.sh");
    std::fs::write(
        &stub,
        "#!/usr/bin/env bash\nif [ \"${1:-}\" = \"telegram-flush\" ]; then echo '{\"flushed\":1} args:' \"$*\"; else echo '{\"decayed\":true,\"telegram\":\"batched\"}'; fi\n",
    )
    .unwrap();
    let chmod = std::process::Command::new("bash")
        .args(["-c", &format!("chmod +x '{}'", wsl(&stub))])
        .status()
        .expect("chmod stub");
    assert!(chmod.success());

    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("20260803_hyperliquid_positions.log"), "").unwrap();
    std::fs::write(data.join("20260803_hyperliquid_BTC.log"), "").unwrap();
    let out = dir.join("out");
    let runs = dir.join("runs");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/run_whale_study_weekly.sh");

    // The wait lives inside the subcommand now — the wrapper just invokes
    // `telegram-flush --wait` (the stub flush ignores it and reports success).
    let res = std::process::Command::new("bash")
        .args(["-c", &format!(
            "MP_DATA_DIR='{}' MP_OUT_DIR='{}' MP_RUNS_DIR='{}' MP_PYTHON=/bin/echo MP_OPS_CMD='{}' MP_REPO_DIR='{}' '{}'",
            wsl(&data), wsl(&out), wsl(&runs), wsl(&stub), wsl(&data), wsl(&script)
        )])
        .output()
        .expect("run wrapper (batched decay)");
    assert_eq!(
        res.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&res.stderr)
    );
    let stdout = String::from_utf8_lossy(&res.stdout);
    assert!(
        stdout.contains("\"batched\""),
        "batched verdict surfaced: {stdout}"
    );
    assert!(
        stdout.contains("telegram-flush --wait"),
        "the wait is the subcommand's, not the wrapper's: {stdout}"
    );
    assert!(
        stdout.contains("{\"flushed\":1}"),
        "flush invoked: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ops_9_weekly_wrapper_warns_on_failed_check_and_never_flushes() {
    // A FAILED decay check (corrupt journal, exit 2) must surface loudly in
    // stderr and must NEVER gate a batch flush — only a clean verdict does.
    let dir = std::env::temp_dir().join(format!("mptg9f-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("stub-fail.sh");
    std::fs::write(
        &stub,
        "#!/usr/bin/env bash\necho 'corrupt trend line 3' >&2; exit 2\n",
    )
    .unwrap();
    let chmod = std::process::Command::new("bash")
        .args(["-c", &format!("chmod +x '{}'", wsl(&stub))])
        .status()
        .expect("chmod stub");
    assert!(chmod.success());
    let flush = dir.join("flush-marker");
    std::fs::write(&flush, "").unwrap();
    let flush_stub = dir.join("stub-flush.sh");
    std::fs::write(
        &flush_stub,
        format!("#!/usr/bin/env bash\nrm '{}'\n", wsl(&flush)),
    )
    .unwrap();
    let chmod = std::process::Command::new("bash")
        .args(["-c", &format!("chmod +x '{}'", wsl(&flush_stub))])
        .status()
        .expect("chmod flush stub");
    assert!(chmod.success());

    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("20260803_hyperliquid_positions.log"), "").unwrap();
    std::fs::write(data.join("20260803_hyperliquid_BTC.log"), "").unwrap();
    let out = dir.join("out");
    let runs = dir.join("runs");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/run_whale_study_weekly.sh");

    // A stub MP_OPS_CMD that fails (exit 2) AND a stub telegram-flush that
    // would delete the marker if invoked — the flush stub must never run.
    let script_path = wsl(&script);
    let stub_path = wsl(&stub);
    let flush_stub_path = wsl(&flush_stub);
    let fail = std::process::Command::new("bash")
        .args(["-c", &format!(
            "MP_DATA_DIR='{}' MP_OUT_DIR='{}' MP_RUNS_DIR='{}' MP_PYTHON=/bin/echo MP_OPS_CMD='{stub_path}' MP_OPS_TELEGRAM_FLUSH='{flush_stub_path}' MP_REPO_DIR='{}' '{}'",
            wsl(&data), wsl(&out), wsl(&runs), wsl(&data), script_path
        )])
        .output()
        .expect("run wrapper (failed check)");
    assert_eq!(
        fail.status.code(),
        Some(0),
        "a failed advisory check never fails the study: {}",
        String::from_utf8_lossy(&fail.stderr)
    );
    let stderr = String::from_utf8_lossy(&fail.stderr);
    assert!(
        stderr.contains("decay check failed (exit 2)"),
        "failure surfaces in journald: {stderr}"
    );
    assert!(
        stderr.contains("corrupt trend line 3"),
        "the check's own stderr is echoed: {stderr}"
    );
    assert!(
        flush.exists(),
        "a failed check must NEVER trigger telegram-flush"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- LIQ-10 (spec 029): weekly RES-4 whale-study timer ---------------------

#[test]
fn liq_10_whale_study_timer_skips_without_logs_and_journals_runs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    // The weekly schedule + skip gate exist and are shaped right (LIQ-10).
    let service = std::fs::read_to_string(root.join("systemd/whale-study.service"))
        .expect("whale-study.service");
    assert!(
        service.contains(
            "ConditionPathExists=/opt/money-printer/data/raw/*_hyperliquid_positions.log"
        ),
        "LIQ-10: skip gate on the spec 028 census log (WHL-6 pattern)"
    );
    assert!(
        service.contains("run_whale_study_weekly.sh"),
        "LIQ-10: the service runs the weekly wrapper"
    );
    assert!(service.contains("ProtectSystem=strict"));
    assert!(
        service.contains(
            "ReadWritePaths=/opt/money-printer/runs /opt/money-printer/research/band_accuracy"
        ),
        "LIQ-10: the study writes only its journals (RES-7/W-6)"
    );
    assert!(
        service.contains("ReadOnlyPaths=/opt/money-printer/data/raw"),
        "LIQ-10: recorded data is read-only to the study"
    );
    assert!(service.contains("User=printer"));

    let timer =
        std::fs::read_to_string(root.join("systemd/whale-study.timer")).expect("whale-study.timer");
    assert!(timer.contains("OnCalendar=Tue *-*-* 06:30:00 UTC"));
    assert!(
        timer.contains("Persistent=true"),
        "LIQ-10: missed fires are caught up"
    );

    let script = root.join("scripts/run_whale_study_weekly.sh");
    assert!(
        script.exists(),
        "LIQ-10: wrapper script is checked in (OPS-8)"
    );
    let text = std::fs::read_to_string(&script).unwrap();
    assert!(
        text.contains("--runs-dir"),
        "LIQ-10: journals to runs/index.jsonl"
    );
    assert!(
        text.contains("_hyperliquid_positions.log"),
        "LIQ-10: gated on the mp-whale census log"
    );

    // Skip-not-fail: no positions log ⇒ exit 0, nothing journaled.
    let empty = std::env::temp_dir().join(format!("mpli10-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    // `Command::new("bash")` resolves to WSL bash on this box, so paths must
    // be converted via `wsl()` (same as the restore-drill tests above).
    let skip = std::process::Command::new("bash")
        .args([
            "-c",
            &format!(
                "MP_DATA_DIR='{}' MP_REPO_DIR='{}' '{}'",
                wsl(&empty),
                wsl(&empty),
                wsl(&script)
            ),
        ])
        .output()
        .expect("run wrapper (skip)");
    assert_eq!(
        skip.status.code(),
        Some(0),
        "no positions log ⇒ clean skip; stderr: {}",
        String::from_utf8_lossy(&skip.stderr)
    );
    assert!(String::from_utf8_lossy(&skip.stdout).contains("skipping"));
    assert!(!empty.join("index.jsonl").exists());

    // Wiring: fresh logs in the window reach the job as --log args, with
    // --runs-dir + a stamped git sha (echo stands in for the python job).
    let data = std::env::temp_dir().join(format!("mpli10d-{}", std::process::id()));
    let out = std::env::temp_dir().join(format!("mpli10o-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("20260803_hyperliquid_positions.log"), "").unwrap();
    std::fs::write(data.join("20260803_hyperliquid_BTC.log"), "").unwrap();
    let runs = data.join("runs");
    let wire = std::process::Command::new("bash")
        .args(["-c", &format!(
            "MP_DATA_DIR='{}' MP_OUT_DIR='{}' MP_RUNS_DIR='{}' MP_PYTHON=/bin/echo MP_REPO_DIR='{}' '{}'",
            wsl(&data), wsl(&out), wsl(&runs), wsl(&data), wsl(&script)
        )])
        .output()
        .expect("run wrapper (wired)");
    assert_eq!(
        wire.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&wire.stderr)
    );
    let stdout = String::from_utf8_lossy(&wire.stdout);
    assert!(
        stdout.contains("--log"),
        "wrapper passes discovered logs: {stdout}"
    );
    assert!(
        stdout.contains("hyperliquid_positions.log"),
        "positions log wired: {stdout}"
    );
    assert!(
        stdout.contains("hyperliquid_BTC.log"),
        "market log wired: {stdout}"
    );
    assert!(
        stdout.contains("--runs-dir") && stdout.contains("runs"),
        "runs dir wired: {stdout}"
    );
    assert!(
        stdout.contains("--git-sha unknown"),
        "git sha stamped (unknown without a repo): {stdout}"
    );
    let _ = std::fs::remove_dir_all(&empty);
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn sto_7_disk_watchdog_alerts_and_never_deletes() {
    use mp_ops::disk_alert;
    // The watchdog is ALERT-ONLY (STO-7/W-6): it has no deletion capability —
    // its whole surface is (readings) -> Option<Alert>. Prove the alert fires
    // past the budget and that data on disk is untouched by the check.
    let dir = std::env::temp_dir().join(format!("mpsto7-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let data = dir.join("recorded.parquet");
    std::fs::write(&data, b"recorded market data").unwrap();
    let a = disk_alert(0.90, 0.85, MIN).expect("over budget must alert");
    assert_eq!(a.id, "disk-high");
    assert!(data.exists(), "the watchdog never deletes recorded data");
    assert_eq!(std::fs::read(&data).unwrap(), b"recorded market data");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- P1 webhook egress (owner decision 2026-08-06, audit 08-04 #3/#9) ----

/// The P1 channel is WIRED: `mp-ops p1-webhook` posts the dispatch JSON to
/// the owner-configured sink via curl (the host's TLS stack — https is
/// accepted, never forced cleartext) and reports `egress: sent`. The stub
/// captures the raw HTTP request; the payload must carry the exact
/// `Dispatch::from_alert` shape (id/severity/detail/runbook/ts_ns).
#[test]
fn ops_9_p1_webhook_posts_dispatch_via_curl_to_stub() {
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let (url, handle) = stub_telegram_server();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args([
            "p1-webhook",
            "--id",
            "recon-diverged",
            "--detail",
            "BTCUSDT position mismatch",
            "--ts-ns",
            "1234",
        ])
        .env("MP_OPS_P1_WEBHOOK", &url)
        .output()
        .expect("run mp-ops p1-webhook");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("\"egress\":\"sent\""), "{stdout}");
    assert!(stdout.contains("\"severity\":\"P1\""), "{stdout}");
    assert!(stdout.contains("\"ts_ns\":1234"), "{stdout}");

    let req = handle.join().unwrap();
    assert!(req.starts_with("POST /"), "{req}");
    assert!(req.contains("Content-Type: application/json"), "{req}");
    assert!(req.contains("\"id\":\"recon-diverged\""), "{req}");
    assert!(req.contains("\"severity\":\"P1\""), "{req}");
    assert!(
        req.contains("\"detail\":\"BTCUSDT position mismatch\""),
        "{req}"
    );
    assert!(
        req.contains("\"runbook\":\"ops/runbooks/recon-diverged.md\""),
        "{req}"
    );
    assert!(req.contains("\"ts_ns\":1234"), "{req}");
}

/// Dead until creds, loudly: `MP_OPS_P1_WEBHOOK` unset ⇒ the command exits 2
/// with the "dead until credentials" reason — never a silent drop, never a
/// fake send (the audit's #3 complaint was exactly the silent dead channel).
#[test]
fn ops_9_p1_webhook_fails_closed_when_unconfigured() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args(["p1-webhook", "--id", "recon-diverged", "--detail", "x"])
        .env_remove("MP_OPS_P1_WEBHOOK")
        .output()
        .expect("run mp-ops p1-webhook (unconfigured)");
    assert_eq!(out.status.code(), Some(2), "fail-closed exit 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MP_OPS_P1_WEBHOOK unset") && stderr.contains("dead until credentials"),
        "loud reason, not a silent drop: {stderr}"
    );
}

/// Fail-closed on a failed delivery: a non-2xx sink response is an error
/// (exit 2), never a "sent" lie — the same posture as the Telegram edge.
#[test]
fn ops_9_p1_webhook_rejects_non_2xx_sink_response() {
    if !curl_available() {
        eprintln!("SKIPPED: curl not available on this host");
        return;
    }
    let (url, _handle) = stub_telegram_server_with_body(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mp-ops"))
        .args(["p1-webhook", "--id", "recon-diverged", "--detail", "x"])
        .env("MP_OPS_P1_WEBHOOK", &url)
        .output()
        .expect("run mp-ops p1-webhook (500)");
    assert_eq!(out.status.code(), Some(2), "non-2xx must fail closed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("non-2xx"), "{stderr}");
}
