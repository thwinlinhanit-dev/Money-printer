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
        Venue::Deribit => "deribit",
        Venue::Fred => "fred",
        Venue::Ethereum => "ethereum",
        Venue::Cboe => "cboe",
        Venue::DeFiLlama => "defillama",
        Venue::Coinalyze => "coinalyze",
    }
}

/// Stable numeric venue code stored in Parquet (self-contained files).
/// Appended in schema order; old codes are unchanged (CONV-20).
pub fn venue_code(v: Venue) -> u16 {
    match v {
        Venue::BinanceFutures => 1,
        Venue::Bybit => 2,
        Venue::Okx => 3,
        Venue::Hyperliquid => 4,
        Venue::Coinbase => 5,
        Venue::KrakenFutures => 6,
        Venue::Deribit => 7,
        Venue::Fred => 8,
        Venue::Ethereum => 9,
        Venue::Cboe => 10,
        Venue::DeFiLlama => 11,
        Venue::Coinalyze => 12,
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
        7 => Some(Venue::Deribit),
        8 => Some(Venue::Fred),
        9 => Some(Venue::Ethereum),
        10 => Some(Venue::Cboe),
        11 => Some(Venue::DeFiLlama),
        12 => Some(Venue::Coinalyze),
        _ => None,
    }
}

/// The stream directory name for an event body (spec 003 layout). Spec
/// 028/030/031 streams route to their own cold partitions (`positions`,
/// `macro`, `options`) — never mixed into the live tick streams (W-6).
pub fn stream_type_name(body: &MarketEvent) -> &'static str {
    match body {
        MarketEvent::Trade { .. } | MarketEvent::TradeWithAddr { .. } => "trades",
        MarketEvent::BookDelta { .. } => "book_deltas",
        MarketEvent::BookSnapshot { .. } => "book_snapshots",
        MarketEvent::Funding { .. } => "funding",
        MarketEvent::MarkPrice { .. } => "mark_price",
        MarketEvent::OpenInterest { .. } => "open_interest",
        MarketEvent::Liquidation { .. } => "liquidations",
        MarketEvent::IndexPrice { .. } => "index_price",
        MarketEvent::Status { .. } => "status",
        MarketEvent::WhalePosition { .. } => "positions",
        MarketEvent::MacroPoint { .. } => "macro",
        MarketEvent::OptionTrade { .. }
        | MarketEvent::OptionBook { .. }
        | MarketEvent::OptionTicker { .. } => "options",
        MarketEvent::NetflowSnapshot { .. } => "netflow",
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

/// The raw event-log directory that siblings the cold root. The compaction
/// caller (`mp-ops compact`) reads `data/raw/{YYYYMMDD}_{venue}_{symbol}.log`
/// and writes cold Parquet under `data/cold/`, so given a cold root the raw
/// corpus is always the sibling `raw/` directory. Returns `None` when the cold
/// root has no parent (e.g. a bare temp dir in tests) — callers treat that as
/// "no raw logs to verify" (STO-3 prune provenance, C-2 audit 2026-08-28).
pub fn raw_log_dir(cold_root: &Path) -> Option<PathBuf> {
    cold_root.parent().map(|p| p.join("raw"))
}

/// Candidate venue tokens used in raw day-file names
/// (`{YYYYMMDD}_{token}_{symbol}.log`). The primary token matches the
/// `mp-ops compact --venue` spelling (`parse_venue`), which differs from
/// [`venue_slug`] for the futures venues; the slug is kept as a fallback
/// candidate so a differently-spelled but unambiguous day-file is still
/// checked rather than silently skipped.
pub fn raw_file_venue_tokens(v: Venue) -> Vec<&'static str> {
    match v {
        Venue::BinanceFutures => vec!["binance", "binance_futures"],
        Venue::KrakenFutures => vec!["kraken", "kraken_futures"],
        other => vec![venue_slug(other)],
    }
}

/// Streams that own a cold Parquet partition: spec 003 v1 trades + specs
/// 028/030/031 (positions, macro, options). SINGLE source of truth — the
/// compactor (write path) and prune (verify path) both consult this, so a
/// fifth Parquet-backed stream can never be written but forgotten at prune.
/// Manifest-only streams (book_deltas, status, …) return false in v1
/// (spec 003 Decisions).
pub fn has_parquet_partition(stream: &str) -> bool {
    matches!(stream, "trades" | "positions" | "macro" | "options")
}
