//! Cold-store acceptance tests for specs 028/030/031 streams: `cold/positions/`
//! (WHL-6), `cold/macro/` (MAC-6), `cold/options/` (OPT-5). Test names embed
//! requirement IDs (CONV-21). Each asserts the compact path writes the stream
//! to its OWN partition (W-6), manifest `sampled: false`, and a Parquet
//! roundtrip.

use mp_core::{
    EventEnvelope, InstrumentKind, MarketEvent, OptionKind, OptionLeg, Side, SymbolId, SymbolMeta,
    SymbolTable, Venue,
};
use mp_storage::{compact_day, Dataset};

const DAY: i64 = 86_400_000_000_000; // 1 day in ns

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mpstore-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn table(venue: Venue, symbol: &str) -> (SymbolTable, SymbolId) {
    let mut t = SymbolTable::new();
    let id = t.intern(venue, symbol, |id| {
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
    });
    (t, id)
}

/// `compact_day` over one stream's events and return (stats, Dataset).
fn compact(
    root: &std::path::Path,
    venue: Venue,
    date: &str,
    events: Vec<EventEnvelope>,
    syms: &SymbolTable,
) -> mp_storage::CompactStats {
    compact_day(
        root, venue, date, 0, DAY, events, syms, "hash028", "gitsha", 0,
    )
    .unwrap()
}

#[test]
fn whl_6_writes_raw_and_cold_positions_append_only() {
    let root = tmp("whl6");
    let (syms, btc) = table(Venue::Hyperliquid, "BTC");
    let events = vec![
        EventEnvelope::new(
            Venue::Hyperliquid,
            btc,
            10,
            10,
            1,
            MarketEvent::WhalePosition {
                address: "0xaaaa000000000000000000000000000000000001".into(),
                size: 1.5,
                entry: 97000.0,
                leverage: 10.0,
                liq_price: 89000.0,
            },
        ),
        EventEnvelope::new(
            Venue::Hyperliquid,
            btc,
            20,
            20,
            2,
            MarketEvent::WhalePosition {
                address: "0xaaaa000000000000000000000000000000000001".into(),
                size: -2.0,
                entry: 96000.0,
                leverage: 20.0,
                liq_price: f64::NAN, // null liquidationPx sentinel survives
            },
        ),
    ];
    let stats = compact(
        &root,
        Venue::Hyperliquid,
        "2026-07-10",
        events.clone(),
        &syms,
    );
    assert_eq!(
        stats.positions_files_written, 1,
        "positions parquet written"
    );
    assert_eq!(stats.position_rows, 2);
    assert_eq!(stats.trades_files_written, 0, "separate stream, not trades");

    // W-6: file lives under cold/positions/, never cold/trades/.
    let path = mp_storage::layout::partition_file(
        &root,
        "positions",
        Venue::Hyperliquid,
        "BTC",
        "2026-07-10",
    );
    assert!(
        path.exists(),
        "cold/positions file exists: {}",
        path.display()
    );
    assert!(!mp_storage::layout::partition_file(
        &root,
        "trades",
        Venue::Hyperliquid,
        "BTC",
        "2026-07-10"
    )
    .exists());

    // Roundtrip: values survive (NaN compared via is_nan).
    let ds = Dataset::open(&root);
    let back = ds
        .positions_day(Venue::Hyperliquid, "BTC", "2026-07-10")
        .unwrap();
    assert_eq!(back.len(), 2);
    match &back[0].body {
        MarketEvent::WhalePosition {
            address,
            size,
            entry,
            leverage,
            liq_price,
        } => {
            assert_eq!(address, "0xaaaa000000000000000000000000000000000001");
            assert_eq!(*size, 1.5);
            assert_eq!(*entry, 97000.0);
            assert_eq!(*leverage, 10.0);
            assert_eq!(*liq_price, 89000.0);
        }
        other => panic!("expected WhalePosition, got {other:?}"),
    }
    match &back[1].body {
        MarketEvent::WhalePosition { liq_price, .. } => {
            assert!(liq_price.is_nan(), "NaN sentinel survives the roundtrip");
        }
        other => panic!("expected WhalePosition, got {other:?}"),
    }

    // Manifest: census, not sampled (WHL-6).
    let m = mp_storage::compactor::load_manifest(&root, Venue::Hyperliquid, "2026-07-10").unwrap();
    let pos = m.streams.get("positions:BTC").expect("positions stream");
    assert!(!pos.sampled, "positions are a census (WHL-6)");
    assert_eq!(pos.events, 2);

    // Idempotent re-run skips (STO-1), and prune now verifies the new stream.
    let stats2 = compact(&root, Venue::Hyperliquid, "2026-07-10", events, &syms);
    assert_eq!(stats2.positions_files_skipped, 1);
    assert_eq!(stats2.positions_files_written, 0);
    assert!(mp_storage::prune::verify_prunable(&root, Venue::Hyperliquid, "2026-07-10").is_ok());
}

