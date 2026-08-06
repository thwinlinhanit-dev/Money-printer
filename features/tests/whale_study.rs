//! End-to-end test for the `whale_study` binary (RES-4, spec 029 LIQ-6 /
//! spec 028 cross-link): writes two temp mp event logs with DELIBERATELY
//! DIFFERENT symbol-id spaces (each collector run interns its own ids, EVT-8)
//! and verifies the binary remaps them onto one canonical space, merges in
//! recv order, replays through WhaleBandStudy, and reports the band accuracy.
//! Local temp files, no network (CONV-23); test name embeds the requirement ID
//! (CONV-21).

use mp_core::log::EventLogWriter;
use mp_core::{EventEnvelope, MarketEvent, SymbolId, SymbolMeta, SymbolTable, Venue};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Unique, pre-cleaned per-test scratch dir (tests run in parallel threads in
/// one process — a shared dir would race on the same log filenames).
fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("whale-study-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_log(path: &Path, metas: Vec<SymbolMeta>, events: Vec<EventEnvelope>) {
    let (mut w, _) = EventLogWriter::open(path).unwrap();
    w.write_symbols(&metas).unwrap();
    for ev in events {
        w.append(&ev).unwrap();
    }
    w.sync().unwrap();
}

fn mark(sym: SymbolId, recv: i64, m: f64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        sym,
        recv,
        recv,
        0,
        MarketEvent::MarkPrice { mark: m, index: m },
    )
}

fn oi(sym: SymbolId, recv: i64, contracts: f64, notional: f64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        sym,
        recv,
        recv,
        0,
        MarketEvent::OpenInterest {
            oi_contracts: contracts,
            oi_notional: notional,
        },
    )
}

fn whale(sym: SymbolId, recv: i64, size: f64, liq_price: f64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        sym,
        recv,
        recv,
        0,
        MarketEvent::WhalePosition {
            address: "0xwhale".into(),
            size,
            entry: 100.0,
            leverage: 50.0,
            liq_price,
        },
    )
}

fn whale_full(
    sym: SymbolId,
    recv: i64,
    size: f64,
    leverage: f64,
    entry: f64,
    liq_price: f64,
) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        sym,
        recv,
        recv,
        0,
        MarketEvent::WhalePosition {
            address: "0xwhale".into(),
            size,
            entry,
            leverage,
            liq_price,
        },
    )
}

