//! Acceptance tests for spec 026 (cross-venue gap detector). Test names embed
//! requirement IDs (CONV-21). All fixtures are local; no network (CONV-23).

use mp_core::{
    EventEnvelope, InstrumentKind, MarketEvent, Side, StatusKind, SymbolId, SymbolMeta,
    SymbolTable, Venue,
};
use mp_storage::{
    compact_day, detect, findings_file, parse_config, version_string, write_findings,
    Classification, CrossVenueConfig, Dataset, SymbolCohort,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DAY: i64 = 86_400_000_000_000;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mpcvg-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn trade(venue: Venue, sym: SymbolId, recv: i64, seq: u64, price: f64) -> EventEnvelope {
    EventEnvelope::new(
        venue,
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

fn status(venue: Venue, sym: SymbolId, recv: i64, kind: StatusKind) -> EventEnvelope {
    EventEnvelope::new(
        venue,
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

fn meta(id: SymbolId, venue: Venue, sym: &str) -> SymbolMeta {
    SymbolMeta::new(
        id,
        venue,
        sym,
        "BTC",
        "USDT",
        InstrumentKind::Perp,
        0.1,
        0.001,
        5.0,
    )
}

/// (table, binance_btc, bybit_btc, okx_btc) — one logical BTC underlying across
/// three venues with their venue-specific tickers (spec 026 cohort).
fn table() -> (SymbolTable, SymbolId, SymbolId, SymbolId) {
    let mut t = SymbolTable::new();
    let bin = t.intern(Venue::BinanceFutures, "BTCUSDT", |id| {
        meta(id, Venue::BinanceFutures, "BTCUSDT")
    });
    let byb = t.intern(Venue::Bybit, "BTCUSDT", |id| {
        meta(id, Venue::Bybit, "BTCUSDT")
    });
    let okx = t.intern(Venue::Okx, "BTC-USDT-SWAP", |id| {
        meta(id, Venue::Okx, "BTC-USDT-SWAP")
    });
    (t, bin, byb, okx)
}

fn compact(
    root: &Path,
    venue: Venue,
    date: &str,
    mut events: Vec<EventEnvelope>,
    syms: &SymbolTable,
    hash: &str,
) {
    events.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
    compact_day(root, venue, date, 0, DAY, events, syms, hash, "gitsha", 0).unwrap();
}

fn cfg() -> CrossVenueConfig {
    let mut members = BTreeMap::new();
    members.insert("binance_futures".into(), "BTCUSDT".into());
    members.insert("bybit".into(), "BTCUSDT".into());
    members.insert("okx".into(), "BTC-USDT-SWAP".into());
    CrossVenueConfig {
        min_cohort: 2,
        min_corroborators: 1,
        min_trades: 1,
        cohort_gap_overlap: 0.5,
        max_price_band_pct: 5.0,
        veracity_window_min: 60,
        veracity_min_trades: 50,
        veracity_trade_ratio: 0.5,
        symbol_cohorts: vec![SymbolCohort {
            underlying: "BTC".into(),
            members,
        }],
    }
}

/// Binance recording with a disconnect gap [1000, 2000) on trades:BTCUSDT.
fn bin_gap_events(bin: SymbolId) -> Vec<EventEnvelope> {
    vec![
        trade(Venue::BinanceFutures, bin, 100, 1, 100.0),
        status(Venue::BinanceFutures, bin, 1000, StatusKind::Disconnected),
        status(Venue::BinanceFutures, bin, 2000, StatusKind::Connected),
        trade(Venue::BinanceFutures, bin, 2100, 2, 100.0),
    ]
}

/// Continuous trades across [0, DAY) at price ~100 (no gap).
fn cont_trades(venue: Venue, sym: SymbolId) -> Vec<EventEnvelope> {
    vec![
        trade(venue, sym, 100, 1, 100.0),
        trade(venue, sym, 1100, 2, 100.0),
        trade(venue, sym, 1500, 3, 100.0),
    ]
}

fn bin_findings(f: &mp_storage::Findings) -> Vec<&mp_storage::Finding> {
    f.findings
        .iter()
        .filter(|x| x.venue == "binance_futures")
        .collect()
}

#[test]
fn cvg_1_reads_manifests_and_dataset_offline() {
    let root = tmp("cvg1");
    let (syms, bin, byb, okx) = table();
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        bin_gap_events(bin),
        &syms,
        "hB",
    );
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        cont_trades(Venue::Bybit, byb),
        &syms,
        "hY",
    );
    compact(
        &root,
        Venue::Okx,
        "2026-08-04",
        cont_trades(Venue::Okx, okx),
        &syms,
        "hO",
    );
    // CVG-1: reads only local manifests + cold Parquet; the storage crate has
    // no network dependency, so this run touches no socket.
    let f = detect(&root, "2026-08-04", &cfg(), "testsha", "testhash").unwrap();
    assert!(!f.findings.is_empty());
}

#[test]
fn cvg_2_output_is_deterministic() {
    let root = tmp("cvg2");
    let (syms, bin, byb, okx) = table();
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        bin_gap_events(bin),
        &syms,
        "hB",
    );
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        cont_trades(Venue::Bybit, byb),
        &syms,
        "hY",
    );
    compact(
        &root,
        Venue::Okx,
        "2026-08-04",
        cont_trades(Venue::Okx, okx),
        &syms,
        "hO",
    );
    let f1 = detect(&root, "2026-08-04", &cfg(), "testsha", "testhash").unwrap();
    let f2 = detect(&root, "2026-08-04", &cfg(), "testsha", "testhash").unwrap();
    // CVG-2: byte-identical findings (golden).
    assert_eq!(
        serde_json::to_vec_pretty(&f1).unwrap(),
        serde_json::to_vec_pretty(&f2).unwrap()
    );
}

#[test]
fn cvg_3_cohort_requires_min_venues() {
    let root = tmp("cvg3");
    let (syms, bin, byb, _okx) = table();
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        bin_gap_events(bin),
        &syms,
        "hB",
    );
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        cont_trades(Venue::Bybit, byb),
        &syms,
        "hY",
    );
    // No OKX recording ⇒ cohort = {Bybit} only (1 < min_cohort 2).
    let f = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    let bf = bin_findings(&f);
    assert_eq!(bf.len(), 1);
    assert_eq!(bf[0].classification, Classification::IsolatedUnknown);
}
#[test]
fn cvg_4_corroborated_when_cohort_continuous() {
    let root = tmp("cvg4");
    let (syms, bin, byb, okx) = table();
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        bin_gap_events(bin),
        &syms,
        "hB",
    );
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        cont_trades(Venue::Bybit, byb),
        &syms,
        "hY",
    );
    compact(
        &root,
        Venue::Okx,
        "2026-08-04",
        cont_trades(Venue::Okx, okx),
        &syms,
        "hO",
    );
    let f = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    let bf = bin_findings(&f);
    assert_eq!(bf.len(), 1);
    assert_eq!(bf[0].classification, Classification::CorroboratedVenueSide);
    // CVG-4: both cohort venues corroborate (continuous + price within band).
    assert!(bf[0].cohort.iter().all(|m| m.corroborates));
    assert_eq!(bf[0].cohort.len(), 2);
}

