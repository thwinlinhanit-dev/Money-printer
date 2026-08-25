//! Acceptance tests for spec 031 (Deribit options recorder). Fixtures are
//! synthetic-representative JSON-RPC frames built to the documented channel
//! shapes — no network (CONV-23). Test names embed requirement IDs (CONV-21).

use mp_collectors::collector::{Collector, CollectorConfig, DriveOutcome};
use mp_collectors::deribit::{parse_instrument_name, DeribitNormalizer};
use mp_collectors::transport::{MockTransport, TeeTransport, Transport};
use mp_collectors::Normalizer;
use mp_core::codec::{decode_event, encode_event};
use mp_core::{EventEnvelope, MarketEvent, OptionKind, Side, StatusKind, Venue};
use proptest::prelude::*;

fn norm(n: &mut dyn Normalizer, recv: i64, json: &str) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    n.normalize(recv, json.as_bytes(), &mut out).unwrap();
    out
}

const BOOK_SNAPSHOT: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"book.BTC-28JUN26-100000-C.10.100ms","data":{"type":"snapshot","timestamp":1,"change_id":100,"instrument_name":"BTC-28JUN26-100000-C","bids":[["new",1000.0,1.0],["new",999.0,2.0]],"asks":[["new",1001.0,0.5]]}}}"#;

fn book_change(prev: u64, cid: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"subscription","params":{{"channel":"book.BTC-28JUN26-100000-C.10.100ms","data":{{"type":"change","timestamp":2,"prev_change_id":{prev},"change_id":{cid},"instrument_name":"BTC-28JUN26-100000-C","bids":[["new",998.0,3.0],["delete",1000.0,0]],"asks":[]}}}}}}"#
    )
}

const TRADES: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"trades.BTC-28JUN26-100000-C.100ms","data":[{"trade_seq":1,"trade_id":"12345","timestamp":3,"price":1000.5,"amount":0.25,"direction":"buy","instrument_name":"BTC-28JUN26-100000-C"},{"trade_seq":2,"trade_id":"abcdef","timestamp":4,"price":999.0,"amount":0.1,"direction":"sell","instrument_name":"BTC-28JUN26-100000-C"}]}}"#;

const TICKER: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"ticker.BTC-28JUN26-100000-C.100ms","data":{"timestamp":5,"instrument_name":"BTC-28JUN26-100000-C","mark_iv":0.55,"mark_price":1002.0,"underlying_price":99000.0,"open_interest":123.4,"greeks":{"delta":0.6,"gamma":0.01,"theta":-0.5,"vega":0.2}}}}"#;

#[test]
fn opt_1_connects_reconnects_emits_status() {
    // Drive the Deribit normalizer through the generic Collector driver with
    // a scripted transport: frames flow, a disconnect resets book state, and
    // status events (gap) flow through the same pipe (COL-1/COL-3).
    let mut collector = Collector::new(DeribitNormalizer::new(), CollectorConfig::default());
    let mut mock = MockTransport::new();
    mock.push_frame(1, BOOK_SNAPSHOT);
    mock.push_frame(2, book_change(100, 101));
    mock.push_disconnect();
    mock.push_frame(3, BOOK_SNAPSHOT);
    mock.push_frame(4, TRADES);

    let mut out = Vec::new();
    let first = collector.drive(&mut mock, &mut out);
    assert_eq!(
        first,
        DriveOutcome::Disconnected,
        "disconnect must surface (COL-1)"
    );
    assert!(out
        .iter()
        .any(|e| matches!(e.body, MarketEvent::OptionBook { is_snapshot, .. } if is_snapshot)));
    assert!(out
        .iter()
        .any(|e| matches!(e.body, MarketEvent::OptionBook { is_snapshot, .. } if !is_snapshot)));

    // Reconnect: book state was reset by the disconnect (reset_books on
    // Disconnected in `Collector::drive`), so a fresh snapshot re-initializes.
    let second = collector.drive(&mut mock, &mut out);
    assert_eq!(second, DriveOutcome::Exhausted);
    assert!(out
        .iter()
        .any(|e| matches!(e.body, MarketEvent::OptionTrade { .. })));
}

