//! `mp-ops` — operational CLI for the Money Printer system (SPEC-011).
//!
//! Subcommands:
//!   compact --date YYYY-MM-DD --venue bybit --symbol BTCUSDT
//!           Compacts a raw event log into partitioned Parquet files.
//!           Date also accepts YYYYMMDD (backward compatible).
//!
//! Usage:
//!   cargo run --package mp-ops --bin mp-ops -- compact --date 2026-07-15 --venue bybit --symbol BTCUSDT

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, SymbolTable, Venue};
use mp_storage::{audit_raw_log, compactor, scorecard, AuditConfig, RawLogAudit};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const COMPACTOR_VERSION: &str = "0.1.0";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("missing {name}"))
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter_map(|pair| (pair[0] == name).then(|| pair[1].clone()))
        .collect()
}

fn parse_venue(s: &str) -> Result<Venue, String> {
    match s {
        "bybit" => Ok(Venue::Bybit),
        "binance" => Ok(Venue::BinanceFutures),
        "okx" => Ok(Venue::Okx),
        "hyperliquid" => Ok(Venue::Hyperliquid),
        "coinbase" => Ok(Venue::Coinbase),
        "kraken" | "kraken_futures" => Ok(Venue::KrakenFutures),
        _ => Err(format!("unknown venue: {s}")),
    }
}

fn date_to_nanos(date: &str) -> Result<(i64, i64), String> {
    // Accept both YYYYMMDD and YYYY-MM-DD.
    let norm = date.replace('-', "");
    if norm.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let y: i64 = norm[0..4].parse().map_err(|_| "bad year")?;
    let m: u32 = norm[4..6].parse().map_err(|_| "bad month")?;
    let d: u32 = norm[6..8].parse().map_err(|_| "bad day")?;

    // Days since Unix epoch for this date
    let days = days_from_epoch(y, m, d);
    let day_start_ns = (days as i64) * 86_400_000_000_000;
    let day_end_ns = day_start_ns + 86_400_000_000_000;
    Ok((day_start_ns, day_end_ns))
}

