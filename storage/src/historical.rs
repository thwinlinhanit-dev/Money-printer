//! Historical bootstrap core (spec 027, HBS-2..HBS-10). Transport-agnostic: a
//! [`HistoricalSource`] yields a day's Binance aggTrades CSV text; the core
//! parses it into canonical Trade events and writes them to a SEPARATE
//! `cold/historical/` namespace — never `data/raw/` or `cold/trades/` (HBS-2,
//! W-6) — labeled `source: external_archive`, `fidelity: aggregated` (HBS-3).
//!
//! Deterministic (HBS-5) and idempotent (HBS-4, content-hash). The core itself
//! has NO network dependency. The live Binance-archive auto-download lives in
//! [`crate::historical_download`] behind the `live-http` feature (HBS-1/HBS-8,
//! owner-approved 2026-08-05, CLAUDE.md); enabled builds combine it with this
//! core via the shared [`HistoricalConfig`] download fields. Offline drives
//! `MockHistoricalSource` (tests) / `FileHistoricalSource` (local CSV).

use crate::{layout, parquet_trades, StorageError};
use mp_core::{
    EventEnvelope, InstrumentKind, MarketEvent, Side, SymbolId, SymbolMeta, SymbolTable, Venue,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where a day of historical CSV text comes from. The core only ever sees text.
pub trait HistoricalSource {
    fn fetch_day(&self, symbol: &str, date: &str) -> Result<String, StorageError>;
}

/// Test/dev source returning fixed text regardless of symbol/date.
pub struct MockHistoricalSource {
    pub text: String,
}
impl HistoricalSource for MockHistoricalSource {
    fn fetch_day(&self, _symbol: &str, _date: &str) -> Result<String, StorageError> {
        Ok(self.text.clone())
    }
}

/// Local-file source: read a manually-downloaded aggTrades CSV. No network —
/// the fully-implementable path until the live source is approved.
pub struct FileHistoricalSource {
    pub path: PathBuf,
}
impl HistoricalSource for FileHistoricalSource {
    fn fetch_day(&self, _symbol: &str, _date: &str) -> Result<String, StorageError> {
        Ok(std::fs::read_to_string(&self.path)?)
    }
}

/// Historical bootstrap config (HBS-9, CONV-16). `deny_unknown_fields` so a
/// typo errors instead of silently defaulting.
///
/// The `download_*` fields tune the LIVE archive path (HBS-1/HBS-8) and are
/// inert to the offline core (they plug into
/// [`crate::historical_download::BinanceVisionSource`]). `--check-config`
/// accepts them regardless of the `live-http` feature so the SAME
/// `historical_bootstrap.toml` is valid for both build modes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalConfig {
    /// `aggregated` (default): aggTrades are bucketed ⇒ backtests on this data
    /// are upper-bound on maker fills (SIM-2 trade-print rule).
    #[serde(default = "default_fidelity")]
    pub fidelity: String,
    /// HBS-8: max archive downloads per second (default 1 req/s).
    #[serde(default = "default_rate_per_sec")]
    pub download_rate_per_sec: f64,
    /// HBS-8: retries for a transient (5xx/transport) HTTP failure; non-2xx
    /// goes straight through to the error path.
    #[serde(default = "default_max_retries")]
    pub download_max_retries: u32,
    /// HBS-8: exponential-backoff floor for the first retry, ms (jittered,
    /// deterministic given the retry counter).
    #[serde(default = "default_backoff_base_ms")]
    pub download_backoff_base_ms: u64,
    /// HBS-8: backoff ceiling, ms.
    #[serde(default = "default_backoff_cap_ms")]
    pub download_backoff_cap_ms: u64,
    /// HBS-1: archive base URL (override for mirrors / local test fixtures;
    /// default is Binance's public no-auth archive).
    #[serde(default = "default_base_url")]
    pub download_base_url: String,
    /// HBS-1: data path inside the archive (`futures/um` for USDⓈ-M perps —
    /// matches the cold/historical writer's `Venue::BinanceFutures`).
    #[serde(default = "default_data_class")]
    pub download_data_class: String,
}

fn default_fidelity() -> String {
    "aggregated".into()
}
fn default_rate_per_sec() -> f64 {
    1.0
}
fn default_max_retries() -> u32 {
    3
}
fn default_backoff_base_ms() -> u64 {
    1000
}
fn default_backoff_cap_ms() -> u64 {
    30_000
}
fn default_base_url() -> String {
    "https://data.binance.vision".into()
}
fn default_data_class() -> String {
    "futures/um".into()
}

impl Default for HistoricalConfig {
    fn default() -> Self {
        Self {
            fidelity: default_fidelity(),
            download_rate_per_sec: default_rate_per_sec(),
            download_max_retries: default_max_retries(),
            download_backoff_base_ms: default_backoff_base_ms(),
            download_backoff_cap_ms: default_backoff_cap_ms(),
            download_base_url: default_base_url(),
            download_data_class: default_data_class(),
        }
    }
}

/// Parse historical config TOML (HBS-9).
pub fn parse_historical_config(toml: &str) -> Result<HistoricalConfig, toml::de::Error> {
    toml::from_str(toml)
}