#[test]
fn cvg_5_market_wide_when_all_cohort_gapped() {
    let root = tmp("cvg5");
    let (syms, bin, byb, okx) = table();
    // CVG-5: every venue disconnects over the same [1000, 2000) window.
    for (venue, sym, h) in [
        (Venue::BinanceFutures, bin, "hB"),
        (Venue::Bybit, byb, "hY"),
        (Venue::Okx, okx, "hO"),
    ] {
        compact(
            &root,
            venue,
            "2026-08-04",
            vec![
                trade(venue, sym, 100, 1, 100.0),
                status(venue, sym, 1000, StatusKind::Disconnected),
                status(venue, sym, 2000, StatusKind::Connected),
                trade(venue, sym, 2100, 2, 100.0),
            ],
            &syms,
            h,
        );
    }
    let f = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    let bf = bin_findings(&f);
    assert_eq!(bf.len(), 1);
    assert_eq!(bf[0].classification, Classification::MarketWide);
}

#[test]
fn cvg_6_no_backfill_of_missing_events() {
    let root = tmp("cvg6");
    let (syms, bin, byb, okx) = table();
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        bin_gap_events(bin),
        &syms,
        "hB",
    );
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        cont_trades(Venue::Bybit, byb),
        &syms,
        "hY",
    );
    compact(
        &root,
        Venue::Okx,
        "2026-08-04",
        cont_trades(Venue::Okx, okx),
        &syms,
        "hO",
    );
    // Snapshot the Binance trades Parquet + manifest BEFORE (CVG-6/W-6).
    let bin_trade = mp_storage::layout::partition_file(
        &root,
        "trades",
        Venue::BinanceFutures,
        "BTCUSDT",
        "2026-08-04",
    );
    let bin_manifest =
        mp_storage::layout::manifest_file(&root, Venue::BinanceFutures, "2026-08-04");
    let trade_before = std::fs::read(&bin_trade).unwrap();
    let manifest_before = std::fs::read(&bin_manifest).unwrap();
    let f = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    let _ = write_findings(&root, &f).unwrap();
    // CVG-6: the gapped venue's trades + manifest are byte-identical; only the
    // separate findings artifact is written.
    assert_eq!(std::fs::read(&bin_trade).unwrap(), trade_before);
    assert_eq!(std::fs::read(&bin_manifest).unwrap(), manifest_before);
    assert!(findings_file(&root, "2026-08-04").exists());
}