proptest! {
    #[test]
    fn opt_2_instrument_name_parses_to_option_metadata(
        underlying in prop_oneof!["BTC", "ETH", "SOL"],
        y in 24u32..40,
        m in 1u32..12,
        d in 1u32..28,
        strike in 1u64..100_000_000,
        call in any::<bool>(),
    ) {
        let month = ["JAN","FEB","MAR","APR","MAY","JUN","JUL","AUG","SEP","OCT","NOV","DEC"][(m - 1) as usize];
        let name = format!("{underlying}-{d:02}{month}{y:02}-{strike}-{}", if call { "C" } else { "P" });
        let leg = parse_instrument_name(&name).expect("generated name must parse");
        // Strike scaling: BTC/SOL cents of USD, ETH milli-USD.
        let divisor = if underlying == "ETH" { 1000.0 } else { 100.0 };
        prop_assert_eq!(leg.underlying, underlying);
        prop_assert_eq!(leg.kind, if call { OptionKind::Call } else { OptionKind::Put });
        prop_assert!((leg.strike - strike as f64 / divisor).abs() < 1e-9);
        prop_assert!(leg.expiry_ts_ns > 0);
    }
}

#[test]
fn opt_2_book_and_trade_events_carry_leg() {
    let mut n = DeribitNormalizer::new();
    let events = norm(&mut n, 1, BOOK_SNAPSHOT);
    match &events[0].body {
        MarketEvent::OptionBook {
            leg,
            bids,
            asks,
            is_snapshot,
            change_id,
        } => {
            assert_eq!(leg.underlying, "BTC");
            assert_eq!(leg.strike, 1000.0);
            assert_eq!(leg.kind, OptionKind::Call);
            assert!(is_snapshot);
            assert_eq!(*change_id, 100);
            assert_eq!(bids.len(), 2);
            assert_eq!(asks.len(), 1);
        }
        other => panic!("expected OptionBook, got {other:?}"),
    }

    let trades = norm(&mut n, 2, TRADES);
    assert_eq!(trades.len(), 2);
    match &trades[0].body {
        MarketEvent::OptionTrade {
            leg,
            price,
            qty,
            side,
            trade_id,
        } => {
            assert_eq!(leg.underlying, "BTC");
            assert_eq!(*price, 1000.5);
            assert_eq!(*qty, 0.25);
            assert_eq!(*side, Side::Buy);
            assert_eq!(*trade_id, 12345);
        }
        other => panic!("expected OptionTrade, got {other:?}"),
    }
    // Non-numeric trade_id falls back to trade_seq (spec 001 Decisions).
    match &trades[1].body {
        MarketEvent::OptionTrade { trade_id, side, .. } => {
            assert_eq!(*trade_id, 2);
            assert_eq!(*side, Side::Sell);
        }
        other => panic!("expected OptionTrade, got {other:?}"),
    }
}

#[test]
fn opt_2_ticker_carries_greeks_at_record() {
    let mut n = DeribitNormalizer::new();
    let events = norm(&mut n, 1, TICKER);
    match &events[0].body {
        MarketEvent::OptionTicker {
            leg,
            mark_iv,
            greeks,
            ..
        } => {
            assert_eq!(leg.underlying, "BTC");
            assert_eq!(*mark_iv, 0.55);
            let g = greeks.as_ref().expect("greeks present in ticker fixture");
            assert_eq!(g.delta, 0.6);
            assert_eq!(g.vega, 0.2);
        }
        other => panic!("expected OptionTicker, got {other:?}"),
    }
}

