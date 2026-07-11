//! Acceptance tests for spec 003. Test names embed requirement IDs (CONV-21).

use mp_core::{
    EventEnvelope, InstrumentKind, MarketEvent, Side, SnapshotReason, StatusKind, Venue,
};
use mp_core::{SymbolId, SymbolTable};
use mp_storage::manifest::{derive_manifest, GapKind};
use mp_storage::scd2::{SymbolScd2, SymbolVersion};
use mp_storage::{compact_day, prune, Dataset};

const DAY: i64 = 86_400_000_000_000; // 1 day in ns

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mpstore-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn trade(sym: SymbolId, recv: i64, seq: u64, price: f64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        sym,
        recv,
        recv,
        seq,
        MarketEvent::Trade {
            price,
            qty: 1.0,
            side: Side::Buy,
            trade_id: seq,
        },
    )
}

fn status(sym: SymbolId, recv: i64, kind: StatusKind) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        sym,
        recv,
        recv,
        0,
        MarketEvent::Status {
            kind,
            detail: String::new(),
        },
    )
}

fn table() -> (SymbolTable, SymbolId) {
    let mut t = SymbolTable::new();
    let id = t.intern(Venue::Bybit, "BTCUSDT", |id| {
        mp_core::SymbolMeta::new(
            id,
            Venue::Bybit,
            "BTCUSDT",
            "BTC",
            "USDT",
            InstrumentKind::Perp,
            0.1,
            0.001,
            5.0,
        )
    });
    (t, id)
}

#[test]
fn sto_1_and_4_trades_roundtrip_and_idempotent() {
    let root = tmp("sto14");
    let (syms, btc) = table();
    let events = vec![
        trade(btc, 10, 1, 100.0),
        trade(btc, 20, 2, 101.0),
        trade(btc, 30, 3, 99.5),
    ];

    let stats = compact_day(
        &root,
        Venue::Bybit,
        "2026-07-10",
        0,
        DAY,
        events.clone(),
        &syms,
        "hashA",
        "gitsha1",
        0,
    )
    .unwrap();
    assert_eq!(stats.trades_files_written, 1);
    assert_eq!(stats.trade_rows, 3);

    // STO-4: read back identical (order preserved).
    let ds = Dataset::open(&root);
    let got = ds
        .trades_day(Venue::Bybit, "BTCUSDT", "2026-07-10")
        .unwrap();
    assert_eq!(got.len(), 3);
    for (a, b) in events.iter().zip(got.iter()) {
        assert_eq!(a, b);
    }

    // STO-1: re-run with same source hash is a no-op.
    let stats2 = compact_day(
        &root,
        Venue::Bybit,
        "2026-07-10",
        0,
        DAY,
        events,
        &syms,
        "hashA",
        "gitsha1",
        0,
    )
    .unwrap();
    assert_eq!(stats2.trades_files_written, 0);
    assert_eq!(stats2.trades_files_skipped, 1);
}

#[test]
fn sto_2_manifest_disconnect_gap_and_coverage() {
    let (_syms, btc) = table();
    // Disconnect at t=1000, reconnect at t=1000+half-day ⇒ coverage 0.5 exactly.
    let half = DAY / 2;
    let events = vec![
        trade(btc, 100, 1, 100.0),
        status(btc, 1000, StatusKind::Disconnected),
        status(btc, 1000 + half, StatusKind::Connected),
        trade(btc, DAY - 1, 2, 100.0),
    ];
    let m = derive_manifest(
        "bybit",
        "2026-07-10",
        0,
        DAY,
        events.into_iter(),
        |_| "BTCUSDT".to_string(),
        "gitsha1",
        0,
    );
    let s = m.streams.get("trades:BTCUSDT").expect("trades stream");
    assert_eq!(s.events, 2);
    assert_eq!(s.gaps.len(), 1);
    assert_eq!(s.gaps[0].kind, GapKind::Disconnect);
    assert_eq!(s.gaps[0].from_ns, 1000);
    assert_eq!(s.gaps[0].to_ns, 1000 + half);
    // coverage = 1 - half/day = 0.5, to 6 decimals.
    assert!((s.coverage - 0.5).abs() < 1e-6, "coverage={}", s.coverage);
}