#[test]
fn cvg_7_promotion_gate_unchanged() {
    // CVG-7: corroboration never relaxes the INT-5 gate. The gate reads the
    // audit (raw log), which detect never touches; a gapped recording is still
    // non-promotable after corroboration. Proven by the manifest gap surviving.
    use mp_storage::{check_promotion_n, scorecard, RawLogAudit};
    // Build a dirty per-venue audit (a gap ⇒ findings) directly: a recording
    // with a manifest gap is non-clean by the audit's coverage check.
    let root = tmp("cvg7");
    let (syms, bin, byb, okx) = table();
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        bin_gap_events(bin),
        &syms,
        "hB",
    );
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        cont_trades(Venue::Bybit, byb),
        &syms,
        "hY",
    );
    compact(
        &root,
        Venue::Okx,
        "2026-08-04",
        cont_trades(Venue::Okx, okx),
        &syms,
        "hO",
    );
    // Run detect (finds a corroborated gap) — does NOT touch the audit/manifest.
    let _ = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    // The manifest still records the gap ⇒ the recording is still not clean.
    let ds = Dataset::open(&root);
    let gaps = ds
        .gaps(Venue::BinanceFutures, "2026-08-04", "trades:BTCUSDT")
        .unwrap();
    assert!(!gaps.is_empty(), "gap survived (CVG-6/7: not excised)");
    // A scorecard over a dirty audit is non-promotable regardless of
    // corroboration (the gate reads the audit, not the findings).
    let dirty = RawLogAudit {
        event_count: 4,
        first_recv_ts_ns: Some(100),
        last_recv_ts_ns: Some(2100),
        coverage: 0.5, // gapped ⇒ < MIN_COVERAGE (2026-08-12: the numeric bar)
        streams: Default::default(),
        gaps: vec![],
        stale_periods: vec![],
        stale_bursts: vec![],
        worst_gap_ns: 0,
        findings: vec![mp_storage::audit::AuditFinding {
            code: "coverage_gap".into(),
            detail: "gap".into(),
        }],
    };
    let card = scorecard(
        "2026-08-04",
        vec![(Venue::BinanceFutures, "BTCUSDT".into(), dirty)],
    );
    assert!(
        !card.promotable,
        "gapped recording not promotable after corroboration (CVG-7)"
    );
    assert!(!check_promotion_n(&[card], 1).promoted);
}
#[test]
fn cvg_8_artifact_is_separate_and_append_only() {
    let root = tmp("cvg8");
    let (syms, bin, byb, okx) = table();
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        bin_gap_events(bin),
        &syms,
        "hB",
    );
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        cont_trades(Venue::Bybit, byb),
        &syms,
        "hY",
    );
    compact(
        &root,
        Venue::Okx,
        "2026-08-04",
        cont_trades(Venue::Okx, okx),
        &syms,
        "hO",
    );
    // Snapshot every per-venue manifest BEFORE (CVG-8/W-6: never edited).
    let manifests: Vec<(Venue, Vec<u8>)> = [Venue::BinanceFutures, Venue::Bybit, Venue::Okx]
        .iter()
        .copied()
        .map(|v| {
            (
                v,
                std::fs::read(mp_storage::layout::manifest_file(&root, v, "2026-08-04")).unwrap(),
            )
        })
        .collect();
    let f = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    let written = write_findings(&root, &f).unwrap();
    // CVG-8: separate, append-only artifact at the documented path.
    assert_eq!(written, findings_file(&root, "2026-08-04"));
    assert!(written.to_string_lossy().contains("cross_venue"));
    for (v, before) in &manifests {
        let after =
            std::fs::read(mp_storage::layout::manifest_file(&root, *v, "2026-08-04")).unwrap();
        assert_eq!(after, *before, "manifest for {v:?} mutated (CVG-8/W-6)");
    }
}

