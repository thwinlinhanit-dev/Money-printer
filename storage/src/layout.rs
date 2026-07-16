//! Cold-store layout: partition paths, stream type names, venue codes
//! (spec 003 Design). Column/partition names match spec 001 field names.

use mp_core::{MarketEvent, Venue};
use std::path::{Path, PathBuf};

/// Stable lowercase venue slug used in partition paths.
pub fn venue_slug(v: Venue) -> &'static str {
    match v {
        Venue::BinanceFutures => "binance_futures",
        Venue::Bybit => "bybit",
        Venue::Okx => "okx",
        Venue::Hyperliquid => "hyperliquid",
        Venue::Coinbase => "coinbase",
        Venue::KrakenFutures => "kraken_futures",
    }
}

/// Stable numeric venue code stored in Parquet (self-contained files).
pub fn venue_code(v: Venue) -> u16 {
    match v {
        Venue::BinanceFutures => 1,
        Venue::Bybit => 2,
        Venue::Okx => 3,
        Venue::Hyperliquid => 4,
        Venue::Coinbase => 5,
        Venue::KrakenFutures => 6,
    }
}

/// Inverse of [`venue_code`].
pub fn venue_from_code(code: u16) -> Option<Venue> {
    match code {
        1 => Some(Venue::BinanceFutures),
        2 => Some(Venue::Bybit),
        3 => Some(Venue::Okx),
        4 => Some(Venue::Hyperliquid),
        5 => Some(Venue::Coinbase),
        6 => Some(Venue::KrakenFutures),
        _ => None,
    }
}

/// The stream directory name for an event body (spec 003 layout).
pub fn stream_type_name(body: &MarketEvent) -> &'static str {
    match body {
        MarketEvent::Trade { .. } => "trades",
        MarketEvent::BookDelta { .. } => "book_deltas",
        MarketEvent::BookSnapshot { .. } => "book_snapshots",
        MarketEvent::Funding { .. } => "funding",
        MarketEvent::MarkPrice { .. } => "mark_price",
        MarketEvent::OpenInterest { .. } => "open_interest",
        MarketEvent::Liquidation { .. } => "liquidations",
        MarketEvent::IndexPrice { .. } => "index_price",
        MarketEvent::Status { .. } => "status",
    }
}

/// `cold/{stream}/venue={v}/symbol={s}/date={d}/part-000.parquet`.
pub fn partition_file(
    root: &Path,
    stream: &str,
    venue: Venue,
    symbol: &str,
    date: &str,
) -> PathBuf {
    root.join(stream)
        .join(format!("venue={}", venue_slug(venue)))
        .join(format!("symbol={symbol}"))
        .join(format!("date={date}"))
        .join("part-000.parquet")
}

/// `cold/manifests/venue={v}/date={d}.json`.
pub fn manifest_file(root: &Path, venue: Venue, date: &str) -> PathBuf {
    root.join("manifests")
        .join(format!("venue={}", venue_slug(venue)))
        .join(format!("date={date}.json"))
}