#[test]
fn sto_2_manifest_sequence_gap_on_book_deltas() {
    let (_syms, btc) = table();
    // GapDetected then a GapResync snapshot closes it; applies to book_deltas.
    let events = vec![
        EventEnvelope::new(
            Venue::Bybit,
            btc,
            0,
            500,
            5,
            MarketEvent::BookDelta {
                bids: Default::default(),
                asks: Default::default(),
                first_seq: 5,
                last_seq: 5,
            },
        ),
        status(btc, 1000, StatusKind::GapDetected),
        EventEnvelope::new(
            Venue::Bybit,
            btc,
            0,
            1000 + DAY / 4,
            9,
            MarketEvent::BookSnapshot {
                bids: Default::default(),
                asks: Default::default(),
                seq: 9,
                depth: 0,
                reason: SnapshotReason::GapResync,
            },
        ),
    ];
    let m = derive_manifest(
        "bybit",
        "d",
        0,
        DAY,
        events.into_iter(),
        |_| "BTCUSDT".into(),
        "g",
        0,
    );
    let deltas = m.streams.get("book_deltas:BTCUSDT").expect("book_deltas");
    assert_eq!(deltas.gaps.len(), 1);
    assert_eq!(deltas.gaps[0].kind, GapKind::Venue);
    assert!(
        (deltas.coverage - 0.75).abs() < 1e-6,
        "coverage={}",
        deltas.coverage
    );
}

#[test]
fn sto_5_dataset_reads_coverage_from_manifest() {
    let root = tmp("sto5");
    let (syms, btc) = table();
    let half = DAY / 2;
    let events = vec![
        trade(btc, 100, 1, 100.0),
        status(btc, 1000, StatusKind::Disconnected),
        status(btc, 1000 + half, StatusKind::Connected),
    ];
    compact_day(
        &root,
        Venue::Bybit,
        "2026-07-10",
        0,
        DAY,
        events,
        &syms,
        "h",
        "g",
        0,
    )
    .unwrap();
    let ds = Dataset::open(&root);
    let cov = ds
        .coverage(Venue::Bybit, "2026-07-10", "trades:BTCUSDT")
        .unwrap();
    assert!(cov.is_some());
    assert!((cov.unwrap() - 0.5).abs() < 1e-6);
    let gaps = ds
        .gaps(Venue::Bybit, "2026-07-10", "trades:BTCUSDT")
        .unwrap();
    assert_eq!(gaps.len(), 1);
}

#[test]
fn sto_3_prune_refuses_on_row_mismatch() {
    let root = tmp("sto3");
    let (syms, btc) = table();
    let events = vec![trade(btc, 10, 1, 100.0), trade(btc, 20, 2, 101.0)];
    compact_day(
        &root,
        Venue::Bybit,
        "2026-07-10",
        0,
        DAY,
        events,
        &syms,
        "h",
        "g",
        0,
    )
    .unwrap();

    // Honest state: prune is safe.
    assert!(prune::verify_prunable(&root, Venue::Bybit, "2026-07-10").is_ok());

    // Corrupt the manifest to claim a different event count ⇒ prune must refuse.
    let mpath = mp_storage::layout::manifest_file(&root, Venue::Bybit, "2026-07-10");
    let mut m: mp_storage::QualityManifest =
        serde_json::from_slice(&std::fs::read(&mpath).unwrap()).unwrap();
    m.streams.get_mut("trades:BTCUSDT").unwrap().events = 999;
    std::fs::write(&mpath, serde_json::to_vec(&m).unwrap()).unwrap();

    let refusal = prune::verify_prunable(&root, Venue::Bybit, "2026-07-10");
    assert!(matches!(
        refusal,
        Err(prune::PruneRefusal::RowCountMismatch { .. })
    ));
}