#[test]
fn mac_6_writes_raw_and_cold_macro_append_only() {
    let root = tmp("mac6");
    let (syms, dgs10) = table(Venue::Fred, "DGS10");
    let events = vec![
        EventEnvelope::new(
            Venue::Fred,
            dgs10,
            1_785_715_200_000_000_000,
            100,
            1,
            MarketEvent::MacroPoint {
                series_id: "DGS10".into(),
                value: 4.21,
                date: 1_785_715_200_000_000_000,
            },
        ),
        EventEnvelope::new(
            Venue::Fred,
            dgs10,
            1_785_801_600_000_000_000,
            200,
            2,
            MarketEvent::MacroPoint {
                series_id: "DGS10".into(),
                value: 4.18,
                date: 1_785_801_600_000_000_000,
            },
        ),
    ];
    let stats = compact(&root, Venue::Fred, "2026-07-10", events, &syms);
    assert_eq!(stats.macro_files_written, 1);
    assert_eq!(stats.macro_rows, 2);

    let path =
        mp_storage::layout::partition_file(&root, "macro", Venue::Fred, "DGS10", "2026-07-10");
    assert!(path.exists(), "cold/macro file exists: {}", path.display());

    let ds = Dataset::open(&root);
    let back = ds.macro_day(Venue::Fred, "DGS10", "2026-07-10").unwrap();
    assert_eq!(back.len(), 2);
    match &back[0].body {
        MarketEvent::MacroPoint {
            series_id,
            value,
            date,
        } => {
            assert_eq!(series_id, "DGS10");
            assert_eq!(*value, 4.21);
            assert_eq!(*date, 1_785_715_200_000_000_000);
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }

    let m = mp_storage::compactor::load_manifest(&root, Venue::Fred, "2026-07-10").unwrap();
    let stream = m.streams.get("macro:DGS10").expect("macro stream");
    assert!(
        !stream.sampled,
        "FRED daily observations are authoritative (MAC-6)"
    );
    assert_eq!(stream.events, 2);
}

#[test]
fn opt_5_writes_raw_and_cold_options_append_only() {
    let root = tmp("opt5");
    let (syms, instr) = table(Venue::Deribit, "BTC-28JUN26-100000-C");
    let leg = OptionLeg {
        underlying: "BTC".into(),
        strike: 100_000.0,
        expiry_ts_ns: 1_785_801_600_000_000_000,
        kind: OptionKind::Call,
    };
    let mut events: Vec<EventEnvelope> = vec![
        EventEnvelope::new(
            Venue::Deribit,
            instr,
            10,
            10,
            1,
            MarketEvent::OptionTrade {
                leg: leg.clone(),
                price: 2.5,
                qty: 3.0,
                side: Side::Buy,
                trade_id: 1001,
            },
        ),
        EventEnvelope::new(
            Venue::Deribit,
            instr,
            20,
            20,
            2,
            MarketEvent::OptionBook {
                leg: leg.clone(),
                bids: vec![(2.4, 5.0), (2.3, 7.0)].into_iter().collect(),
                asks: vec![(2.6, 4.0)].into_iter().collect(),
                change_id: 77,
                is_snapshot: true,
            },
        ),
        EventEnvelope::new(
            Venue::Deribit,
            instr,
            30,
            30,
            3,
            MarketEvent::OptionTicker {
                leg: leg.clone(),
                mark_iv: 0.55,
                mark_price: 2.5,
                underlying_price: 96_000.0,
                open_interest: 1234.0,
                greeks: Some(mp_core::OptionGreeks {
                    delta: 0.42,
                    gamma: 0.0001,
                    theta: -1.2,
                    vega: 0.05,
                }),
            },
        ),
    ];
    let stats = compact(&root, Venue::Deribit, "2026-07-10", events.clone(), &syms);
    assert_eq!(stats.options_files_written, 1);
    assert_eq!(stats.option_rows, 3);

    let path = mp_storage::layout::partition_file(
        &root,
        "options",
        Venue::Deribit,
        "BTC-28JUN26-100000-C",
        "2026-07-10",
    );
    assert!(
        path.exists(),
        "cold/options file exists: {}",
        path.display()
    );

    let ds = Dataset::open(&root);
    let back = ds
        .options_day(Venue::Deribit, "BTC-28JUN26-100000-C", "2026-07-10")
        .unwrap();
    assert_eq!(back.len(), 3);
    // Full roundtrip: re-normalize order and compare structurally.
    events.sort_by_key(|e| e.stream_seq);
    for (a, b) in events.iter().zip(back.iter()) {
        assert_eq!(a.symbol, b.symbol);
        assert_eq!(a.recv_ts_ns, b.recv_ts_ns);
        match (&a.body, &b.body) {
            (
                MarketEvent::OptionTrade {
                    leg: la,
                    price,
                    qty,
                    side,
                    trade_id,
                },
                MarketEvent::OptionTrade {
                    leg: lb,
                    price: p2,
                    qty: q2,
                    side: s2,
                    trade_id: t2,
                },
            ) => {
                assert_eq!(la, lb);
                assert_eq!(price, p2);
                assert_eq!(qty, q2);
                assert_eq!(side, s2);
                assert_eq!(trade_id, t2);
            }
            (
                MarketEvent::OptionBook {
                    leg: la,
                    bids: ba,
                    asks: aa,
                    change_id: ca,
                    is_snapshot: sa,
                },
                MarketEvent::OptionBook {
                    leg: lb,
                    bids: bb,
                    asks: ab,
                    change_id: cb,
                    is_snapshot: sb,
                },
            ) => {
                assert_eq!(la, lb);
                assert_eq!(ca, cb);
                assert_eq!(sa, sb);
                assert_eq!(
                    &ba.iter().copied().collect::<Vec<_>>(),
                    &bb.iter().copied().collect::<Vec<_>>()
                );
                assert_eq!(
                    &aa.iter().copied().collect::<Vec<_>>(),
                    &ab.iter().copied().collect::<Vec<_>>()
                );
            }
            (
                MarketEvent::OptionTicker {
                    leg: la,
                    mark_iv,
                    mark_price,
                    underlying_price,
                    open_interest,
                    greeks,
                },
                MarketEvent::OptionTicker {
                    leg: lb,
                    mark_iv: m2,
                    mark_price: mp2,
                    underlying_price: u2,
                    open_interest: o2,
                    greeks: g2,
                },
            ) => {
                assert_eq!(la, lb);
                assert_eq!(mark_iv, m2);
                assert_eq!(mark_price, mp2);
                assert_eq!(underlying_price, u2);
                assert_eq!(open_interest, o2);
                assert_eq!(greeks, g2);
            }
            (a, b) => panic!("mismatched bodies: {a:?} vs {b:?}"),
        }
    }

    let m = mp_storage::compactor::load_manifest(&root, Venue::Deribit, "2026-07-10").unwrap();
    let stream = m
        .streams
        .get("options:BTC-28JUN26-100000-C")
        .expect("options stream");
    assert!(
        !stream.sampled,
        "Deribit public book/trades not throttled (OPT-5)"
    );
    assert_eq!(stream.events, 3);

    // Idempotent re-run + prune verification of the options stream.
    let stats2 = compact(&root, Venue::Deribit, "2026-07-10", events, &syms);
    assert_eq!(stats2.options_files_skipped, 1);
    assert!(mp_storage::prune::verify_prunable(&root, Venue::Deribit, "2026-07-10").is_ok());
}
