//! Fixture tests for every venue normalizer (COL-5/7/8). Test names embed IDs
//! (CONV-21). Fixtures are SYNTHETIC-representative frames built to documented
//! shapes — real captures replace them to close COL-13 (spec 002 Decisions).

use mp_collectors::{
    BinanceNormalizer, CoinbaseNormalizer, HyperliquidNormalizer, KrakenNormalizer, Normalizer,
    OkxNormalizer,
};
use mp_core::{EventEnvelope, MarketEvent, Side, SnapshotReason, StatusKind};

fn norm(n: &mut dyn Normalizer, recv: i64, json: &str) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    n.normalize(recv, json.as_bytes(), &mut out).unwrap();
    out
}

fn trade_of(e: &EventEnvelope) -> (f64, f64, Side) {
    match e.body {
        MarketEvent::Trade {
            price, qty, side, ..
        } => (price, qty, side),
        _ => panic!("expected Trade, got {:?}", e.body),
    }
}

#[test]
fn col_5_okx_trade_and_book_gap() {
    let mut n = OkxNormalizer::new();
    let t = norm(
        &mut n,
        1,
        r#"{"arg":{"channel":"trades","instId":"BTC-USDT-SWAP"},"data":[{"instId":"BTC-USDT-SWAP","tradeId":"7","px":"50000","sz":"0.5","side":"sell","ts":"1699999999999"}]}"#,
    );
    assert_eq!(trade_of(&t[0]), (50000.0, 0.5, Side::Sell));

    // snapshot then a gapped update (prevSeqId mismatch).
    norm(
        &mut n,
        2,
        r#"{"arg":{"channel":"books","instId":"BTC-USDT-SWAP"},"action":"snapshot","data":[{"bids":[["50000","1"]],"asks":[["50001","1"]],"seqId":10,"prevSeqId":-1,"ts":"2"}]}"#,
    );
    let g = norm(
        &mut n,
        3,
        r#"{"arg":{"channel":"books","instId":"BTC-USDT-SWAP"},"action":"update","data":[{"bids":[["49999","2"]],"asks":[],"seqId":20,"prevSeqId":15,"ts":"3"}]}"#,
    );
    assert!(matches!(
        g[0].body,
        MarketEvent::Status {
            kind: StatusKind::GapDetected,
            ..
        }
    ));
}

#[test]
fn col_5_binance_aggtrade_side_and_markprice_funding() {
    let mut n = BinanceNormalizer::new();
    // m=true ⇒ buyer is maker ⇒ aggressor is the seller.
    let t = norm(
        &mut n,
        1,
        r#"{"e":"aggTrade","E":1,"s":"BTCUSDT","a":5,"p":"50000","q":"0.1","T":1,"m":true}"#,
    );
    assert_eq!(trade_of(&t[0]).2, Side::Sell);

    // markPriceUpdate carries mark + funding together → two events.
    let m = norm(
        &mut n,
        2,
        r#"{"e":"markPriceUpdate","E":2,"s":"BTCUSDT","p":"50010","i":"50005","r":"0.0001","T":1673280000000}"#,
    );
    assert!(m
        .iter()
        .any(|e| matches!(e.body, MarketEvent::MarkPrice { mark, .. } if mark == 50010.0)));
    assert!(m
        .iter()
        .any(|e| matches!(e.body, MarketEvent::Funding { rate, .. } if rate == 0.0001)));
}

#[test]
fn col_7_binance_depth_seeds_then_deltas() {
    let mut n = BinanceNormalizer::new();
    // First depthUpdate seeds a synthetic snapshot.
    let s = norm(
        &mut n,
        1,
        r#"{"e":"depthUpdate","E":1,"s":"BTCUSDT","U":100,"u":105,"pu":99,"b":[["50000","1"]],"a":[["50001","1"]]}"#,
    );
    assert!(matches!(s[0].body, MarketEvent::BookSnapshot { .. }));
    // Contiguous next update (U == last u + 1).
    let d = norm(
        &mut n,
        2,
        r#"{"e":"depthUpdate","E":2,"s":"BTCUSDT","U":106,"u":110,"pu":105,"b":[["49999","2"]],"a":[]}"#,
    );
    assert!(matches!(
        d[0].body,
        MarketEvent::BookDelta {
            first_seq: 106,
            last_seq: 110,
            ..
        }
    ));
}

