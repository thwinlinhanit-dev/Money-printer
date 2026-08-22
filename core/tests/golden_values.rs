//! Representative serialized values for the spec 001 codec-amendment
//! golden-bytes tests (BDC-3).
//!
//! This module is included by both `golden_capture.rs` (which printed the
//! bincode-1.3.3 hex committed below in `GOLDEN`) and `bincode_migration.rs`
//! (which asserts the bincode-2 line reproduces and decodes those bytes).
//! The constructors MUST stay byte-stable: they are the wire-format law for
//! every type the event-log codec serializes.

use mp_core::event::{
    EventEnvelope, EventProvenance, MarketEvent, OptionGreeks, OptionKind, OptionLeg, Side,
    SnapshotReason, SnapshotSource, StatusKind, SymbolId, Venue,
};
use mp_core::log::EnvelopeV1;
use mp_core::{InstrumentKind, Levels, SymbolMeta};

/// Inline SmallVec path (≤ 8 levels).
pub fn inline_levels() -> Levels {
    Levels::from(vec![(100.0, 1.0), (99.5, 2.0)])
}

/// Spilled SmallVec path (> 8 levels).
pub fn spilled_levels() -> Levels {
    Levels::from(
        (0..10)
            .map(|i| (100.0 - i as f64 * 0.5, (i + 1) as f64))
            .collect::<Vec<_>>(),
    )
}

fn env(venue: Venue, symbol: u32, body: MarketEvent) -> EventEnvelope {
    EventEnvelope::new(venue, SymbolId(symbol), 0, 0, 0, body)
}

fn provenance(stream: &str, conn: u64, source: SnapshotSource) -> EventProvenance {
    EventProvenance {
        stream: stream.into(),
        subscription: "orderbook.50.BTCUSDT".into(),
        connection_id: conn,
        snapshot_source: source,
    }
}

fn leg(kind: OptionKind) -> OptionLeg {
    OptionLeg {
        underlying: "BTC".into(),
        strike: 100_000.0,
        expiry_ts_ns: 1_800_000_000_000_000_000,
        kind,
    }
}

