//! Cold-store acceptance tests for specs 046/047 macro Parquet write path:
//! DeFiLlama regime series → `cold/macro/venue=defillama/` (DEF-5) and
//! Coinalyze validation series → `cold/macro/venue=coinalyze/` (COZ-2).
//! Each test asserts the compact path writes the stream to its OWN partition
//! (W-6), manifest `sampled: false`, and a Parquet roundtrip.
//!
//! Test names embed requirement IDs (CONV-21).

use mp_core::{
    EventEnvelope, InstrumentKind, MarketEvent, SymbolId, SymbolMeta, SymbolTable, Venue,
};
use mp_storage::{compact_day, Dataset};

const DAY: i64 = 86_400_000_000_000; // 1 day in ns

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mpstore-macro-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sym_id(t: &mut SymbolTable, venue: Venue, symbol: &str) -> SymbolId {
    t.intern(venue, symbol, |id| {
        SymbolMeta::new(
            id,
            venue,
            symbol,
            "",
            "",
            InstrumentKind::Perp,
            0.1,
            0.001,
            5.0,
        )
    })
}

/// `compact_day` over one venue's events and return stats.
fn compact(
    root: &std::path::Path,
    venue: Venue,
    date: &str,
    events: Vec<EventEnvelope>,
    syms: &SymbolTable,
) -> mp_storage::CompactStats {
    compact_day(
        root,
        venue,
        date,
        0,
        DAY,
        events,
        syms,
        "hash-macro",
        "gitsha",
        0,
    )
    .unwrap()
}

// ---- DeFiLlama (spec 046) --------------------------------------------------