#[test]
fn col_8_binance_force_order_is_liquidation() {
    let mut n = BinanceNormalizer::new();
    let l = norm(
        &mut n,
        1,
        r#"{"e":"forceOrder","E":1,"o":{"s":"BTCUSDT","S":"SELL","q":"2.0","p":"49000","T":1}}"#,
    );
    assert!(matches!(
        l[0].body,
        MarketEvent::Liquidation {
            side: Side::Sell,
            ..
        }
    ));
}

#[test]
fn col_5_coinbase_trade_and_l2_snapshot() {
    let mut n = CoinbaseNormalizer::new();
    let t = norm(
        &mut n,
        1,
        r#"{"channel":"market_trades","events":[{"type":"snapshot","trades":[{"trade_id":"9","product_id":"BTC-USD","price":"50000","size":"0.2","side":"SELL","time":"2023-01-01T00:00:00Z"}]}]}"#,
    );
    assert_eq!(trade_of(&t[0]), (50000.0, 0.2, Side::Sell));

    let b = norm(
        &mut n,
        2,
        r#"{"channel":"l2_data","events":[{"type":"snapshot","product_id":"BTC-USD","updates":[{"side":"bid","price_level":"50000","new_quantity":"1"},{"side":"offer","price_level":"50001","new_quantity":"2"}]}]}"#,
    );
    match &b[0].body {
        MarketEvent::BookSnapshot { bids, asks, .. } => {
            assert_eq!(bids.len(), 1);
            assert_eq!(asks.len(), 1);
        }
        other => panic!("expected snapshot, got {other:?}"),
    }
}

#[test]
fn col_7_kraken_book_snapshot_delta_and_ticker() {
    let mut n = KrakenNormalizer::new();
    norm(
        &mut n,
        1,
        r#"{"feed":"book_snapshot","product_id":"PI_XBTUSD","seq":10,"time":1,"bids":[{"price":50000,"qty":1}],"asks":[{"price":50001,"qty":1}]}"#,
    );
    let d = norm(
        &mut n,
        2,
        r#"{"feed":"book","product_id":"PI_XBTUSD","side":"buy","seq":11,"price":49999,"qty":3,"time":2}"#,
    );
    assert!(matches!(
        d[0].body,
        MarketEvent::BookDelta {
            first_seq: 11,
            last_seq: 11,
            ..
        }
    ));

    let tk = norm(
        &mut n,
        3,
        r#"{"feed":"ticker","product_id":"PI_XBTUSD","markPrice":50005,"funding_rate":0.0001,"openInterest":1234}"#,
    );
    assert_eq!(tk.len(), 3); // mark + funding + OI
}

#[test]
fn col_5_hyperliquid_trade_book_and_ctx() {
    let mut n = HyperliquidNormalizer::new();
    let t = norm(
        &mut n,
        1,
        r#"{"channel":"trades","data":[{"coin":"BTC","side":"A","px":"50000","sz":"0.3","time":1,"tid":42}]}"#,
    );
    assert_eq!(trade_of(&t[0]), (50000.0, 0.3, Side::Sell));

    let b = norm(
        &mut n,
        2,
        r#"{"channel":"l2Book","data":{"coin":"BTC","time":2,"levels":[[{"px":"50000","sz":"1"}],[{"px":"50001","sz":"2"}]]}}"#,
    );
    assert!(matches!(
        b[0].body,
        MarketEvent::BookSnapshot {
            reason: SnapshotReason::Periodic,
            ..
        }
    ));

    let c = norm(
        &mut n,
        3,
        r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"markPx":"50010","oraclePx":"50008","funding":"0.00001","openInterest":"999"}}}"#,
    );
    assert!(c
        .iter()
        .any(|e| matches!(e.body, MarketEvent::MarkPrice { .. })));
    assert!(c
        .iter()
        .any(|e| matches!(e.body, MarketEvent::Funding { .. })));
    assert!(c
        .iter()
        .any(|e| matches!(e.body, MarketEvent::OpenInterest { .. })));
}
