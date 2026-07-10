//! Bybit normalizer fixture tests (COL-5/6/7/8). Test names embed IDs (CONV-21).
//!
//! Fixtures are SYNTHETIC-REPRESENTATIVE frames built to documented Bybit v5
//! shapes — not real captures. Replacing them with real captured frames is
//! required to fully close COL-13 (see spec 002 Decisions).

use mp_collectors::{BybitNormalizer, Normalizer};
use mp_core::{EventEnvelope, MarketEvent, Side, SnapshotReason, StatusKind};

fn norm(n: &mut BybitNormalizer, recv_ts_ns: i64, json: &str) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    n.normalize(recv_ts_ns, json.as_bytes(), &mut out).unwrap();
    out
}

#[test]
fn col_5_trade_normalized_with_recv_ts() {
    let mut n = BybitNormalizer::new();
    let json = r#"{"topic":"publicTrade.BTCUSDT","type":"snapshot","ts":1672304486868,
        "data":[{"T":1672304486865,"s":"BTCUSDT","S":"Buy","v":"0.001","p":"16578.50","i":"abc-1"}]}"#;
    let out = norm(&mut n, 42, json);
    assert_eq!(out.len(), 1);
    let e = &out[0];
    assert_eq!(e.recv_ts_ns, 42, "recv_ts stamped by collector (COL-5)");
    assert_eq!(e.exch_ts_ns, 1672304486865 * 1_000_000);
    match e.body {
        MarketEvent::Trade {
            price, qty, side, ..
        } => {
            assert_eq!(price, 16578.50);
            assert_eq!(qty, 0.001);
            assert_eq!(side, Side::Buy);
        }
        _ => panic!("expected Trade"),
    }
}

#[test]
fn col_6_malformed_frame_is_parse_error() {
    let mut n = BybitNormalizer::new();
    let mut out = Vec::new();
    // Missing required price field.
    let bad = r#"{"topic":"publicTrade.BTCUSDT","type":"snapshot","data":[{"s":"BTCUSDT","S":"Buy","v":"0.001"}]}"#;
    assert!(n.normalize(1, bad.as_bytes(), &mut out).is_err());
    // Non-JSON.
    assert!(n.normalize(1, b"not json", &mut out).is_err());
    // Unknown topic is ignored, not an error.
    let unk = r#"{"topic":"kline.1.BTCUSDT","type":"snapshot","data":[]}"#;
    assert!(n.normalize(1, unk.as_bytes(), &mut out).is_ok());
    // Subscription ack (no topic) is ignored.
    let ack = r#"{"success":true,"op":"subscribe"}"#;
    assert!(n.normalize(1, ack.as_bytes(), &mut out).is_ok());
}

#[test]
fn col_7_book_snapshot_delta_and_gap_resync() {
    let mut n = BybitNormalizer::new();

    // Snapshot at u=100 → Init.
    let snap = r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":1,
        "data":{"s":"BTCUSDT","b":[["100.0","5"],["99.0","3"]],"a":[["101.0","4"]],"u":100}}"#;
    let out = norm(&mut n, 1, snap);
    assert_eq!(out.len(), 1);
    assert!(matches!(
        out[0].body,
        MarketEvent::BookSnapshot {
            seq: 100,
            reason: SnapshotReason::Init,
            ..
        }
    ));

    // Contiguous delta u=101 → applied.
    let d1 = r#"{"topic":"orderbook.50.BTCUSDT","type":"delta","ts":2,
        "data":{"s":"BTCUSDT","b":[["100.0","0"]],"a":[],"u":101}}"#;
    let out = norm(&mut n, 2, d1);
    assert!(matches!(
        out[0].body,
        MarketEvent::BookDelta {
            first_seq: 101,
            last_seq: 101,
            ..
        }
    ));

    // Gap: u jumps to 105 → GapDetected, delta dropped.
    let d2 = r#"{"topic":"orderbook.50.BTCUSDT","type":"delta","ts":3,
        "data":{"s":"BTCUSDT","b":[["98.0","9"]],"a":[],"u":105}}"#;
    let out = norm(&mut n, 3, d2);
    assert_eq!(out.len(), 1);
    assert!(matches!(
        out[0].body,
        MarketEvent::Status {
            kind: StatusKind::GapDetected,
            ..
        }
    ));

    // Further deltas dropped while desynced.
    let d3 = r#"{"topic":"orderbook.50.BTCUSDT","type":"delta","ts":4,
        "data":{"s":"BTCUSDT","b":[["97.0","1"]],"a":[],"u":106}}"#;
    assert_eq!(norm(&mut n, 4, d3).len(), 0);

    // Fresh snapshot → GapResync, book usable again.
    let snap2 = r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":5,
        "data":{"s":"BTCUSDT","b":[["97.0","1"]],"a":[["98.0","1"]],"u":200}}"#;
    let out = norm(&mut n, 5, snap2);
    assert!(matches!(
        out[0].body,
        MarketEvent::BookSnapshot {
            seq: 200,
            reason: SnapshotReason::GapResync,
            ..
        }
    ));
}

#[test]
fn col_8_tickers_and_liquidation_recorded() {
    let mut n = BybitNormalizer::new();

    // tickers delta carrying funding + mark + OI → three events.
    let tick = r#"{"topic":"tickers.BTCUSDT","type":"snapshot","ts":10,
        "data":{"symbol":"BTCUSDT","fundingRate":"0.0001","nextFundingTime":"1673280000000",
                "markPrice":"16600.0","indexPrice":"16599.0","openInterest":"1234.5","openInterestValue":"20000000"}}"#;
    let out = norm(&mut n, 10, tick);
    assert_eq!(out.len(), 3);
    assert!(out
        .iter()
        .any(|e| matches!(e.body, MarketEvent::Funding { rate, .. } if rate == 0.0001)));
    assert!(out
        .iter()
        .any(|e| matches!(e.body, MarketEvent::MarkPrice { mark, .. } if mark == 16600.0)));
    assert!(out
        .iter()
        .any(|e| matches!(e.body, MarketEvent::OpenInterest { oi_contracts, .. } if oi_contracts == 1234.5)));

    // liquidation recorded as-is (COL-8).
    let liq = r#"{"topic":"liquidation.BTCUSDT","type":"snapshot","ts":11,
        "data":{"symbol":"BTCUSDT","side":"Sell","size":"0.5","price":"16500.0"}}"#;
    let out = norm(&mut n, 11, liq);
    assert_eq!(out.len(), 1);
    match out[0].body {
        MarketEvent::Liquidation { price, qty, side } => {
            assert_eq!(price, 16500.0);
            assert_eq!(qty, 0.5);
            assert_eq!(side, Side::Sell);
        }
        _ => panic!("expected Liquidation"),
    }
}