#[test]
fn liq_6b_whale_study_binary_replays_recorded_logs() {
    let dir = temp_dir("6b");
    let market_path = dir.join("market.log");
    let positions_path = dir.join("positions.log");

    // Market log: "ETH" interned FIRST so BTC gets local id 1 (the canonical
    // remap must still pair it with the positions log's BTC = local id 0).
    let mut market_tab = SymbolTable::new();
    let eth = market_tab.intern_default(Venue::Hyperliquid, "ETH");
    let btc = market_tab.intern_default(Venue::Hyperliquid, "BTC");
    assert_eq!(eth, SymbolId(0));
    assert_eq!(btc, SymbolId(1));
    write_log(
        &market_path,
        market_tab.metas().to_vec(),
        vec![mark(btc, 1, 100.0), oi(btc, 2, 1000.0, 100_000.0)],
    );

    // Positions log: fresh table, so BTC is local id 0 — DIFFERENT space.
    let mut pos_tab = SymbolTable::new();
    let btc_pos = pos_tab.intern_default(Venue::Hyperliquid, "BTC");
    assert_eq!(btc_pos, SymbolId(0));
    // recv=0 arrives BEFORE any mark/OI (no estimate yet ⇒ skipped); recv=3
    // lands after the band state is seeded ⇒ recorded. Proves both the merge
    // order and the cross-log symbol remap in one run.
    write_log(
        &positions_path,
        pos_tab.metas().to_vec(),
        vec![whale(btc_pos, 0, 2.0, 95.0), whale(btc_pos, 3, 2.0, 99.0)],
    );

    let out = Command::new(env!("CARGO_BIN_EXE_whale_study"))
        .arg("--log")
        .arg(&market_path)
        .arg("--log")
        .arg(&positions_path)
        .arg("--json")
        .output()
        .expect("whale_study binary runs");
    assert!(
        out.status.success(),
        "binary exited {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Only the recv=3 position pairs with a seeded band ⇒ 1 observation, and
    // its estimate (≈98.03, a 50x/1.5-buffer model on mark=100) is at-or-below
    // the realized 99.0 ⇒ full coverage on the long side.
    assert!(
        stdout.contains("\"observations\": 1"),
        "expected 1 observation, got:\n{stdout}"
    );
    assert!(
        stdout.contains("\"coverage\": 1.0"),
        "expected full coverage, got:\n{stdout}"
    );
    assert!(stdout.contains("\"whale_positions\": 2"));

    let _ = std::fs::remove_file(&market_path);
    let _ = std::fs::remove_file(&positions_path);
}

#[test]
fn liq_6c_whale_study_journals_sim10_run_record() {
    let dir = temp_dir("6c");
    let market_path = dir.join("market.log");
    let positions_path = dir.join("positions.log");
    let runs_dir = dir.join("runs");

    let mut tab = SymbolTable::new();
    let btc = tab.intern_default(Venue::Hyperliquid, "BTC");
    write_log(
        &market_path,
        tab.metas().to_vec(),
        vec![mark(btc, 1, 100.0), oi(btc, 2, 1000.0, 100_000.0)],
    );
    let mut ptab = SymbolTable::new();
    let btc_pos = ptab.intern_default(Venue::Hyperliquid, "BTC");
    write_log(
        &positions_path,
        ptab.metas().to_vec(),
        vec![whale(btc_pos, 3, 2.0, 99.0)],
    );

    let run = |id: &str| {
        Command::new(env!("CARGO_BIN_EXE_whale_study"))
            .arg("--log")
            .arg(&market_path)
            .arg("--log")
            .arg(&positions_path)
            .arg("--run-id")
            .arg(id)
            .arg("--runs-dir")
            .arg(&runs_dir)
            .arg("--git-sha")
            .arg("abc123")
            .output()
            .expect("whale_study binary runs")
    };

    let a = run("ulid-1");
    assert!(
        a.status.success(),
        "{} ",
        String::from_utf8_lossy(&a.stderr)
    );
    let b = run("ulid-2");
    assert!(
        b.status.success(),
        "{} ",
        String::from_utf8_lossy(&b.stderr)
    );

    // Append-only (W-6): two runs ⇒ two lines, both valid JSON with the
    // reproducibility fields (RES-4 / SIM-10).
    let idx = std::fs::read_to_string(runs_dir.join("index.jsonl")).unwrap();
    let lines: Vec<&str> = idx.lines().collect();
    assert_eq!(lines.len(), 2, "expected one line per run:\n{idx}");
    let rec_a: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let rec_b: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(rec_a["run_id"], "ulid-1");
    assert_eq!(rec_b["run_id"], "ulid-2");
    assert_eq!(rec_a["git_sha"], "abc123");
    // Same inputs ⇒ same config hash and same metrics (reproducible, CONV-11).
    assert_eq!(rec_a["config_hash"], rec_b["config_hash"]);
    assert_eq!(rec_a["observations"], 1);
    assert_eq!(rec_a["total"]["coverage"], 1.0);
    // data range = [first, last] merged recv ts: market mark at 1 … whale at 3.
    assert_eq!(rec_a["data_from_ns"], 1);
    assert_eq!(rec_a["data_to_ns"], 3);

    // --run-id without --runs-dir is a usage error, not a silent no-op.
    let bad = Command::new(env!("CARGO_BIN_EXE_whale_study"))
        .arg("--log")
        .arg(&market_path)
        .arg("--run-id")
        .arg("ulid-3")
        .output()
        .expect("binary runs");
    assert!(!bad.status.success());

    let _ = std::fs::remove_file(&market_path);
    let _ = std::fs::remove_file(&positions_path);
}

#[test]
fn liq_11b_whale_study_binary_reports_calibrated_leverage_tiers() {
    let dir = temp_dir("11b");
    let positions_path = dir.join("positions.log");
    let mut tab = SymbolTable::new();
    let btc = tab.intern_default(Venue::Hyperliquid, "BTC");
    // Default tier set {1,2,5,10,20,50}, geometric-midpoint buckets. The NaN
    // liq_price position STILL counts — liq_price is irrelevant to the
    // leverage distribution (unlike the band-accuracy study).
    write_log(
        &positions_path,
        tab.metas().to_vec(),
        vec![
            whale_full(btc, 1, 2.0, 2.0, 100.0, 95.0), // lev 2 → notional 200
            whale_full(btc, 2, 1.0, 5.0, 100.0, f64::NAN), // lev 5 → notional 100
            whale_full(btc, 3, -1.0, 10.0, 100.0, 105.0), // lev 10 → notional 100
            whale_full(btc, 4, 1.0, 100.0, 100.0, 50.0), // lev 100 → tier 50, notional 100
            whale_full(btc, 5, 0.0, 50.0, 100.0, 50.0), // zero size ⇒ skipped
        ],
    );
    let runs_dir = dir.join("runs");
    let out = Command::new(env!("CARGO_BIN_EXE_whale_study"))
        .arg("--log")
        .arg(&positions_path)
        .arg("--leverage-calibration")
        .arg("--json")
        .arg("--run-id")
        .arg("cal-1")
        .arg("--runs-dir")
        .arg(&runs_dir)
        .arg("--git-sha")
        .arg("abc123")
        .output()
        .expect("whale_study binary runs");
    assert!(
        out.status.success(),
        "binary exited {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"study\": \"leverage_calibration\""));
    assert!(stdout.contains("\"n\": 4"), "4 valid samples:\n{stdout}");
    assert!(stdout.contains("\"positions_seen\": 5"));
    // Total notional 500 ⇒ exact decimal weights 0.4/0.2/0.2/0.2.
    for lv in ["2.0", "5.0", "10.0", "50.0"] {
        assert!(
            stdout.contains(&format!("\"leverage\": {lv}")),
            "tier {lv}:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("\"weight\": 0.4"),
        "tier 2 weight:\n{stdout}"
    );
    assert!(
        stdout.contains("\"weight\": 0.2"),
        "tier 5/10/50 weights:\n{stdout}"
    );
    assert!(stdout.contains("\"sum_weights\""));
    // The SIM-10 record is journaled in calibration shape (RES-4/W-6); the
    // journal line is COMPACT JSON (no spaces after colons).
    let idx = std::fs::read_to_string(runs_dir.join("index.jsonl")).unwrap();
    assert!(idx.contains("\"study\":\"leverage_calibration\""), "{idx}");
    assert!(idx.contains("\"run_id\":\"cal-1\""), "{idx}");
    assert!(idx.contains("\"n\":4"), "{idx}");

    let _ = std::fs::remove_file(&positions_path);
}
