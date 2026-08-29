//! Offline feature materialization integration tests (spec 016 / MAT-*).
//! Synthetic recorded event logs → feature engine → FeatureStore Parquet,
//! verifying layout, round-trip, determinism, W-6 versioning, multi-log
//! symbol remap + EVT-5 merge, and the `mp-materialize` CLI.

use mp_core::event::{EventEnvelope, MarketEvent, Side, StatusKind, Venue};
use mp_core::log::EventLogWriter;
use mp_core::{SymbolId, SymbolTable};
use mp_features::{CorrLeg, CorrParams, FeaturesConfig};
use mp_storage::feature_store::{read_feature_meta, read_features};
use mp_storage::{
    load_logs_merged, materialize_logs, materialize_logs_limited, stream_logs_merged,
};
use std::path::{Path, PathBuf};

/// One Bybit day (2026-07-11) with trades/funding/OI in recv order.
const DAY0: i64 = 1_783_728_000_000_000_000; // 2026-07-11T00:00:00Z

fn write_log(path: &Path, table: &SymbolTable, events: &[(i64, SymbolId, MarketEvent)]) {
    write_log_at(path, table, events, Venue::Bybit);
}

fn write_log_at(
    path: &Path,
    table: &SymbolTable,
    events: &[(i64, SymbolId, MarketEvent)],
    venue: Venue,
) {
    let (mut w, _) = EventLogWriter::open(path).unwrap();
    w.write_symbols(table.metas()).unwrap();
    for (i, (ts, sym, body)) in events.iter().enumerate() {
        w.append(&EventEnvelope::new(
            venue,
            *sym,
            *ts - 1,
            *ts,
            i as u64,
            body.clone(),
        ))
        .unwrap();
    }
    w.sync().unwrap();
    drop(w);
}

fn bybit_btc_table() -> SymbolTable {
    let mut t = SymbolTable::new();
    t.intern_default(Venue::Bybit, "BTCUSDT");
    t
}

fn trades(n: i64, base: i64, qty: f64) -> Vec<(i64, SymbolId, MarketEvent)> {
    (0..n)
        .map(|i| {
            (
                base + i * 1_000_000_000,
                SymbolId(0),
                MarketEvent::Trade {
                    price: 100.0,
                    qty,
                    side: if i % 2 == 0 { Side::Buy } else { Side::Sell },
                    trade_id: i as u64,
                },
            )
        })
        .collect()
}

fn walk_parquet(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "parquet") {
                    out.push(p);
                }
            }
        }
    }
    walk(root, &mut out);
    out.sort();
    out
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mp-mat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn mat_6_materialize_writes_layout_and_roundtrips() {
    let dir = tmpdir("layout");
    let log = dir.join("day.log");
    let table = bybit_btc_table();
    let mut evs = trades(5, DAY0, 2.0);
    evs.push((
        DAY0 + 10_000_000_000,
        SymbolId(0),
        MarketEvent::Funding {
            rate: 0.0001,
            interval_s: 28_800,
            next_funding_ts_ns: 0,
        },
    ));
    // Two OI readings so oi.delta emits on the second (delta vs previous).
    evs.push((
        DAY0 + 20_000_000_000,
        SymbolId(0),
        MarketEvent::OpenInterest {
            oi_contracts: 1000.0,
            oi_notional: f64::NAN,
        },
    ));
    evs.push((
        DAY0 + 30_000_000_000,
        SymbolId(0),
        MarketEvent::OpenInterest {
            oi_contracts: 1200.0,
            oi_notional: f64::NAN,
        },
    ));
    write_log(&log, &table, &evs);

    let out = dir.join("features");
    let cfg = FeaturesConfig::default();
    let stats = materialize_logs(&out, &cfg, std::slice::from_ref(&log), "abc123").unwrap();

    assert_eq!(stats.events_read, 8);
    assert!(stats.updates >= 7, "5 cvd trades + funding + 1 oi delta");
    assert!(stats.rows_written >= 7);
    assert!(
        stats.files_written >= 3,
        "cvd, funding.rate, oi.delta files"
    );
    assert!(
        stats.features.iter().any(|f| f == "cvd.bybit"),
        "features: {:?}",
        stats.features
    );
    assert!(stats.nan_suppressed == 0);

    // Layout: {root}/{feature}/ver=N/venue=bybit/symbol=0/{date}.parquet.
    let cvd = out
        .join("cvd.bybit")
        .join("ver=0")
        .join("venue=bybit")
        .join("symbol=0");
    assert!(cvd.join("2026-07-11.parquet").exists(), "cvd file missing");
    let files = walk_parquet(&out);
    assert!(files.len() >= 3);

    // Round-trip: read back and check FEA-6 footer + a known value.
    let rows = read_features(&cvd.join("2026-07-11.parquet")).unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].ts_ns, DAY0);
    assert_eq!(rows[0].value, 2.0, "first trade: +2.0 buy");
    assert_eq!(rows[0].venue_code, Venue::Bybit as u16);
    let meta = read_feature_meta(&cvd.join("2026-07-11.parquet"))
        .unwrap()
        .unwrap();
    assert_eq!(meta.params_hash, cfg.params_hash().unwrap());
    assert_eq!(meta.engine_git_sha, "abc123");
    assert_eq!(meta.feature_ver, 1, "default cvd feature ver");

    let _ = std::fs::remove_dir_all(&dir);
}