/// One envelope per `MarketEvent` variant (all 16), plus the coverage the
/// spec's BDC-3 list demands: inline + spilled `Levels`, `Option` Some/None,
/// `StatusKind::BackpressureDrop`, `Venue::{Deribit, Fred, Ethereum}`,
/// every `SnapshotReason` / `SnapshotSource`, both `OptionKind`s, and empty +
/// non-empty strings. Order matters: `GOLDEN` mirrors it.
pub fn envelopes() -> Vec<(&'static str, EventEnvelope)> {
    vec![
        (
            "trade",
            env(
                Venue::BinanceFutures,
                1,
                MarketEvent::Trade {
                    price: 61_000.5,
                    qty: 0.25,
                    side: Side::Buy,
                    trade_id: 7,
                },
            ),
        ),
        (
            "book_delta",
            env(
                Venue::Bybit,
                2,
                MarketEvent::BookDelta {
                    bids: spilled_levels(),
                    asks: spilled_levels(),
                    first_seq: 100,
                    last_seq: 109,
                },
            )
            .with_provenance(provenance("depth", 43, SnapshotSource::WebSocket)),
        ),
        (
            "book_snapshot_init",
            env(
                Venue::Okx,
                3,
                MarketEvent::BookSnapshot {
                    bids: inline_levels(),
                    asks: inline_levels(),
                    seq: 200,
                    depth: 50,
                    reason: SnapshotReason::Init,
                },
            )
            .with_provenance(provenance("depth", 42, SnapshotSource::Rest)),
        ),
        (
            "book_snapshot_gap",
            env(
                Venue::Okx,
                3,
                MarketEvent::BookSnapshot {
                    bids: spilled_levels(),
                    asks: inline_levels(),
                    seq: 210,
                    depth: 50,
                    reason: SnapshotReason::GapResync,
                },
            ),
        ),
        (
            "book_snapshot_periodic",
            env(
                Venue::Okx,
                3,
                MarketEvent::BookSnapshot {
                    bids: inline_levels(),
                    asks: inline_levels(),
                    seq: 220,
                    depth: 25,
                    reason: SnapshotReason::Periodic,
                },
            ),
        ),
        (
            "funding",
            env(
                Venue::Hyperliquid,
                4,
                MarketEvent::Funding {
                    rate: 0.0001,
                    interval_s: 28_800,
                    next_funding_ts_ns: 1_700_000_000_000_000_000,
                },
            ),
        ),
        (
            "mark_price",
            env(
                Venue::Coinbase,
                5,
                MarketEvent::MarkPrice {
                    mark: 61_000.0,
                    index: f64::NAN,
                },
            ),
        ),
        (
            "open_interest",
            env(
                Venue::KrakenFutures,
                6,
                MarketEvent::OpenInterest {
                    oi_contracts: 12_345.5,
                    oi_notional: f64::NAN,
                },
            ),
        ),
        (
            "liquidation",
            env(
                Venue::BinanceFutures,
                7,
                MarketEvent::Liquidation {
                    price: 59_000.0,
                    qty: 12.5,
                    side: Side::Sell,
                },
            ),
        ),
        (
            "index_price",
            env(Venue::Bybit, 8, MarketEvent::IndexPrice { index: 60_999.9 })
                .with_provenance(provenance("index", 9, SnapshotSource::Synthetic)),
        ),
        (
            "status_backpressure",
            env(
                Venue::Okx,
                9,
                MarketEvent::Status {
                    kind: StatusKind::BackpressureDrop { dropped: 7 },
                    detail: "queue full".into(),
                },
            ),
        ),
        (
            "status_census",
            env(
                Venue::Hyperliquid,
                9,
                MarketEvent::Status {
                    kind: StatusKind::Census,
                    detail: String::new(),
                },
            ),
        ),
        (
            "whale_position",
            env(
                Venue::Hyperliquid,
                10,
                MarketEvent::WhalePosition {
                    address: "0xabc123".into(),
                    size: -123.45,
                    entry: 50_000.0,
                    leverage: 10.0,
                    liq_price: 45_000.0,
                },
            ),
        ),
        (
            "macro_point",
            env(
                Venue::Fred,
                11,
                MarketEvent::MacroPoint {
                    series_id: "DGS10".into(),
                    value: 4.25,
                    date: 1_752_000_000_000_000_000,
                },
            ),
        ),
        (
            "option_trade",
            env(
                Venue::Deribit,
                12,
                MarketEvent::OptionTrade {
                    leg: leg(OptionKind::Call),
                    price: 500.0,
                    qty: 0.1,
                    side: Side::Sell,
                    trade_id: 99,
                },
            ),
        ),
        (
            "option_book",
            env(
                Venue::Deribit,
                12,
                MarketEvent::OptionBook {
                    leg: leg(OptionKind::Put),
                    bids: spilled_levels(),
                    asks: spilled_levels(),
                    change_id: 555,
                    is_snapshot: true,
                },
            ),
        ),
        (
            "option_ticker_greeks_some",
            env(
                Venue::Deribit,
                12,
                MarketEvent::OptionTicker {
                    leg: leg(OptionKind::Call),
                    mark_iv: 0.55,
                    mark_price: 480.0,
                    underlying_price: 61_000.0,
                    open_interest: 1_000.5,
                    greeks: Some(OptionGreeks {
                        delta: 0.5,
                        gamma: 0.001,
                        theta: -12.0,
                        vega: 0.4,
                    }),
                },
            ),
        ),
        (
            "option_ticker_greeks_none",
            env(
                Venue::Deribit,
                12,
                MarketEvent::OptionTicker {
                    leg: leg(OptionKind::Put),
                    mark_iv: 0.6,
                    mark_price: 470.0,
                    underlying_price: 61_000.0,
                    open_interest: 900.25,
                    greeks: None,
                },
            ),
        ),
        (
            "trade_with_addr",
            env(
                Venue::Hyperliquid,
                13,
                MarketEvent::TradeWithAddr {
                    price: 61_000.5,
                    qty: 0.25,
                    side: Side::Buy,
                    trade_id: 7,
                    taker_addr: "0xabc123".into(),
                },
            ),
        ),
        (
            "netflow_snapshot",
            env(
                Venue::Ethereum,
                14,
                MarketEvent::NetflowSnapshot {
                    address: "0xdeadbeef".into(),
                    balance: 1_234_567.89,
                },
            ),
        ),
    ]
}

/// Two `SymbolMeta`s: a plain perp and a `TradFiSynthetic` (spec 030) with
/// non-default multiplier/lifecycle fields.
pub fn symbol_metas() -> Vec<SymbolMeta> {
    vec![
        SymbolMeta::new(
            SymbolId(0),
            Venue::BinanceFutures,
            "BTCUSDT",
            "BTC",
            "USDT",
            InstrumentKind::Perp,
            0.1,
            0.001,
            5.0,
        ),
        SymbolMeta {
            symbol_id: SymbolId(1),
            venue: Venue::Hyperliquid,
            venue_symbol: "CRUDE".into(),
            base: "CRUDE".into(),
            quote: "USD".into(),
            kind: InstrumentKind::TradFiSynthetic,
            tick_size: 0.01,
            step_size: 0.1,
            min_notional: 10.0,
            contract_multiplier: 1_000.0,
            listed_ts_ns: 0,
            delisted_ts_ns: i64::MAX,
        },
    ]
}

/// The pre-provenance schema-1 envelope the legacy decode path reads.
pub fn envelope_v1() -> EnvelopeV1 {
    EnvelopeV1 {
        schema_ver: 1,
        venue: Venue::Bybit,
        symbol: SymbolId(7),
        exch_ts_ns: 99,
        recv_ts_ns: 100,
        stream_seq: 1,
        body: MarketEvent::Trade {
            price: 61_000.5,
            qty: 0.25,
            side: Side::Buy,
            trade_id: 1,
        },
    }
}