#[test]
fn opt_3_raw_frames_captured_verbatim() {
    // COL-9/OPT-3: the tee transport writes the exact venue bytes (recv-prefixed)
    // pre-parse, append-only.
    let dir = std::env::temp_dir().join(format!("mp-opt3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("frames.ndjson");
    let sink = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();

    let mut mock = MockTransport::new();
    mock.push_frame(7, b"{\"channel\":\"alpha\"}".to_vec());
    let mut tee = TeeTransport::new(mock, sink);
    let ev = tee.poll().unwrap();
    assert!(matches!(
        ev,
        mp_collectors::TransportEvent::Frame { recv_ts_ns: 7, .. }
    ));
    assert!(tee.poll().is_none());

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("7\t{\"channel\":\"alpha\"}"),
        "verbatim bytes + recv prefix: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opt_4_normalization_deterministic() {
    let mut n1 = DeribitNormalizer::new();
    let mut n2 = DeribitNormalizer::new();
    let a = norm(&mut n1, 42, BOOK_SNAPSHOT);
    let b = norm(&mut n2, 42, BOOK_SNAPSHOT);
    assert_eq!(a, b, "same input must produce identical events (golden)");
    let a2 = norm(&mut n1, 43, &book_change(100, 101));
    let b2 = norm(&mut n2, 43, &book_change(100, 101));
    assert_eq!(a2, b2);
}

proptest! {
    #[test]
    fn opt_4_book_reconstruction_contiguous_changes(gap_between in 0u64..5) {
        // CONV-22: book reconstruction per Deribit's algorithm. A contiguous
        // change (prev_change_id == last) applies; a jump emits GapDetected
        // and drops changes until the next snapshot.
        let mut n = DeribitNormalizer::new();
        let mut out = Vec::new();
        n.normalize(1, BOOK_SNAPSHOT.as_bytes(), &mut out).unwrap();
        let gap = gap_between > 0;
        let prev = if gap { 100 + gap_between } else { 100 };
        n.normalize(2, book_change(prev, 101).as_bytes(), &mut out).unwrap();
        // Any further changes while stale are dropped.
        n.normalize(3, book_change(101, 102).as_bytes(), &mut out).unwrap();
        let gap_seen = out
            .iter()
            .any(|e| matches!(e.body, MarketEvent::Status { kind: StatusKind::GapDetected, .. }));
        let change_seen = out.iter().any(|e| {
            matches!(e.body, MarketEvent::OptionBook { is_snapshot, .. } if !is_snapshot)
        });
        let any_status = out.iter().any(|e| matches!(e.body, MarketEvent::Status { .. }));
        if gap {
            // The gapped change emits Status::GapDetected and the stale change
            // after it is dropped: only the snapshot + gap status remain.
            prop_assert!(gap_seen);
            prop_assert!(!change_seen);
            prop_assert_eq!(out.len(), 2);
        } else {
            // Both changes are contiguous: snapshot + 2 changes, no status.
            prop_assert!(!any_status);
            prop_assert!(change_seen);
            prop_assert_eq!(out.len(), 3);
        }
    }
}

#[test]
fn opt_6_recording_only_no_analytics() {
    // OPT-6: recording only — no vol-surface/GEX/greeks analytics in scope.
    // Grep for implementation identifiers (doc comments mention the excluded
    // features by name, so match the underscore-identifier forms).
    let src = include_str!("../src/deribit.rs");
    assert!(
        !src.contains("vol_surface"),
        "no vol-surface analytics (OPT-6)"
    );
    assert!(
        !src.contains("fn gex") && !src.contains("gex_surface"),
        "no GEX analytics (OPT-6)"
    );
    let manifest = include_str!("../Cargo.toml");
    assert!(
        !manifest.contains("mp-strategies"),
        "no strategy dependency (PD-4)"
    );
}

#[test]
fn opt_8_fixtures_no_network() {
    // CONV-23: the Normalizer implementation is a pure function of frames.
    // (The file also hosts the live-http-gated `rest` module for instrument
    // discovery — check only the `impl Normalizer` block for network calls.)
    let src = include_str!("../src/deribit.rs");
    let start = src
        .find("impl Normalizer for DeribitNormalizer")
        .expect("normalizer impl");
    let end = src.find("fn reset_books").expect("reset_books");
    let normalizer_impl = &src[start..end];
    assert!(
        !normalizer_impl.contains("TcpStream") && !normalizer_impl.contains("reqwest"),
        "the normalizer itself must never touch the network (CONV-23)"
    );
}

#[test]
fn opt_9_malformed_input_no_panic_and_nan_fail_closed() {
    // CONV-15: no panic on malformed input; CONV-8: NaN never recorded as a
    // real value.
    let mut n = DeribitNormalizer::new();
    let mut out = Vec::new();
    assert!(n.normalize(1, b"not json", &mut out).is_err());
    // Error frames are parse errors (WARN + count + skip), not panics.
    assert!(n
        .normalize(
            1,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Invalid params"}}"#
                .as_bytes(),
            &mut out,
        )
        .is_err());
    // Subscription acks are ignored.
    assert!(n
        .normalize(
            1,
            r#"{"jsonrpc":"2.0","id":1,"result":["book.BTC-28JUN26-100000-C.10.100ms"]}"#
                .as_bytes(),
            &mut out
        )
        .is_ok());
    // Ticker with absent greeks → None; missing values → NaN sentinel.
    let events = norm(
        &mut n,
        1,
        r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"ticker.ETH-30SEP26-5000-P.100ms","data":{"timestamp":1,"instrument_name":"ETH-30SEP26-5000-P"}}}"#,
    );
    assert_eq!(events.len(), 1);
    match &events[0].body {
        MarketEvent::OptionTicker {
            mark_iv, greeks, ..
        } => {
            assert!(mark_iv.is_nan(), "absent mark_iv must be NaN, not invented");
            assert!(greeks.is_none());
        }
        other => panic!("expected OptionTicker, got {other:?}"),
    }
    // Perp/future instruments on a channel are dropped, not events.
    let perp = norm(
        &mut n,
        1,
        r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"trades.BTC-PERPETUAL.100ms","data":[{"price":1,"amount":1,"direction":"buy","instrument_name":"BTC-PERPETUAL"}]}}"#,
    );
    assert!(perp.is_empty());
}