/// COL-29 (spec 024): a recording that carries Liquidation events must
/// materialize the liq.* flow features end-to-end — per-side rolling
/// notional, event rate, and price-distance from mid — proving the
/// feature-store path, not just unit-level emission.
#[test]
fn mat_6_liq_flow_features_materialize_from_liquidation_log() {
    let dir = tmpdir("liqflow");
    let log = dir.join("day.log");
    let table = bybit_btc_table();
    let mut evs: Vec<(i64, SymbolId, MarketEvent)> = vec![(
        DAY0,
        SymbolId(0),
        MarketEvent::BookSnapshot {
            bids: vec![(100.0, 6.0)].into(),
            asks: vec![(101.0, 2.0)].into(),
            seq: 100,
            depth: 2,
            reason: mp_core::SnapshotReason::Init,
        },
    )];
    for (i, (price, qty, side)) in [
        (100.0, 2.0, Side::Buy),
        (101.0, 3.0, Side::Sell),
        (99.0, 1.0, Side::Buy),
    ]
    .into_iter()
    .enumerate()
    {
        evs.push((
            DAY0 + (i as i64 + 1) * 1_000_000_000,
            SymbolId(0),
            MarketEvent::Liquidation { price, qty, side },
        ));
    }
    write_log(&log, &table, &evs);

    let out = dir.join("features");
    let mut cfg = FeaturesConfig::default();
    cfg.liq_flow.window_ns = 300_000_000_000;
    let stats = materialize_logs(&out, &cfg, std::slice::from_ref(&log), "abc123").unwrap();
    assert!(stats.nan_suppressed == 0);

    for feat in ["liq.vol_buy", "liq.vol_sell", "liq.rate", "liq.dist"] {
        let parquet = out
            .join(feat)
            .join("ver=0")
            .join("venue=bybit")
            .join("symbol=0")
            .join("2026-07-11.parquet");
        assert!(parquet.exists(), "{feat} file missing");
        assert!(
            !read_features(&parquet).unwrap().is_empty(),
            "{feat} must have materialized rows"
        );
    }
    // vol_buy rolls 100x2 + 99x1 = 299; the sell-side 101x3 = 303 is separate.
    let buy = read_features(
        &out.join("liq.vol_buy")
            .join("ver=0")
            .join("venue=bybit")
            .join("symbol=0")
            .join("2026-07-11.parquet"),
    )
    .unwrap();
    assert_eq!(buy.last().unwrap().value, 299.0);
    let rate = read_features(
        &out.join("liq.rate")
            .join("ver=0")
            .join("venue=bybit")
            .join("symbol=0")
            .join("2026-07-11.parquet"),
    )
    .unwrap();
    assert_eq!(rate.last().unwrap().value, 3.0 / 300.0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// COL-29 (spec 004 §Liquidation flow): the cross-venue `liq.delta.{a}_{b}`
/// divergence materializes from TWO venue logs — proving the global feature
/// seam (spec 023 FEA-20) works through the real merge path, not just the
/// unit-level engine. Bybit squeezes shorts, Okx dumps longs → positive
/// divergence on the merged day.
#[test]
fn mat_6_cross_venue_liq_delta_materializes_from_two_logs() {
    let dir = tmpdir("liqdelta");
    let bybit_log = dir.join("bybit.log");
    let okx_log = dir.join("okx.log");
    let mut bybit_t = SymbolTable::new();
    bybit_t.intern_default(Venue::Bybit, "BTCUSDT");
    let mut okx_t = SymbolTable::new();
    okx_t.intern_default(Venue::Okx, "BTC-USDT");
    // Bybit: buy (shorts squeezed) 100x5 = 500. Okx: sell (longs dumped)
    // 100x3 = 300. Same recv clock so the windows overlap.
    let bybit_evs: Vec<(i64, SymbolId, MarketEvent)> = vec![(
        DAY0 + 1_000_000_000,
        SymbolId(0),
        MarketEvent::Liquidation {
            price: 100.0,
            qty: 5.0,
            side: Side::Buy,
        },
    )];
    let okx_evs: Vec<(i64, SymbolId, MarketEvent)> = vec![(
        DAY0 + 2_000_000_000,
        SymbolId(0),
        MarketEvent::Liquidation {
            price: 100.0,
            qty: 3.0,
            side: Side::Sell,
        },
    )];
    write_log_at(&bybit_log, &bybit_t, &bybit_evs, Venue::Bybit);
    write_log_at(&okx_log, &okx_t, &okx_evs, Venue::Okx);

    let out = dir.join("features");
    let mut cfg = FeaturesConfig::default();
    cfg.liq_delta.pairs = vec![["bybit".into(), "okx".into()]];
    let logs = [bybit_log.clone(), okx_log.clone()];
    let stats = materialize_logs(&out, &cfg, &logs, "abc123").unwrap();
    assert!(stats.nan_suppressed == 0);
    let feat = "liq.delta.bybit_okx";
    let files = walk_parquet(&out);
    assert!(
        files.iter().any(|p| p.to_string_lossy().contains(feat)),
        "{feat} parquet missing: {files:?}"
    );
    // One row per venue's liquidation, each stamped with the divergence AS OF
    // that event: bybit buy (t+1s, okx hasn't sold yet) → (500-0)-(0-0) =
    // 500; okx sell (t+2s) → (500-0)-(0-300) = 800. The cross-venue state
    // lives in ONE global instance across both symbols — the FEA-20 seam.
    let mut rows_all = Vec::new();
    for f in &files {
        if f.to_string_lossy().contains(feat) {
            rows_all.extend(read_features(f).unwrap());
        }
    }
    rows_all.sort_by_key(|r| r.ts_ns);
    let values: Vec<f64> = rows_all.iter().map(|r| r.value).collect();
    assert_eq!(values, vec![500.0, 800.0], "as-of divergence per event");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cor_3_materializes_btc_eth_from_own_recorded_trade_logs() {
    let dir = tmpdir("corr");
    let btc_log = dir.join("btc.log");
    let eth_log = dir.join("eth.log");
    let mut btc_table = SymbolTable::new();
    btc_table.intern_default(Venue::Hyperliquid, "BTC");
    let mut eth_table = SymbolTable::new();
    eth_table.intern_default(Venue::Hyperliquid, "ETH");

    let mut btc = 100.0;
    let mut eth = 100.0;
    let mut btc_events = Vec::new();
    let mut eth_events = Vec::new();
    for day in 0..14i64 {
        if day > 0 {
            let step = day as f64 / 100.0;
            btc *= 1.0 + step;
            eth *= 1.0 + step;
        }
        let ts = DAY0 + day * 86_400_000_000_000;
        btc_events.push((
            ts,
            SymbolId(0),
            MarketEvent::Trade {
                price: btc,
                qty: 1.0,
                side: Side::Buy,
                trade_id: day as u64,
            },
        ));
        eth_events.push((
            ts,
            SymbolId(0),
            MarketEvent::Trade {
                price: eth,
                qty: 1.0,
                side: Side::Buy,
                trade_id: day as u64,
            },
        ));
    }
    write_log_at(&btc_log, &btc_table, &btc_events, Venue::Hyperliquid);
    write_log_at(&eth_log, &eth_table, &eth_events, Venue::Hyperliquid);

    let mut cfg = FeaturesConfig::default();
    cfg.corr.enabled = true;
    cfg.corr.pairs = vec![CorrParams {
        window_days: 30,
        min_overlap_days: 5,
        leg_a: CorrLeg {
            venue: "hyperliquid".into(),
            symbol: "BTC".into(),
        },
        leg_b: CorrLeg {
            venue: "hyperliquid".into(),
            symbol: "ETH".into(),
        },
    }];
    let out = dir.join("features");
    let stats = materialize_logs(&out, &cfg, &[btc_log, eth_log], "abc123").unwrap();
    assert!(stats
        .features
        .iter()
        .any(|feature| feature == "corr.btc_eth"));

    let rows: Vec<_> = walk_parquet(&out)
        .into_iter()
        .filter(|path| path.to_string_lossy().contains("corr.btc_eth"))
        .flat_map(|path| read_features(&path).expect("correlation parquet reads"))
        .collect();
    assert!(!rows.is_empty(), "corr.btc_eth must persist after warmup");
    assert!(rows.iter().all(|row| row.value > 0.99));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mat_5_materialize_is_deterministic_and_idempotent() {
    let dir = tmpdir("determinism");
    let log = dir.join("day.log");
    write_log(&log, &bybit_btc_table(), &trades(3, DAY0, 1.0));
    let out_a = dir.join("a");
    let out_b = dir.join("b");

    let cfg = FeaturesConfig::default();
    let s1 = materialize_logs(&out_a, &cfg, std::slice::from_ref(&log), "sha-x").unwrap();
    let s2 = materialize_logs(&out_b, &cfg, std::slice::from_ref(&log), "sha-x").unwrap();
    assert_eq!(s1.rows_written, s2.rows_written);
    assert!(s1.files_written >= 1, "cvd.bybit file (3 trades)");

    // MAT-5: two independent runs must produce BYTE-identical Parquet.
    let files_a = walk_parquet(&out_a);
    let files_b = walk_parquet(&out_b);
    assert_eq!(files_a.len(), files_b.len());
    for (a, b) in files_a.iter().zip(files_b.iter()) {
        assert_eq!(
            std::fs::read(a).unwrap(),
            std::fs::read(b).unwrap(),
            "deterministic bytes: {} vs {}",
            a.display(),
            b.display()
        );
    }

    // Idempotent: same params hash → same ver dir, and a re-run against the
    // SAME root must not rewrite any file (W-6 no-overwrite guard).
    for f in &files_a {
        assert!(f.to_string_lossy().contains("ver=0"));
    }
    let pre: Vec<(PathBuf, Vec<u8>)> = files_a
        .into_iter()
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect();
    let s3 = materialize_logs(&out_a, &cfg, std::slice::from_ref(&log), "sha-x").unwrap();
    assert_eq!(s3.rows_written, s1.rows_written);
    assert_eq!(
        walk_parquet(&out_a).len(),
        pre.len(),
        "no new files on re-run"
    );
    for (p, bytes) in &pre {
        assert_eq!(
            &std::fs::read(p).unwrap(),
            bytes,
            "re-run must leave recorded bytes untouched (W-6)"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fea_6_params_change_allocates_new_version_and_never_overwrites() {
    let dir = tmpdir("versioning");
    let log = dir.join("day.log");
    write_log(&log, &bybit_btc_table(), &trades(2, DAY0, 1.0));
    let out = dir.join("features");

    // Run 1: default config → ver=0.
    let cfg_a = FeaturesConfig::default();
    materialize_logs(&out, &cfg_a, std::slice::from_ref(&log), "sha").unwrap();
    assert!(out.join("cvd.bybit").join("ver=0").is_dir());

    // Run 2: a real param change that KEEPS cvd.bybit registered (bar_tf_ns)
    // → the same feature gets NEW ver=1, and ver=0 is untouched (W-6).
    let cfg_b = FeaturesConfig {
        bar_tf_ns: 120_000_000_000,
        ..FeaturesConfig::default()
    };
    materialize_logs(&out, &cfg_b, std::slice::from_ref(&log), "sha").unwrap();
    assert!(
        out.join("cvd.bybit")
            .join("ver=0")
            .join("venue=bybit")
            .join("symbol=0")
            .join("2026-07-11.parquet")
            .exists(),
        "ver=0 must never be overwritten (W-6)"
    );
    assert!(
        out.join("cvd.bybit")
            .join("ver=1")
            .join("venue=bybit")
            .join("symbol=0")
            .join("2026-07-11.parquet")
            .exists(),
        "changed params must allocate ver=1"
    );

    // Re-running with cfg_a reuses ver=0 (idempotent, FEA-6 resolve).
    materialize_logs(&out, &cfg_a, std::slice::from_ref(&log), "sha").unwrap();
    assert_eq!(
        walk_parquet(&out)
            .iter()
            .filter(|p| p.to_string_lossy().contains("cvd.bybit")
                && p.to_string_lossy().contains("ver=0"))
            .count(),
        1,
        "no second ver=0 file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mat_6_multi_log_symbols_remap_and_merge() {
    // Two logs, each with its OWN local symbol table (BTCUSDT is id 0 in both)
    // and interleaved recv times → the engine must see ONE merged stream and
    // one shared cvd accumulator (same (venue, symbol) ⇒ same shared id).
    let dir = tmpdir("merge");
    let log_a = dir.join("a.log");
    let log_b = dir.join("b.log");
    write_log(&log_a, &bybit_btc_table(), &trades(2, DAY0, 1.0));
    write_log(
        &log_b,
        &bybit_btc_table(),
        &trades(2, DAY0 + 500_000_000, 1.0),
    );

    let out = dir.join("features");
    let stats = materialize_logs(&out, &FeaturesConfig::default(), &[log_a, log_b], "sha").unwrap();
    assert_eq!(stats.events_read, 4);

    // Merged cvd: interleaved recv order (A0, B0, A1, B1 by recv_ts) with
    // buy(+1)/sell(-1) alternating per log → cumulative 1, 2, 1, 0. The
    // accumulator is SHARED across logs (same (venue,symbol) ⇒ same shared
    // symbol id), so log B's first trade lands on log A's state.
    let cvd_path = out
        .join("cvd.bybit")
        .join("ver=0")
        .join("venue=bybit")
        .join("symbol=0")
        .join("2026-07-11.parquet");
    let rows = read_features(&cvd_path).unwrap();
    assert_eq!(rows.len(), 4);
    // A0 buy(+1)→1.0; B0 buy(+1)→2.0; A1 sell(−1)→1.0; B1 sell(−1)→0.0.
    let vals: Vec<f64> = rows.iter().map(|r| r.value).collect();
    assert_eq!(vals, vec![1.0, 2.0, 1.0, 0.0], "cross-log cvd must merge");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mat_6_materialize_skips_missing_config_file_cleanly() {
    // A missing log is an error (fail-closed), not a silent empty run.
    let dir = tmpdir("missing");
    let out = dir.join("features");
    let err = materialize_logs(
        &out,
        &FeaturesConfig::default(),
        &[dir.join("nope.log")],
        "sha",
    )
    .unwrap_err();
    assert!(err.contains("nope.log"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mat_6_cli_materializes_and_exits_zero() {
    // End-to-end through the shipped binary (CARGO_BIN_EXE_mp-materialize).
    let dir = tmpdir("cli");
    let log = dir.join("day.log");
    write_log(&log, &bybit_btc_table(), &trades(3, DAY0, 0.5));
    let out = dir.join("features");

    let bin = env!("CARGO_BIN_EXE_mp-materialize");
    let out_s = out.to_string_lossy().to_string();
    let out_handle = std::process::Command::new(bin)
        .args([
            "--log",
            log.to_str().unwrap(),
            "--out",
            &out_s,
            "--git-sha",
            "cli-test",
        ])
        .output()
        .unwrap();
    assert!(
        out_handle.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_handle.stderr)
    );
    let stdout = String::from_utf8_lossy(&out_handle.stdout);
    assert!(stdout.contains("materialized:"), "{stdout}");
    assert!(stdout.contains("rows="), "{stdout}");
    assert!(!walk_parquet(&out).is_empty(), "cli must write parquet");

    // Determinism across two CLI runs.
    let h1 = walk_parquet(&out);
    let out2 = dir.join("features2");
    let out2_s = out2.to_string_lossy().to_string();
    let h2h = std::process::Command::new(bin)
        .args([
            "--log",
            log.to_str().unwrap(),
            "--out",
            &out2_s,
            "--git-sha",
            "cli-test",
        ])
        .output()
        .unwrap();
    assert!(h2h.status.success());
    let h2 = walk_parquet(&out2);
    assert_eq!(h1.len(), h2.len());
    for (a, b) in h1.iter().zip(h2.iter()) {
        let ra = read_features(a).unwrap();
        let rb = read_features(b).unwrap();
        assert_eq!(ra, rb, "CLI runs must produce identical rows");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Two logs with DIFFERENT (venue, symbol) per log, passed in both orders.
/// Symbol ids would differ under argument-order-dependent interning (audit
/// 2026-08-06); canonical sort must make `--log a --log b` ≡ `--log b --log a`
/// down to the byte.
#[test]
fn mat_5_log_argument_order_does_not_change_symbol_ids_or_bytes() {
    let dir = tmpdir("order");
    let log_a = dir.join("a.log");
    let log_b = dir.join("b.log");
    let mut ta = SymbolTable::new();
    ta.intern_default(Venue::Bybit, "BTCUSDT");
    write_log(&log_a, &ta, &trades(3, DAY0, 1.0));
    let mut tb = SymbolTable::new();
    tb.intern_default(Venue::Bybit, "ETHUSDT");
    write_log(&log_b, &tb, &trades(3, DAY0 + 1_000_000_000, 1.0));

    let out_ab = dir.join("ab");
    let out_ba = dir.join("ba");
    let cfg = FeaturesConfig::default();
    let s_ab = materialize_logs(&out_ab, &cfg, &[log_a.clone(), log_b.clone()], "sha").unwrap();
    let s_ba = materialize_logs(&out_ba, &cfg, &[log_b.clone(), log_a.clone()], "sha").unwrap();

    // Identical stats — including the symbols table identity.
    assert_eq!(s_ab.rows_written, s_ba.rows_written);
    assert_eq!(s_ab.files_written, s_ba.files_written);
    assert_eq!(s_ab.features, s_ba.features);
    assert_eq!(s_ab.symbols, 2);
    assert_eq!(
        s_ab.symbols_hash, s_ba.symbols_hash,
        "same table => same hash"
    );

    // Same relative file set, byte-identical content (MAT-5).
    let rel = |root: &Path| -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = walk_parquet(root)
            .into_iter()
            .map(|p| {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                (rel, std::fs::read(&p).unwrap())
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        rel(&out_ab),
        rel(&out_ba),
        "arg order must not change layout or bytes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The shared symbol table is persisted as `{out}/symbols/{hash}.json` and the
/// hash is recorded in every Parquet footer — a consumer can resolve the
/// numeric `symbol_id` columns (audit 2026-08-06: the store is now
/// self-describing). Canonical ordering makes BTCUSDT id 0 here.
#[test]
fn mat_5_symbols_snapshot_written_and_resolves_ids() {
    let dir = tmpdir("snapshot");
    let log_a = dir.join("a.log");
    let log_b = dir.join("b.log");
    let mut ta = SymbolTable::new();
    ta.intern_default(Venue::Bybit, "BTCUSDT");
    write_log(&log_a, &ta, &trades(3, DAY0, 1.0));
    let mut tb = SymbolTable::new();
    tb.intern_default(Venue::Bybit, "ETHUSDT");
    write_log(&log_b, &tb, &trades(3, DAY0 + 1_000_000_000, 1.0));

    let out = dir.join("features");
    let stats = materialize_logs(&out, &FeaturesConfig::default(), &[log_a, log_b], "sha").unwrap();
    assert!(!stats.symbols_hash.is_empty());
    assert_eq!(stats.symbols, 2); // Snapshot exists at {root}/symbols/{hash}.json and parses to the table in
                                  // id order; canonical sort gives BTCUSDT id 0, ETHUSDT id 1.
    let path = out
        .join("symbols")
        .join(format!("{}.json", stats.symbols_hash));
    assert!(path.exists(), "snapshot missing: {}", path.display());
    let rows: Vec<mp_storage::SymbolRow> =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("snapshot parses");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, 0);
    assert_eq!(rows[0].venue_symbol, "BTCUSDT");
    assert_eq!(rows[1].venue_symbol, "ETHUSDT");

    // Every Parquet footer records the same hash — read the footer, resolve
    // the row's symbol_id through the snapshot.
    let files = walk_parquet(&out);
    assert!(!files.is_empty());
    for f in &files {
        let meta = read_feature_meta(f).unwrap().expect("feature footer");
        assert_eq!(meta.symbols_hash, stats.symbols_hash);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Passing the SAME path twice must behave exactly like passing it once
/// (canonical dedup, MAT-5) — a duplicated log would otherwise double its
/// events and corrupt cumulative features.
#[test]
fn mat_5_duplicate_log_paths_are_deduped() {
    let dir = tmpdir("dedup");
    let log = dir.join("day.log");
    write_log(&log, &bybit_btc_table(), &trades(3, DAY0, 1.0));
    let out_one = dir.join("one");
    let out_two = dir.join("two");
    let cfg = FeaturesConfig::default();
    let s1 = materialize_logs(&out_one, &cfg, std::slice::from_ref(&log), "sha").unwrap();
    let s2 = materialize_logs(&out_two, &cfg, &[log.clone(), log.clone()], "sha").unwrap();
    assert_eq!(
        s1.events_read, s2.events_read,
        "duplicate path must be dropped"
    );
    assert_eq!(s1.rows_written, s2.rows_written);
    assert_eq!(s1.files_written, s2.files_written);
    assert_eq!(s1.symbols_hash, s2.symbols_hash);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The RAM guard fails closed BEFORE any log is opened: a corpus over the cap
/// is an explicit error with guidance, and nothing is written.
#[test]
fn mat_5_ram_guard_rejects_oversized_corpus_before_reading() {
    let dir = tmpdir("guard");
    let log = dir.join("day.log");
    write_log(&log, &bybit_btc_table(), &trades(3, DAY0, 1.0));
    let out = dir.join("features");
    let err = materialize_logs_limited(
        &out,
        &FeaturesConfig::default(),
        &[log],
        "sha",
        Some(1), // 1-byte cap: any real log trips the guard
    )
    .unwrap_err();
    assert!(err.contains("MP_MATERIALIZE_MAX_BYTES"), "{err}");
    assert!(err.to_lowercase().contains("slice"), "{err}");
    assert!(walk_parquet(&out).is_empty(), "guard must write nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- streaming loader (spec 018 determinism RAM guard) ----

/// The streaming loader (frame-by-frame, never materializing) must emit the
/// EXACT canonical stream the eager loader does — same order, same shared-id
/// remap, same drop rules — or the determinism replay would measure a
/// different decision stream than materialization (MAT-5 / EVT-8).
#[test]
fn evt_8_stream_merge_matches_eager_merge_order_and_remap() {
    let dir = tmpdir("stream");
    // Two logs whose LOCAL id 0 means different things (EVT-8 collision):
    // A: BTCUSDT=0, ETHUSDT=1; B: BTCUSDT=0, SOLUSDT=1. Canonical sort (A < B)
    // interns BTC=0, ETH=1, then SOL=2 — local ids must land on shared ids.
    let mut ta = SymbolTable::new();
    ta.intern_default(Venue::Bybit, "BTCUSDT");
    ta.intern_default(Venue::Bybit, "ETHUSDT");
    let mut tb = SymbolTable::new();
    tb.intern_default(Venue::Bybit, "BTCUSDT");
    tb.intern_default(Venue::Bybit, "SOLUSDT");
    let log_a = dir.join("20260818_a.log");
    let log_b = dir.join("20260818_b.log");
    let t = |price: f64, id: u64| MarketEvent::Trade {
        price,
        qty: 1.0,
        side: Side::Buy,
        trade_id: id,
    };
    // Interleaved timestamps so the k-way merge actually interleaves sources.
    write_log_at(
        &log_a,
        &ta,
        &[
            (DAY0 + 1_000_000_000, SymbolId(0), t(1.0, 1)), // BTC -> shared 0
            (DAY0 + 3_000_000_000, SymbolId(1), t(2.0, 2)), // ETH -> shared 1
        ],
        Venue::Bybit,
    );
    write_log_at(
        &log_b,
        &tb,
        &[
            (DAY0 + 2_000_000_000, SymbolId(0), t(3.0, 3)), // BTC -> shared 0
            (DAY0 + 4_000_000_000, SymbolId(1), t(4.0, 4)), // SOL -> shared 2
        ],
        Venue::Bybit,
    );

    let eager = load_logs_merged(&[log_a.clone(), log_b.clone()]).unwrap();
    let mut streamed = stream_logs_merged(&[log_a, log_b]).unwrap();
    let mut got = Vec::new();
    for item in streamed.by_ref() {
        let ev = item.unwrap();
        got.push((ev.recv_ts_ns, ev.stream_seq, ev.symbol.0, ev.body));
    }
    let want: Vec<(i64, u64, u32, MarketEvent)> = eager
        .events
        .iter()
        .map(|e| (e.recv_ts_ns, e.stream_seq, e.symbol.0, e.body.clone()))
        .collect();
    assert_eq!(
        got, want,
        "streamed order + remap must equal the eager loader"
    );
    assert!(
        got.iter().any(|(_, _, id, _)| *id == 2),
        "SOL's local id 1 must land on shared id 2 (EVT-8 remap)"
    );
    assert_eq!(streamed.events_read, got.len() as u64);
    assert_eq!(eager.events_read, got.len() as u64);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A symbol-less log carrying only control/Status events streams to empty
/// (same rule as the eager loader: drop control events, do not fail).
#[test]
fn stream_merge_symbol_less_log_drops_control_events() {
    let dir = tmpdir("stream-nosym");
    let log = dir.join("20260818_whale.log");
    write_log_at(
        &log,
        &SymbolTable::new(),
        &[
            (
                DAY0 + 1,
                SymbolId(0),
                MarketEvent::Status {
                    kind: StatusKind::GapDetected,
                    detail: String::new(),
                },
            ),
            (
                DAY0 + 2,
                SymbolId(0),
                MarketEvent::Status {
                    kind: StatusKind::VenueHalt,
                    detail: String::new(),
                },
            ),
        ],
        Venue::Hyperliquid,
    );
    let mut streamed = stream_logs_merged(&[log]).unwrap();
    assert!(streamed.next().is_none(), "status-only log streams empty");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A symbol-less log carrying a DATA event fails closed, never silently
/// drops (MAT-5 parity with the eager loader).
#[test]
fn stream_merge_fails_closed_on_data_event_without_symbols() {
    let dir = tmpdir("stream-nosym-data");
    let log = dir.join("20260818_whale.log");
    write_log_at(
        &log,
        &SymbolTable::new(),
        &[(
            DAY0 + 1,
            SymbolId(0),
            MarketEvent::Trade {
                price: 1.0,
                qty: 1.0,
                side: Side::Buy,
                trade_id: 1,
            },
        )],
        Venue::Hyperliquid,
    );
    let mut streamed = stream_logs_merged(&[log]).unwrap();
    let err = streamed.next().unwrap().unwrap_err();
    assert!(err.contains("corrupt log"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The merge writer path (mp-query merge) re-interns symbols onto a shared
/// table and emits the same canonical event sequence as the eager loader —
/// the merged log is the sim's multi-day replay input, so it must round-trip
/// through `load_logs_merged` with identical events and a resolvable symbol
/// table (MAT-5/EVT-8 parity).
#[test]
fn mat_5_merge_round_trips_shared_symbol_table() {
    use mp_core::log::LogReader;

    let dir = tmpdir("merge-roundtrip");
    let log_a = dir.join("20260711_bybit_BTCUSDT.log");
    let log_b = dir.join("20260712_bybit_ETHUSDT.log");
    let mut ta = SymbolTable::new();
    ta.intern_default(Venue::Bybit, "BTCUSDT");
    write_log(&log_a, &ta, &trades(3, DAY0, 1.0));
    let mut tb = SymbolTable::new();
    tb.intern_default(Venue::Bybit, "ETHUSDT");
    write_log(&log_b, &tb, &trades(3, DAY0 + 1_000_000_000, 1.0));

    // Write the merged log exactly as mp-query merge does: shared table header
    // + streamed canonical-order events.
    let merged_log = dir.join("merged.log");
    {
        let merged = stream_logs_merged(&[log_a.clone(), log_b.clone()]).unwrap();
        let mut w = EventLogWriter::open(&merged_log).unwrap().0;
        w.write_symbols(merged.symbols.metas()).unwrap();
        for ev in merged {
            w.append(&ev.unwrap()).unwrap();
        }
        w.sync().unwrap();
    }

    // Round-trip through the eager loader: same events, same symbol count,
    // ids resolve to the shared table (BTCUSDT id 0, ETHUSDT id 1).
    let loaded = load_logs_merged(std::slice::from_ref(&merged_log)).unwrap();
    assert_eq!(loaded.events.len(), 6);
    assert_eq!(loaded.symbols.metas().len(), 2);
    assert_eq!(loaded.symbols.metas()[0].venue_symbol, "BTCUSDT");
    assert_eq!(loaded.symbols.metas()[1].venue_symbol, "ETHUSDT");

    // Re-opening as a plain log yields the same events (the writer produced a
    // readable log, not a symbol-table-only header).
    let mut reader = LogReader::open(&merged_log).unwrap();
    reader.load_symbols().unwrap();
    let replayed: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(replayed.len(), 6);
    assert_eq!(replayed[0].recv_ts_ns, DAY0);
    assert_eq!(replayed[5].recv_ts_ns, DAY0 + 3 * 1_000_000_000);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ibi_10_cli_cross_out_writes_leadlag_table() {
    // End-to-end through the shipped binary (CARGO_BIN_EXE_mp-materialize):
    // two venue logs + --cross-out produce the lead-lag Parquet table.
    let dir = tmpdir("ibi10cli");
    let cboe = dir.join("cboe.log");
    let deribit = dir.join("deribit.log");

    // Minimal symbol tables: one IBIT contract (cboe) + one BTC perp ticker
    // symbol (deribit). Events are OptionTickers on consecutive days.
    let mut cboe_syms = mp_core::SymbolTable::new();
    let ibit_sym = cboe_syms.intern(Venue::Cboe, "IBIT260918C00045000", |id| {
        let mut m = mp_core::SymbolMeta::new(
            id,
            Venue::Cboe,
            "IBIT260918C00045000",
            "IBIT",
            "USD",
            mp_core::InstrumentKind::Option,
            0.01,
            0.01,
            1.0,
        );
        m.contract_multiplier = 100.0;
        m
    });
    let mut deriv_syms = mp_core::SymbolTable::new();
    let btc_sym = deriv_syms.intern(Venue::Deribit, "BTC-PERPETUAL", |id| {
        mp_core::SymbolMeta::new(
            id,
            Venue::Deribit,
            "BTC-PERPETUAL",
            "BTC",
            "USD",
            mp_core::InstrumentKind::Perp,
            0.5,
            0.01,
            1.0,
        )
    });

    let day_ns = 86_400_000_000_000i64;
    let mk = |venue: Venue, sym: SymbolId, day: i64, iv: f64, oi: f64, delta: f64| {
        (
            day * day_ns + 3_600_000_000_000,
            sym,
            MarketEvent::OptionTicker {
                leg: mp_core::OptionLeg {
                    underlying: if venue == Venue::Cboe {
                        "IBIT".into()
                    } else {
                        "BTC".into()
                    },
                    strike: 50.0,
                    expiry_ts_ns: 0,
                    kind: mp_core::OptionKind::Call,
                },
                mark_iv: iv,
                mark_price: 5.0,
                underlying_price: 100.0,
                open_interest: oi,
                greeks: Some(mp_core::OptionGreeks {
                    delta,
                    gamma: 0.0,
                    theta: 0.0,
                    vega: 0.0,
                }),
            },
        )
    };

    let mut cboe_events: Vec<(i64, SymbolId, MarketEvent)> = Vec::new();
    let mut deribit_events: Vec<(i64, SymbolId, MarketEvent)> = Vec::new();
    for d in 1..=3i64 {
        cboe_events.push(mk(Venue::Cboe, ibit_sym, d, 0.40, 1000.0, 0.5));
        deribit_events.push(mk(Venue::Deribit, btc_sym, d, 0.55, 100.0, 0.4));
    }
    write_log_at(&cboe, &cboe_syms, &cboe_events, Venue::Cboe);
    write_log_at(&deribit, &deriv_syms, &deribit_events, Venue::Deribit);

    let cfg = dir.join("features.toml");
    std::fs::write(
        &cfg,
        "[ibit_cross]\nderiv_underlying = \"BTC\"\nmin_overlap_days = 1\n",
    )
    .unwrap();
    let cross_out = dir.join("cross/leadlag.parquet");
    let bin = env!("CARGO_BIN_EXE_mp-materialize");
    let out_handle = std::process::Command::new(bin)
        .args([
            "--log",
            cboe.to_str().unwrap(),
            "--log",
            deribit.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
            "--cross-out",
            cross_out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out_handle.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_handle.stderr)
    );
    let stdout = String::from_utf8_lossy(&out_handle.stdout);
    assert!(stdout.contains("cross-market rows="), "{stdout}");

    let rows = mp_storage::ibit_leadlag::read_leadlag(&cross_out).unwrap();
    assert_eq!(rows.len(), 2, "days 1 and 2 completed by days 2/3");
    assert!((rows[0].iv_divergence - (0.40 - 0.55)).abs() < 1e-12);
}