#[test]
fn sto_9_scd2_as_of_resolves_across_change() {
    let mut scd = SymbolScd2::new();
    scd.append(SymbolVersion {
        venue: Venue::Bybit,
        venue_symbol: "BTCUSDT".into(),
        kind: InstrumentKind::Perp,
        tick_size: 0.5,
        step_size: 0.001,
        min_notional: 5.0,
        valid_from_ns: 0,
        valid_to_ns: 0, // open
    });
    // Tick size changes at t=1000.
    scd.append(SymbolVersion {
        venue: Venue::Bybit,
        venue_symbol: "BTCUSDT".into(),
        kind: InstrumentKind::Perp,
        tick_size: 0.1,
        step_size: 0.001,
        min_notional: 5.0,
        valid_from_ns: 1000,
        valid_to_ns: 0,
    });

    assert_eq!(
        scd.as_of(Venue::Bybit, "BTCUSDT", 500).unwrap().tick_size,
        0.5
    );
    assert_eq!(
        scd.as_of(Venue::Bybit, "BTCUSDT", 1500).unwrap().tick_size,
        0.1
    );
    assert_eq!(
        scd.as_of(Venue::Bybit, "BTCUSDT", 1000).unwrap().tick_size,
        0.1
    );
    assert!(scd.as_of(Venue::Bybit, "ETHUSDT", 500).is_none());
}

// ---- FEA-6: feature-store materialization + ver=N -------------------------

#[test]
fn fea_6_materialize_versions_on_params_change_never_overwrites() {
    use mp_storage::feature_store::{materialize, read_feature_meta, read_features};
    use mp_storage::{FeatureMeta, FeatureRow};

    let root_buf = tmp("featstore");
    let root = root_buf.as_path();
    let rows = vec![
        FeatureRow {
            symbol_id: 0,
            venue_code: 1,
            ts_ns: 10,
            value: 1.5,
            ver: 1,
        },
        FeatureRow {
            symbol_id: 0,
            venue_code: 1,
            ts_ns: 20,
            value: 2.5,
            ver: 1,
        },
    ];
    let meta_a = FeatureMeta {
        feature_ver: 1,
        engine_git_sha: "abc123".into(),
        params_hash: "aaaa0000".into(),
    };

    // First materialization → ver=0.
    let p0 = materialize(root, "cvd.bybit", "bybit", 0, "2026-07-11", &rows, &meta_a).unwrap();
    assert!(p0.to_string_lossy().contains("ver=0"));
    // Footer carries {feature ver, engine git sha, params hash} (FEA-6).
    let read_meta = read_feature_meta(&p0).unwrap().unwrap();
    assert_eq!(read_meta, meta_a);
    // Data round-trips through real Parquet.
    assert_eq!(read_features(&p0).unwrap(), rows);

    // Re-materializing the SAME params reuses ver=0 (idempotent).
    let p0b = materialize(root, "cvd.bybit", "bybit", 0, "2026-07-11", &rows, &meta_a).unwrap();
    assert_eq!(p0, p0b);

    // CHANGED params ⇒ a new ver=1 directory; the old ver=0 file is untouched.
    let meta_b = FeatureMeta {
        params_hash: "bbbb1111".into(),
        ..meta_a.clone()
    };
    let p1 = materialize(root, "cvd.bybit", "bybit", 0, "2026-07-11", &rows, &meta_b).unwrap();
    assert!(p1.to_string_lossy().contains("ver=1"));
    assert_ne!(p0, p1);
    // W-6: the original version's data and metadata are still there, unchanged.
    assert_eq!(
        read_feature_meta(&p0).unwrap().unwrap().params_hash,
        "aaaa0000"
    );
    assert_eq!(read_features(&p0).unwrap(), rows);
}
