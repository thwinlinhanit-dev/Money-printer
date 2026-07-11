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
                                    // Empty inputs render explicit "no data", never a blank cell.
    let mut empty = fixture_report();
    empty.strategies.clear();
    empty.tracking.clear();
    let md2 = empty.render_markdown();
    assert!(md2.contains("_no data_"));
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
        allowed: &allowed,
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
    let script = root.join("restore-drill.sh");
    assert!(script.exists(), "ops/restore-drill.sh must exist (OPS-5)");
    // No backup argument ⇒ usage error (exit 2), never a fake PASS.
    let out = std::process::Command::new("bash")
        .arg(&script)
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
            allowed: &allowed,
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
    let script = root.join("restore-drill.sh");
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
        .arg(&script)
        .arg(&tarball)
        .env("MP_DRILL_VERIFY_CMD", "true")
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
        .arg(&script)
        .arg(&bad)
        .env("MP_DRILL_VERIFY_CMD", "true")
        .output()
        .expect("run drill bad");
    assert!(
        !fail.status.success(),
        "missing journal/ must fail the drill"
    );
    let _ = std::fs::remove_dir_all(&dir);
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