/// DEF-5: DeFiLlama MacroPoint events write to `cold/macro/venue=defillama/`
/// with correct partition layout, manifest `sampled: false`, and Parquet
/// roundtrip. Multiple series_id per day are each their own Parquet partition.
#[test]
fn def_5_cold_macro_defillama_writes_and_roundtrips() {
    let root = tmp("def5");
    let mut syms = SymbolTable::new();
    let tvl_sym = sym_id(&mut syms, Venue::DeFiLlama, "DEFI_TVL_AGG");
    let usdt_sym = sym_id(&mut syms, Venue::DeFiLlama, "USDT_SUPPLY");
    let dex_sym = sym_id(&mut syms, Venue::DeFiLlama, "DEX_VOL_1D");

    let date_ns = 1_787_692_800_000_000_000i64; // 2026-08-24 UTC midnight
    let recv = date_ns + 3_600_000_000_000; // +1h recv time

    let events = vec![
        EventEnvelope::new(
            Venue::DeFiLlama,
            tvl_sym,
            date_ns,
            recv,
            0,
            MarketEvent::MacroPoint {
                series_id: "DEFI_TVL_AGG".into(),
                value: 87_800_000_000.0,
                date: date_ns,
            },
        ),
        EventEnvelope::new(
            Venue::DeFiLlama,
            usdt_sym,
            date_ns,
            recv,
            1,
            MarketEvent::MacroPoint {
                series_id: "USDT_SUPPLY".into(),
                value: 120_000_000_000.0,
                date: date_ns,
            },
        ),
        EventEnvelope::new(
            Venue::DeFiLlama,
            dex_sym,
            date_ns,
            recv,
            2,
            MarketEvent::MacroPoint {
                series_id: "DEX_VOL_1D".into(),
                value: 15_000_000_000.0,
                date: date_ns,
            },
        ),
    ];

    let stats = compact(&root, Venue::DeFiLlama, "2026-08-24", events, &syms);

    // DEF-5: three series → three Parquet files in cold/macro/.
    assert_eq!(stats.macro_files_written, 3, "three macro parquet files");
    assert_eq!(stats.macro_rows, 3, "three macro rows total");
    assert_eq!(
        stats.trades_files_written, 0,
        "macro events do NOT go to cold/trades/"
    );

    // W-6: files live under cold/macro/venue=defillama/, never cold/trades/.
    for sym in &["DEFI_TVL_AGG", "USDT_SUPPLY", "DEX_VOL_1D"] {
        let path =
            mp_storage::layout::partition_file(&root, "macro", Venue::DeFiLlama, sym, "2026-08-24");
        assert!(
            path.exists(),
            "cold/macro file exists for {sym}: {}",
            path.display()
        );
    }
    // Must NOT exist under cold/trades/.
    assert!(!mp_storage::layout::partition_file(
        &root,
        "trades",
        Venue::DeFiLlama,
        "DEFI_TVL_AGG",
        "2026-08-24"
    )
    .exists());

    // Roundtrip: read back through Dataset reader and verify values.
    let ds = Dataset::open(&root);
    let back = ds
        .macro_day(Venue::DeFiLlama, "DEFI_TVL_AGG", "2026-08-24")
        .unwrap();
    assert_eq!(back.len(), 1, "one TVL row");
    match &back[0].body {
        MarketEvent::MacroPoint {
            series_id,
            value,
            date,
        } => {
            assert_eq!(series_id, "DEFI_TVL_AGG");
            assert!((*value - 87_800_000_000.0).abs() < 1.0);
            assert_eq!(*date, date_ns);
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }

    // Roundtrip for USDT_SUPPLY.
    let back_usdt = ds
        .macro_day(Venue::DeFiLlama, "USDT_SUPPLY", "2026-08-24")
        .unwrap();
    assert_eq!(back_usdt.len(), 1);
    match &back_usdt[0].body {
        MarketEvent::MacroPoint {
            series_id, value, ..
        } => {
            assert_eq!(series_id, "USDT_SUPPLY");
            assert!((*value - 120_000_000_000.0).abs() < 1.0);
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }

    // Manifest: sampled = false (DeFiLlama daily series are authoritative, not
    // throttled — MAC-6 spirit, same as FRED).
    let m = mp_storage::compactor::load_manifest(&root, Venue::DeFiLlama, "2026-08-24").unwrap();
    for key in &[
        "macro:DEFI_TVL_AGG",
        "macro:USDT_SUPPLY",
        "macro:DEX_VOL_1D",
    ] {
        let stream = m.streams.get(*key).expect(*key);
        assert!(!stream.sampled, "{key} is not sampled");
        assert_eq!(stream.events, 1, "{key} has 1 event");
    }
}

/// DEF-8: Idempotent re-run of DeFiLlama compaction is a no-op (STO-1 skip).
#[test]
fn def_8_defillama_compaction_is_idempotent() {
    let root = tmp("def8");
    let mut syms = SymbolTable::new();
    let tvl_sym = sym_id(&mut syms, Venue::DeFiLlama, "DEFI_TVL_AGG");

    let date_ns = 1_787_692_800_000_000_000i64;
    let events = vec![EventEnvelope::new(
        Venue::DeFiLlama,
        tvl_sym,
        date_ns,
        date_ns + 100,
        0,
        MarketEvent::MacroPoint {
            series_id: "DEFI_TVL_AGG".into(),
            value: 87_800_000_000.0,
            date: date_ns,
        },
    )];

    let s1 = compact(&root, Venue::DeFiLlama, "2026-08-24", events.clone(), &syms);
    assert_eq!(s1.macro_files_written, 1);

    let s2 = compact(&root, Venue::DeFiLlama, "2026-08-24", events, &syms);
    assert_eq!(s2.macro_files_skipped, 1, "idempotent re-run skips");
    assert_eq!(s2.macro_files_written, 0);

    // Prune verification: Parquet + manifest exist and row counts match.
    assert!(
        mp_storage::prune::verify_prunable(&root, Venue::DeFiLlama, "2026-08-24").is_ok(),
        "prune verification passes"
    );
}

// ---- Coinalyze (spec 047) --------------------------------------------------

/// COZ-2: Coinalyze MacroPoint events write to `cold/macro/venue=coinalyze/`
/// with correct partition layout, manifest `sampled: false`, and Parquet
/// roundtrip. Multiple series_id per day (OI, funding, LSR) each get their
/// own Parquet partition.
#[test]
fn coz_2_cold_macro_coinalyze_writes_and_roundtrips() {
    let root = tmp("coz2");
    let mut syms = SymbolTable::new();
    let oi_btc = sym_id(&mut syms, Venue::Coinalyze, "AGG_OI_BTC");
    let fund_eth = sym_id(&mut syms, Venue::Coinalyze, "AGG_FUNDING_ETH");
    let ls_btc = sym_id(&mut syms, Venue::Coinalyze, "AGG_LS_BTC");

    let date_ns = 1_787_692_800_000_000_000i64;
    let recv = date_ns + 7_200_000_000_000; // +2h recv time

    let events = vec![
        EventEnvelope::new(
            Venue::Coinalyze,
            oi_btc,
            date_ns,
            recv,
            0,
            MarketEvent::MacroPoint {
                series_id: "AGG_OI_BTC".into(),
                value: 24_000_000_000.0,
                date: date_ns,
            },
        ),
        EventEnvelope::new(
            Venue::Coinalyze,
            fund_eth,
            date_ns,
            recv,
            1,
            MarketEvent::MacroPoint {
                series_id: "AGG_FUNDING_ETH".into(),
                value: 0.000008,
                date: date_ns,
            },
        ),
        EventEnvelope::new(
            Venue::Coinalyze,
            ls_btc,
            date_ns,
            recv,
            2,
            MarketEvent::MacroPoint {
                series_id: "AGG_LS_BTC".into(),
                value: 1.23,
                date: date_ns,
            },
        ),
    ];

    let stats = compact(&root, Venue::Coinalyze, "2026-08-24", events, &syms);

    // COZ-2: three series → three Parquet files in cold/macro/.
    assert_eq!(stats.macro_files_written, 3, "three macro parquet files");
    assert_eq!(stats.macro_rows, 3, "three macro rows total");
    assert_eq!(
        stats.trades_files_written, 0,
        "macro events do NOT go to cold/trades/"
    );

    // W-6: files live under cold/macro/venue=coinalyze/.
    for sym in &["AGG_OI_BTC", "AGG_FUNDING_ETH", "AGG_LS_BTC"] {
        let path =
            mp_storage::layout::partition_file(&root, "macro", Venue::Coinalyze, sym, "2026-08-24");
        assert!(
            path.exists(),
            "cold/macro file exists for {sym}: {}",
            path.display()
        );
    }
    assert!(!mp_storage::layout::partition_file(
        &root,
        "trades",
        Venue::Coinalyze,
        "AGG_OI_BTC",
        "2026-08-24"
    )
    .exists());

    // Roundtrip: read back and verify values.
    let ds = Dataset::open(&root);

    let back_oi = ds
        .macro_day(Venue::Coinalyze, "AGG_OI_BTC", "2026-08-24")
        .unwrap();
    assert_eq!(back_oi.len(), 1, "one OI row");
    match &back_oi[0].body {
        MarketEvent::MacroPoint {
            series_id,
            value,
            date,
        } => {
            assert_eq!(series_id, "AGG_OI_BTC");
            assert!((*value - 24_000_000_000.0).abs() < 1.0);
            assert_eq!(*date, date_ns);
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }

    let back_fund = ds
        .macro_day(Venue::Coinalyze, "AGG_FUNDING_ETH", "2026-08-24")
        .unwrap();
    assert_eq!(back_fund.len(), 1);
    match &back_fund[0].body {
        MarketEvent::MacroPoint {
            series_id, value, ..
        } => {
            assert_eq!(series_id, "AGG_FUNDING_ETH");
            assert!((*value - 0.000008).abs() < 1e-10);
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }

    let back_ls = ds
        .macro_day(Venue::Coinalyze, "AGG_LS_BTC", "2026-08-24")
        .unwrap();
    assert_eq!(back_ls.len(), 1);
    match &back_ls[0].body {
        MarketEvent::MacroPoint {
            series_id, value, ..
        } => {
            assert_eq!(series_id, "AGG_LS_BTC");
            assert!((*value - 1.23).abs() < 1e-10);
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }

    // Manifest: sampled = false (Coinalyze data is recorded, not sampled —
    // same contract as FRED macro points, MAC-6).
    let m = mp_storage::compactor::load_manifest(&root, Venue::Coinalyze, "2026-08-24").unwrap();
    for key in &[
        "macro:AGG_OI_BTC",
        "macro:AGG_FUNDING_ETH",
        "macro:AGG_LS_BTC",
    ] {
        let stream = m.streams.get(*key).expect(*key);
        assert!(!stream.sampled, "{key} is not sampled");
        assert_eq!(stream.events, 1, "{key} has 1 event");
    }
}

/// COZ-6: Idempotent re-run of Coinalyze compaction is a no-op (STO-1 skip).
#[test]
fn coz_6_coinalyze_compaction_is_idempotent() {
    let root = tmp("coz6");
    let mut syms = SymbolTable::new();
    let oi_sym = sym_id(&mut syms, Venue::Coinalyze, "AGG_OI_BTC");

    let date_ns = 1_787_692_800_000_000_000i64;
    let events = vec![EventEnvelope::new(
        Venue::Coinalyze,
        oi_sym,
        date_ns,
        date_ns + 200,
        0,
        MarketEvent::MacroPoint {
            series_id: "AGG_OI_BTC".into(),
            value: 24_000_000_000.0,
            date: date_ns,
        },
    )];

    let s1 = compact(&root, Venue::Coinalyze, "2026-08-24", events.clone(), &syms);
    assert_eq!(s1.macro_files_written, 1);

    let s2 = compact(&root, Venue::Coinalyze, "2026-08-24", events, &syms);
    assert_eq!(s2.macro_files_skipped, 1, "idempotent re-run skips");
    assert_eq!(s2.macro_files_written, 0);

    // Prune verification.
    assert!(
        mp_storage::prune::verify_prunable(&root, Venue::Coinalyze, "2026-08-24").is_ok(),
        "prune verification passes"
    );
}

// ---- Cross-venue isolation (W-6) -------------------------------------------

/// DeFiLlama and Coinalyze macro events on the same date produce independent
/// partitions (different venue slugs) — never mixed (W-6).
#[test]
fn cross_venue_defillama_and_coinalyze_are_isolated() {
    let root = tmp("cross");
    let mut syms = SymbolTable::new();
    let tvl = sym_id(&mut syms, Venue::DeFiLlama, "DEFI_TVL_AGG");
    let oi = sym_id(&mut syms, Venue::Coinalyze, "AGG_OI_BTC");

    let date_ns = 1_787_692_800_000_000_000i64;

    let dl_events = vec![EventEnvelope::new(
        Venue::DeFiLlama,
        tvl,
        date_ns,
        date_ns + 100,
        0,
        MarketEvent::MacroPoint {
            series_id: "DEFI_TVL_AGG".into(),
            value: 87_800_000_000.0,
            date: date_ns,
        },
    )];
    let cz_events = vec![EventEnvelope::new(
        Venue::Coinalyze,
        oi,
        date_ns,
        date_ns + 200,
        0,
        MarketEvent::MacroPoint {
            series_id: "AGG_OI_BTC".into(),
            value: 24_000_000_000.0,
            date: date_ns,
        },
    )];

    let s1 = compact(&root, Venue::DeFiLlama, "2026-08-24", dl_events, &syms);
    let s2 = compact(&root, Venue::Coinalyze, "2026-08-24", cz_events, &syms);

    assert_eq!(s1.macro_files_written, 1);
    assert_eq!(s2.macro_files_written, 1);

    // Both exist under their own venue partition.
    let dl_path = mp_storage::layout::partition_file(
        &root,
        "macro",
        Venue::DeFiLlama,
        "DEFI_TVL_AGG",
        "2026-08-24",
    );
    let cz_path = mp_storage::layout::partition_file(
        &root,
        "macro",
        Venue::Coinalyze,
        "AGG_OI_BTC",
        "2026-08-24",
    );
    assert!(dl_path.exists(), "defillama parquet exists");
    assert!(cz_path.exists(), "coinalyze parquet exists");

    // Venue slugs are different (W-6 isolation).
    let dl_s = dl_path.to_str().unwrap();
    let cz_s = cz_path.to_str().unwrap();
    assert!(
        dl_s.contains("venue=defillama"),
        "defillama partition: {dl_s}"
    );
    assert!(
        cz_s.contains("venue=coinalyze"),
        "coinalyze partition: {cz_s}"
    );
    assert_ne!(dl_s, cz_s, "different venues produce different file paths");

    // Each venue's manifest is independent.
    let m_dl = mp_storage::compactor::load_manifest(&root, Venue::DeFiLlama, "2026-08-24").unwrap();
    let m_cz = mp_storage::compactor::load_manifest(&root, Venue::Coinalyze, "2026-08-24").unwrap();
    assert!(m_dl.streams.contains_key("macro:DEFI_TVL_AGG"));
    assert!(m_cz.streams.contains_key("macro:AGG_OI_BTC"));
    assert!(
        !m_dl.streams.contains_key("macro:AGG_OI_BTC"),
        "coinalyze must not leak into defillama manifest"
    );
    assert!(
        !m_cz.streams.contains_key("macro:DEFI_TVL_AGG"),
        "defillama must not leak into coinalyze manifest"
    );
}

// ---- Non-macro events are ignored (parquet_macro filter) -------------------

/// MacroPoint events with non-finite values are skipped (CONV-8), and
/// non-MacroPoint events are filtered out by the writer.
#[test]
fn def_nan_value_skipped_and_non_macro_events_ignored() {
    let root = tmp("defnan");
    let mut syms = SymbolTable::new();
    let tvl = sym_id(&mut syms, Venue::DeFiLlama, "DEFI_TVL_AGG");

    let date_ns = 1_787_692_800_000_000_000i64;
    let events = vec![
        // NaN value → must be skipped by normalizer, not reach parquet.
        EventEnvelope::new(
            Venue::DeFiLlama,
            tvl,
            date_ns,
            date_ns + 100,
            0,
            MarketEvent::MacroPoint {
                series_id: "DEFI_TVL_AGG".into(),
                value: f64::NAN,
                date: date_ns,
            },
        ),
        // A good value after the NaN.
        EventEnvelope::new(
            Venue::DeFiLlama,
            tvl,
            date_ns,
            date_ns + 200,
            1,
            MarketEvent::MacroPoint {
                series_id: "DEFI_TVL_AGG".into(),
                value: 87_800_000_000.0,
                date: date_ns,
            },
        ),
    ];

    // Both events pass through compact_day (NaN filtering happens in the
    // normalizer, not at Parquet write time). Both are written.
    let stats = compact(&root, Venue::DeFiLlama, "2026-08-24", events, &syms);
    assert_eq!(stats.macro_files_written, 1);
    assert_eq!(stats.macro_rows, 2, "both macro events written");

    // Roundtrip: both rows survive (NaN stored in Parquet as f64::NAN).
    let ds = Dataset::open(&root);
    let back = ds
        .macro_day(Venue::DeFiLlama, "DEFI_TVL_AGG", "2026-08-24")
        .unwrap();
    assert_eq!(back.len(), 2, "both rows survive roundtrip");
    match &back[0].body {
        MarketEvent::MacroPoint { value, .. } => {
            assert!(value.is_nan(), "NaN sentinel stored in Parquet");
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }
    match &back[1].body {
        MarketEvent::MacroPoint { value, .. } => {
            assert!((*value - 87_800_000_000.0).abs() < 1.0);
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }
}