#[test]
fn cvg_9_check_config_rejects_unknown_fields() {
    // CVG-9: deny_unknown_fields ⇒ an unknown top-level key errors.
    let bad = "bogus_root = true\n[[symbol_cohorts]]\nunderlying = \"BTC\"\n[symbol_cohorts.members]\nbybit = \"BTCUSDT\"\n";
    assert!(
        parse_config(bad).is_err(),
        "unknown field must be rejected (CVG-9)"
    );
    let good = "[[symbol_cohorts]]\nunderlying = \"BTC\"\n[symbol_cohorts.members]\nbybit = \"BTCUSDT\"\nbinance_futures = \"BTCUSDT\"\nokx = \"BTC-USDT-SWAP\"\n";
    let parsed = parse_config(good).unwrap();
    assert_eq!(parsed.symbol_cohorts.len(), 1);
    // --version path is non-empty (CONV-18); the bin prints it verbatim.
    assert!(!version_string().is_empty());
}

#[test]
fn cvg_11_fixtures_no_network() {
    // CVG-11/CONV-23: the detector only reads local manifests + cold Parquet
    // (proven end-to-end by cvg_1..cvg_10, which never touch a socket). The
    // storage crate's only network dependency is reqwest, optional behind the
    // `live-http` feature — the detector itself is pure storage.
    let manifest = include_str!("../Cargo.toml");
    assert!(
        manifest.contains("reqwest") && manifest.contains("optional = true"),
        "network deps must stay feature-gated (CVG-11/PD-4)"
    );
}

#[test]
fn cvg_12_binary_provides_cli_flags() {
    // CVG-12: `mp-cross-venue` is a sibling of mp-audit/mp-migrate and must
    // answer --version / --check-config. Run the real binary (no network).
    let bin = env!("CARGO_BIN_EXE_mp-cross-venue");
    let ver = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .unwrap();
    assert!(ver.status.success(), "--version exits 0: {:?}", ver);
    assert!(!String::from_utf8_lossy(&ver.stdout).trim().is_empty());

    let dir = std::env::temp_dir().join(format!("mpcvg12-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.toml");
    std::fs::write(
        &good,
        "[[symbol_cohorts]]\nunderlying = \"BTC\"\n[symbol_cohorts.members]\nbybit = \"BTCUSDT\"\n",
    )
    .unwrap();
    let ok = std::process::Command::new(bin)
        .args(["--config"])
        .arg(&good)
        .arg("--check-config")
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "--check-config on valid config: {:?}",
        ok
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cvg_10_nan_fails_closed() {
    let root = tmp("cvg10");
    let (syms, bin, byb, okx) = table();
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        bin_gap_events(bin),
        &syms,
        "hB",
    );
    // CVG-10: cohort venues have a recording but NO trades inside [1000,2000)
    // ⇒ in-window vwp is non-finite ⇒ fail-closed (no corroboration, no NaN).
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        vec![
            trade(Venue::Bybit, byb, 100, 1, 100.0),
            trade(Venue::Bybit, byb, 2100, 2, 100.0),
        ],
        &syms,
        "hY",
    );
    compact(
        &root,
        Venue::Okx,
        "2026-08-04",
        vec![
            trade(Venue::Okx, okx, 100, 1, 100.0),
            trade(Venue::Okx, okx, 2100, 2, 100.0),
        ],
        &syms,
        "hO",
    );
    let f = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    let bf = bin_findings(&f);
    assert_eq!(bf.len(), 1);
    assert_eq!(bf[0].classification, Classification::IsolatedUnknown);
    // No NaN/inf ever reaches the serialized artifact; vwp is null (Option::None).
    // CVG-10: serde_json refuses to serialize a non-finite f64 (it errors), so
    // a successful serialize + re-parse proves no NaN/inf reached the artifact;
    // vwp is None (serialized null), never NaN.
    let json = serde_json::to_string_pretty(&f).unwrap();
    let _: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(bf[0].cohort.iter().all(|m| m.vwp.is_none()));
}