/// Binance aggTrades CSV → canonical Trade events (HBS-6). Columns (positional,
/// per data.binance.vision daily aggTrades):
///   [0] price, [1] qty, [2] quote_qty, [3] time_ms, [4] is_buyer_maker,
///   [5] is_best_match (ignored if present)
/// `is_buyer_maker=true` ⇒ the buyer is the maker ⇒ the aggressor is SELL
/// (mirrors `collectors::binance` REST aggTrades `m`, COL-25..27). The row
/// index doubles as `stream_seq` and `trade_id` (the daily CSV omits the agg
/// id). Deterministic: same text ⇒ same rows in the same order.
pub fn parse_aggtrades(text: &str, symbol: SymbolId) -> Result<Vec<EventEnvelope>, StorageError> {
    let mut out = Vec::new();
    for (row, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        if cols.len() < 5 {
            tracing::warn!(
                row,
                kind = "hbs.csv",
                "malformed aggTrades row skipped (CONV-15)"
            );
            continue;
        }
        let Ok(price) = cols[0].parse::<f64>() else {
            continue; // header row / non-numeric col0 (expected) ⇒ skip
        };
        let Ok(qty) = cols[1].parse::<f64>() else {
            tracing::warn!(row, kind = "hbs.csv", "bad qty skipped (CONV-15)");
            continue;
        };
        let Ok(time_ms) = cols[3].parse::<i64>() else {
            tracing::warn!(row, kind = "hbs.csv", "bad time_ms skipped (CONV-15)");
            continue;
        };
        let is_buyer_maker = cols[4].eq_ignore_ascii_case("true");
        let side = if is_buyer_maker {
            Side::Sell
        } else {
            Side::Buy
        };
        let ts_ns = time_ms * 1_000_000;
        out.push(EventEnvelope::new(
            Venue::BinanceFutures,
            symbol,
            ts_ns,      // exch_ts_ns
            ts_ns, // recv_ts_ns: the archive has no local receive clock; replay ⇒ exch (honest)
            row as u64, // stream_seq (deterministic row order)
            MarketEvent::Trade {
                price,
                qty,
                side,
                trade_id: row as u64,
            },
        ));
    }
    Ok(out)
}
/// Outcome of one day's bootstrap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootstrapStats {
    pub files_written: u64,
    pub files_skipped: u64,
    pub rows_written: u64,
}

/// `cold/historical/trades/venue=binance_futures/symbol={s}/date={d}/part-000.parquet`
pub fn historical_trades_file(root: &Path, symbol: &str, date: &str) -> PathBuf {
    layout::partition_file(
        &root.join("historical"),
        "trades",
        Venue::BinanceFutures,
        symbol,
        date,
    )
}

/// `cold/historical/manifests/symbol={s}/{date}.json`
pub fn historical_manifest_file(root: &Path, symbol: &str, date: &str) -> PathBuf {
    root.join("historical")
        .join("manifests")
        .join(format!("symbol={symbol}"))
        .join(format!("{date}.json"))
}

#[derive(Debug, Clone, Serialize)]
struct HistoricalManifest {
    schema_ver: u16,
    source: &'static str, // "external_archive" — NOT the live recorder
    fidelity: String,
    symbol: String,
    date: String,
    rows: u64,
    source_hash: String,
}

/// Bootstrap one (symbol, date) into `cold/historical/` (HBS-2..5, HBS-7).
/// Deterministic and idempotent: same source text ⇒ same Parquet; a re-run with
/// the same content-hash skips the Parquet rewrite (HBS-4) but rewrites the
/// (identical) manifest so the label/date inventory is always present.
pub fn bootstrap_day(
    root: &Path,
    source: &dyn HistoricalSource,
    symbol: &str,
    date: &str,
    compactor_version: &str,
    cfg: &HistoricalConfig,
) -> Result<BootstrapStats, StorageError> {
    let text = source.fetch_day(symbol, date)?;
    let hash = format!("{:016x}", mp_core::fnv1a_64_str(&text));
    let mut table = SymbolTable::new();
    let sym = table.intern(Venue::BinanceFutures, symbol, |id| {
        SymbolMeta::new(
            id,
            Venue::BinanceFutures,
            symbol,
            "",
            "",
            InstrumentKind::Perp,
            f64::NAN,
            f64::NAN,
            f64::NAN,
        )
    });
    let mut events = parse_aggtrades(&text, sym)?;
    // Compactor contract: recv-sorted before write (hist. recv == exch).
    events.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
    let rows = events.len() as u64;
    let dest = historical_trades_file(root, symbol, date);
    let skip = dest.exists()
        && matches!(parquet_trades::read_source_hash(&dest), Ok(Some(h)) if h == hash);
    let mut stats = BootstrapStats::default();
    if skip {
        stats.files_skipped = 1;
    } else {
        parquet_trades::write_trades(&dest, &events, compactor_version, &hash)?;
        stats.files_written = 1;
    }
    stats.rows_written += rows;

    // HBS-3: label source + fidelity + schema_ver on a separate manifest.
    let manifest = HistoricalManifest {
        schema_ver: mp_core::SCHEMA_VER,
        source: "external_archive",
        fidelity: cfg.fidelity.clone(),
        symbol: symbol.to_owned(),
        date: date.to_owned(),
        rows,
        source_hash: hash,
    };
    let mpath = historical_manifest_file(root, symbol, date);
    if let Some(dir) = mpath.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // SAFETY: HistoricalManifest is derived-Serialize over plain data (CONV-13).
    let json = serde_json::to_string_pretty(&manifest).expect("manifest serializes");
    std::fs::write(&mpath, json)?;
    Ok(stats)
}

/// HBS-7 per-date completion marker: the (symbol, date) is fully bootstrapped
/// iff BOTH the trades Parquet and the labeled manifest exist. The live path
/// (mp-bootstrap date range) consults this BEFORE fetching, so a completed day
/// costs zero network requests (resumable ranges, HBS-4/7).
pub fn day_complete(root: &Path, symbol: &str, date: &str) -> bool {
    historical_trades_file(root, symbol, date).exists()
        && historical_manifest_file(root, symbol, date).exists()
}