proptest! {
    #[test]
    fn opt_10_option_event_variants_roundtrip(
        underlying in "[A-Z]{2,4}",
        strike in 0.0f64..1.0e6,
        expiry in any::<i64>(),
        price in -1.0e9f64..1.0e9,
        qty in 0.0f64..1.0e9,
        trade_id in any::<u64>(),
    ) {
        // The new option variants are part of the event schema (spec 001
        // amendment, CONV-20) and must round-trip (EVT-3).
        let leg = mp_core::OptionLeg { underlying, strike, expiry_ts_ns: expiry, kind: OptionKind::Call };
        let e = EventEnvelope::new(Venue::Deribit, mp_core::SymbolId(0), 0, 1, 0,
            MarketEvent::OptionTrade { leg, price, qty, side: Side::Buy, trade_id });
        let back = decode_event(&encode_event(&e).unwrap()).unwrap();
        prop_assert_eq!(e, back);
    }
}

#[test]
fn opt_10_host_and_schema_signed_off() {
    // OPT-10: the new schema (schema_ver 5, specs 033/034 + spec 040 —
    // appended TradeWithAddr/NetflowSnapshot variants + Venue::Ethereum +
    // Venue::Cboe) + new host are documented in the checked-in example
    // config, with no credentials (PD-2).
    assert_eq!(
        mp_core::SCHEMA_VER,
        5,
        "schema amendment (spec 001) is live"
    );
    let example = include_str!("../deribit.example.toml");
    assert!(
        example.contains("wss://www.deribit.com") || example.contains("deribit"),
        "example config documents the host"
    );
    assert!(!example.contains("api_key") && !example.contains("secret"));
}

#[cfg(all(feature = "live-ws", feature = "live-http"))]
#[test]
fn opt_7_check_config_rejects_unknown_fields() {
    // CONV-16: the real binary rejects unknown config keys on --check-config.
    let bin = env!("CARGO_BIN_EXE_mp-collector");
    let dir = std::env::temp_dir().join(format!("mp-opt7-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.toml");
    std::fs::write(
        &good,
        "venue = \"deribit\"\nsymbol = \"BTC-OPTIONS\"\ncurrency = \"BTC\"\n",
    )
    .unwrap();
    let bad = dir.join("bad.toml");
    std::fs::write(
        &bad,
        "venue = \"deribit\"\nsymbol = \"BTC-OPTIONS\"\nbogus_key = 1\n",
    )
    .unwrap();

    let ok = std::process::Command::new(bin)
        .args(["--check-config", "--config"])
        .arg(&good)
        .output()
        .unwrap();
    assert!(ok.status.success(), "good config must pass: {:?}", ok);

    let err = std::process::Command::new(bin)
        .args(["--check-config", "--config"])
        .arg(&bad)
        .output()
        .unwrap();
    assert!(
        !err.status.success(),
        "unknown field must be rejected (CONV-16)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