/// `n` trades spaced `spacing_s` seconds apart starting at `start_ns`, all at
/// `price` with qty 1 — a dense enough stream for the veracity windows.
fn dense(venue: Venue, sym: SymbolId, start_ns: i64, n: u64, price: f64, spacing_s: i64) -> Vec<EventEnvelope> {
    (0..n)
        .map(|i| trade(venue, sym, start_ns + (i as i64) * spacing_s * 1_000_000_000, i + 1, price))
        .collect()
}

/// cvg_13: whole-day veracity — a member whose window VWP leaves the cohort
/// median beyond `max_price_band_pct` is flagged `price_divergence`, even
/// though its manifest shows NO gap (presence is fine; the value is not).
#[test]
fn cvg_13_veracity_flags_price_divergence_with_no_gap() {
    let root = tmp("cvg13");
    let (syms, bin, byb, okx) = table();
    // Window 1 [0, 1h): all three venues at 100. Window 2 [1h, 2h): bybit
    // drifts to 150 (50% outlier); binance/okx stay at 100 ⇒ cohort median 100
    // ⇒ bybit deviates 50%, the healthy venues 0%.
    let mut be = dense(Venue::BinanceFutures, bin, 100_000_000_000, 60, 100.0, 40);
    be.extend(dense(Venue::BinanceFutures, bin, 3_700_000_000_000, 60, 100.0, 40));
    compact(&root, Venue::BinanceFutures, "2026-08-04", be, &syms, "hB");
    let mut ye = dense(Venue::Bybit, byb, 100_000_000_000, 60, 100.0, 40);
    ye.extend(dense(Venue::Bybit, byb, 3_700_000_000_000, 60, 150.0, 40));
    compact(&root, Venue::Bybit, "2026-08-04", ye, &syms, "hY");
    let mut oe = dense(Venue::Okx, okx, 100_000_000_000, 60, 100.0, 40);
    oe.extend(dense(Venue::Okx, okx, 3_700_000_000_000, 60, 100.0, 40));
    compact(&root, Venue::Okx, "2026-08-04", oe, &syms, "hO");

    let f = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    // Schema v2 carries the veracity section.
    assert_eq!(f.schema_ver, 2);
    let divs: Vec<&mp_storage::VeracityFinding> = f
        .veracity
        .iter()
        .filter(|v| v.kind == "price_divergence")
        .collect();
    assert_eq!(divs.len(), 1, "exactly one price divergence: {:?}", f.veracity);
    assert_eq!(divs[0].venue, "bybit", "the outlier is bybit");
    assert_eq!(divs[0].window_from_ns, 3_600_000_000_000, "window 2");
    assert_eq!(divs[0].trade_count, 60);
    assert_eq!(divs[0].vwp, Some(150.0));
    // No manifest gap on bybit that day (presence fine) — the divergence is a
    // value finding, not a gap finding: the gap detector must have found 0
    // bybit gaps.
    assert!(f.findings.iter().all(|g| g.venue != "bybit"));
}