fn days_from_epoch(y: i64, m: u32, d: u32) -> u64 {
    // Count days from 1970-01-01
    let mut total = 0u64;
    for year in 1970..y {
        total += if is_leap(year) { 366 } else { 365 };
    }
    let months = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for month in 0..(m as usize - 1) {
        total += months[month] as u64;
    }
    total += (d - 1) as u64;
    total
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn compute_source_hash(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let hash = crc32fast::hash(&data);
    Ok(format!("{:08x}", hash))
}

fn read_log_with_symbols(path: &Path) -> Result<(Vec<EventEnvelope>, SymbolTable), String> {
    let mut reader = LogReader::open(path).map_err(|e| format!("open log {path:?}: {e}"))?;
    let mut events = Vec::new();
    for ev in &mut reader {
        events.push(ev.map_err(|e| format!("read log {path:?}: {e}"))?);
    }
    let metas = reader.symbols().to_vec();
    let symbols = SymbolTable::from_metas(metas);
    Ok((events, symbols))
}

fn audit_config(args: &[String], venue: Venue, symbol: &str) -> Result<AuditConfig, String> {
    let mut config = AuditConfig::single(venue, symbol);
    if let Some(seconds) = flag(args, "--max-gap-sec") {
        let seconds: i64 = seconds
            .parse()
            .map_err(|_| "--max-gap-sec must be an integer")?;
        if seconds <= 0 {
            return Err("--max-gap-sec must be positive".into());
        }
        config.max_gap_ns = seconds.saturating_mul(1_000_000_000);
    }
    config.required_streams = flags(args, "--require-stream")
        .into_iter()
        .collect::<BTreeSet<_>>();
    Ok(config)
}

fn clean_audit(path: &Path, config: &AuditConfig) -> Result<RawLogAudit, String> {
    let audit = audit_raw_log(path, config);
    if audit.is_clean() {
        Ok(audit)
    } else {
        let reasons = audit
            .findings
            .iter()
            .map(|finding| format!("{}: {}", finding.code, finding.detail))
            .collect::<Vec<_>>()
            .join("; ");
        Err(format!("quarantined raw log {}: {reasons}", path.display()))
    }
}

fn cmd_compact(args: &[String]) -> Result<String, String> {
    let date_raw = need(args, "--date")?;
    // Normalize: strip dashes for raw log filename, build dashed for partition paths.
    let date_flat = date_raw.replace('-', "");
    if date_flat.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let date_dashed = format!(
        "{}-{}-{}",
        &date_flat[0..4],
        &date_flat[4..6],
        &date_flat[6..8]
    );
    let venue_str = need(args, "--venue")?;
    let symbol = need(args, "--symbol")?;
    let venue = parse_venue(&venue_str)?;

    let raw_path = Path::new("data")
        .join("raw")
        .join(format!("{date_flat}_{venue_str}_{symbol}.log"));
    let cold_root = Path::new("data").join("cold");

    if !raw_path.exists() {
        return Err(format!("raw log not found: {}", raw_path.display()));
    }

    // INT-4: no compaction is allowed to turn contaminated raw data into a
    // trustworthy-looking cold dataset.  The audit runs before reading events.
    let config = audit_config(args, venue, &symbol)?;
    clean_audit(&raw_path, &config)?;

    tracing::info!(path = %raw_path.display(), "compacting raw log");

    let (day_start_ns, day_end_ns) = date_to_nanos(&date_flat)?;
    let source_hash = compute_source_hash(&raw_path)?;
    let (events, symbols) = read_log_with_symbols(&raw_path)?;

    let created_ts_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64;

    std::fs::create_dir_all(&cold_root).map_err(|e| format!("create {cold_root:?}: {e}"))?;

    let stats = compactor::compact_day(
        &cold_root,
        venue,
        &date_dashed,
        day_start_ns,
        day_end_ns,
        events,
        &symbols,
        &source_hash,
        COMPACTOR_VERSION,
        created_ts_ns,
    )
    .map_err(|e| format!("compact_day failed: {e}"))?;

    Ok(format!(
        "compact done: {} trade files written ({} rows), {} files skipped",
        stats.trades_files_written, stats.trade_rows, stats.trades_files_skipped
    ))
}

fn cmd_audit(args: &[String]) -> Result<String, String> {
    let date = need(args, "--date")?.replace('-', "");
    if date.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let venue_name = need(args, "--venue")?;
    let symbol = need(args, "--symbol")?;
    let venue = parse_venue(&venue_name)?;
    let path = Path::new("data")
        .join("raw")
        .join(format!("{date}_{venue_name}_{symbol}.log"));
    let audit = audit_raw_log(&path, &audit_config(args, venue, &symbol)?);
    serde_json::to_string_pretty(&audit).map_err(|error| error.to_string())
}

fn cmd_scorecard(args: &[String]) -> Result<String, String> {
    let date = need(args, "--date")?.replace('-', "");
    if date.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let required = flags(args, "--required");
    if required.is_empty() {
        return Err("scorecard requires one or more --required venue:symbol entries".into());
    }
    let mut entries = Vec::new();
    for item in required {
        let (venue_name, symbol) = item
            .split_once(':')
            .ok_or_else(|| format!("invalid --required {item}; expected venue:symbol"))?;
        let venue = parse_venue(venue_name)?;
        let path = Path::new("data")
            .join("raw")
            .join(format!("{date}_{venue_name}_{symbol}.log"));
        let audit = audit_raw_log(&path, &audit_config(args, venue, symbol)?);
        entries.push((venue, symbol.to_owned(), audit));
    }
    let dashed = format!("{}-{}-{}", &date[0..4], &date[4..6], &date[6..8]);
    serde_json::to_string_pretty(&scorecard(dashed, entries)).map_err(|error| error.to_string())
}

fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: mp-ops <subcommand> [options]");
        eprintln!("Subcommands: compact, audit, scorecard");
        return ExitCode::FAILURE;
    }

    let result = match args[1].as_str() {
        "compact" => cmd_compact(&args[2..]),
        "audit" => cmd_audit(&args[2..]),
        "scorecard" => cmd_scorecard(&args[2..]),
        other => Err(format!("unknown subcommand: {other}")),
    };

    match result {
        Ok(msg) => {
            tracing::info!("{msg}");
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::log::EventLogWriter;
    use mp_core::{
        EventEnvelope, EventProvenance, InstrumentKind, MarketEvent, Side, SnapshotSource,
        SymbolId, SymbolMeta,
    };

    #[test]
    fn int_4_compaction_refuses_quarantined_log() {
        let path = std::env::temp_dir().join(format!("mp-int-compact-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (mut writer, _) = EventLogWriter::open(&path).unwrap();
        writer
            .write_symbols(&[SymbolMeta::new(
                SymbolId(0),
                Venue::BinanceFutures,
                "BTCUSDT",
                "BTC",
                "USDT",
                InstrumentKind::Perp,
                0.1,
                0.1,
                1.0,
            )])
            .unwrap();
        let event = EventEnvelope::new(
            Venue::Hyperliquid,
            SymbolId(0),
            1,
            1,
            1,
            MarketEvent::Trade {
                price: 1.0,
                qty: 1.0,
                side: Side::Buy,
                trade_id: 1,
            },
        )
        .with_provenance(EventProvenance {
            stream: "trade".into(),
            subscription: "x".into(),
            connection_id: 1,
            snapshot_source: SnapshotSource::None,
        });
        writer.append(&event).unwrap();
        writer.sync().unwrap();
        let err = clean_audit(
            &path,
            &AuditConfig::single(Venue::BinanceFutures, "BTCUSDT"),
        )
        .unwrap_err();
        assert!(err.contains("quarantined raw log"));
        let _ = std::fs::remove_file(path);
    }
}