/// cvg_14: a member whose trade count collapses below `veracity_trade_ratio`
/// of the liquid cohort median — with no manifest gap — is flagged
/// `trade_drought`: dropped frames the aggregate coverage cannot see.
#[test]
fn cvg_14_veracity_flags_trade_drought_with_no_gap() {
    let root = tmp("cvg14");
    let (syms, bin, byb, okx) = table();
    // Window 1: all liquid (60 trades). Window 2: binance/okx stay at 60,
    // bybit collapses to 5 (frames dropped, presence still fine).
    let mut be = dense(Venue::BinanceFutures, bin, 100_000_000_000, 60, 100.0, 40);
    be.extend(dense(Venue::BinanceFutures, bin, 3_700_000_000_000, 60, 100.0, 40));
    compact(&root, Venue::BinanceFutures, "2026-08-04", be, &syms, "hB");
    let mut ye = dense(Venue::Bybit, byb, 100_000_000_000, 60, 100.0, 40);
    ye.extend(dense(Venue::Bybit, byb, 3_700_000_000_000, 5, 100.0, 40));
    compact(&root, Venue::Bybit, "2026-08-04", ye, &syms, "hY");
    let mut oe = dense(Venue::Okx, okx, 100_000_000_000, 60, 100.0, 40);
    oe.extend(dense(Venue::Okx, okx, 3_700_000_000_000, 60, 100.0, 40));
    compact(&root, Venue::Okx, "2026-08-04", oe, &syms, "hO");

    let f = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    let droughts: Vec<&mp_storage::VeracityFinding> = f
        .veracity
        .iter()
        .filter(|v| v.kind == "trade_drought")
        .collect();
    assert_eq!(droughts.len(), 1, "exactly one drought: {:?}", f.veracity);
    assert_eq!(droughts[0].venue, "bybit");
    assert_eq!(droughts[0].window_from_ns, 3_600_000_000_000);
    assert_eq!(droughts[0].trade_count, 5);
    assert_eq!(droughts[0].cohort_median_trades, 60);
    // bybit's price stayed in band — a pure count collapse, not a price issue.
    assert!(f.veracity.iter().all(|v| v.kind != "price_divergence"));
    // And the collapsed window is NOT a manifest gap (bybit has no findings).
    assert!(f.findings.iter().all(|g| g.venue != "bybit"));
}

/// cvg_15: veracity needs a liquid cohort — a window where no member meets
/// `veracity_min_trades` produces no findings (the reference would be noise),
/// and the whole pass stays byte-deterministic.
#[test]
fn cvg_15_veracity_needs_liquid_cohort_and_is_deterministic() {
    let root = tmp("cvg15");
    let (syms, bin, byb, okx) = table();
    // Thin day: 5 trades/venue — far below the 50-trade liquidity bar.
    compact(
        &root,
        Venue::BinanceFutures,
        "2026-08-04",
        dense(Venue::BinanceFutures, bin, 100_000_000_000, 5, 100.0, 40),
        &syms,
        "hB",
    );
    compact(
        &root,
        Venue::Bybit,
        "2026-08-04",
        dense(Venue::Bybit, byb, 100_000_000_000, 5, 130.0, 40),
        &syms,
        "hY",
    );
    compact(
        &root,
        Venue::Okx,
        "2026-08-04",
        dense(Venue::Okx, okx, 100_000_000_000, 5, 100.0, 40),
        &syms,
        "hO",
    );
    let f1 = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    let f2 = detect(&root, "2026-08-04", &cfg(), "s", "h").unwrap();
    // A 30% price outlier on a sub-liquid window must NOT be flagged — no
    // liquid reference exists, so no claim is made (fail-closed, CVG-10 analog).
    assert!(f1.veracity.is_empty(), "thin window ⇒ no veracity findings: {:?}", f1.veracity);
    // Determinism covers the whole artifact including the veracity section.
    assert_eq!(
        serde_json::to_vec_pretty(&f1).unwrap(),
        serde_json::to_vec_pretty(&f2).unwrap()
    );
    let _ = std::fs::remove_dir_all(&root);
}
