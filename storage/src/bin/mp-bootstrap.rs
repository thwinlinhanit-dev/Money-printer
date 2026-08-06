//! Historical bootstrap CLI (spec 027). Sibling of mp-audit / mp-migrate.
//! Bootstraps Binance aggTrades CSV data into the SEPARATE `cold/historical/`
//! namespace (HBS-2, W-6) with `source: external_archive` labels (HBS-3).
//!
//! Two source modes:
//! - `--input <aggTrades.csv>` — bootstraps a locally-saved CSV (no network;
//!   works without the `live-http` feature).
//! - default — auto-download from data.binance.vision (HBS-1/HBS-8;
//!   owner-approved 2026-08-05). Build with `--features live-http` and give a
//!   date range, e.g. `--symbol BTCUSDT --start-date 2026-01-01
//!   --end-date 2026-01-31`. Dates iterate inclusively and completed days are
//!   skipped before any network fetch (HBS-4/7 completion marker ⇒ resumable).
//!
//! Run:
//!   cargo run -p mp-storage --bin mp-bootstrap -- --data-dir data --symbol BTCUSDT --date 2026-08-01 --input btc_aggTrades_20260801.csv --config historical_bootstrap.toml
//!   cargo run -p mp-storage --bin mp-bootstrap -- --config historical_bootstrap.toml --check-config
//!   cargo run -p mp-storage --bin mp-bootstrap -- --version

use mp_storage::{
    app_version, bootstrap_day, day_complete, parse_historical_config, FileHistoricalSource,
    HistoricalConfig, HistoricalSource,
};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("{}", app_version("mp-bootstrap"));
        return;
    }
    let config_path =
        flag(&args, "--config").unwrap_or_else(|| "historical_bootstrap.toml".to_string());
    let cfg: HistoricalConfig = match load_config(&config_path) {
        Ok(c) => c,
        Err(code) => std::process::exit(code),
    };
    if args.iter().any(|a| a == "--check-config") {
        println!(
            "config OK: {config_path} (fidelity={}, rate={} req/s, retries={}, base={}, class={})",
            cfg.fidelity,
            cfg.download_rate_per_sec,
            cfg.download_max_retries,
            cfg.download_base_url,
            cfg.download_data_class,
        );
        return;
    }
    let data_dir = flag(&args, "--data-dir").unwrap_or_else(|| "data".to_string());
    let symbol = match flag(&args, "--symbol") {
        Some(s) => s,
        None => {
            eprintln!("error: --symbol required");
            std::process::exit(2);
        }
    };
    let dates = match date_range(&args) {
        Ok(d) => d,
        Err(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(2);
        }
    };
    let cold_root = Path::new(&data_dir).join("cold");
    let ver = app_version("mp-bootstrap");
    let source: Box<dyn HistoricalSource> = match flag(&args, "--input") {
        Some(p) => Box::new(FileHistoricalSource { path: p.into() }),
        None => live_source(&cfg),
    };
    let mut written = 0u64;
    let mut skipped = 0u64;
    let mut rows = 0u64;
    for date in &dates {
        if day_complete(&cold_root, &symbol, date) {
            // HBS-7 completion marker: already bootstrapped (parquet +
            // manifest both present) ⇒ zero network, zero work.
            skipped += 1;
            println!("{symbol} {date}: already complete — skipped (HBS-4/7)");
            continue;
        }
        match bootstrap_day(&cold_root, source.as_ref(), &symbol, date, &ver, &cfg) {
            Ok(stats) => {
                written += stats.files_written;
                skipped += stats.files_skipped;
                rows += stats.rows_written;
                println!(
                    "bootstrap {symbol} {date}: written={} rows={} (fidelity={})",
                    stats.files_written, stats.rows_written, cfg.fidelity
                );
            }
            Err(e) => {
                eprintln!("error: bootstrap {symbol} {date} failed: {e}");
                std::process::exit(1);
            }
        }
    }
    println!(
        "done: {symbol} {}..{} — files_written={written} days_skipped={skipped} rows={rows}",
        dates[0],
        dates[dates.len() - 1]
    );
}

fn load_config(config_path: &str) -> Result<HistoricalConfig, i32> {
    let toml_str = match std::fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read config '{config_path}': {e}");
            return Err(2);
        }
    };
    match parse_historical_config(&toml_str) {
        Ok(c) => Ok(c),
        Err(e) => {
            eprintln!("error: config '{config_path}' parse failed: {e}");
            Err(1)
        }
    }
}

/// Live downloader (HBS-1). Compiled only when the `live-http` feature is on;
/// the offline build says so loudly instead of silently downloading nothing.
#[cfg(feature = "live-http")]
fn live_source(cfg: &HistoricalConfig) -> Box<dyn HistoricalSource> {
    Box::new(mp_storage::BinanceVisionSource::new(cfg))
}

#[cfg(not(feature = "live-http"))]
fn live_source(_cfg: &HistoricalConfig) -> Box<dyn HistoricalSource> {
    eprintln!(
        "error: the Binance-archive auto-download (HBS-1) needs the `live-http` feature: \
         rebuild/run with `--features live-http` (owner-approved 2026-08-05); \
         or pass `--input <aggTrades.csv>` for the offline path"
    );
    std::process::exit(2);
}

/// `--date D` (single day) OR `--start-date A --end-date B` (inclusive range,
/// HBS-7). Both parse with strict ISO-8601 validation (no chrono dependency;
/// civil-calendar arithmetic).
fn date_range(args: &[String]) -> Result<Vec<String>, String> {
    let single = flag(args, "--date");
    let start = flag(args, "--start-date");
    let end = flag(args, "--end-date");
    let (start, end) = match (single, start, end) {
        (Some(d), None, None) => (d.clone(), d),
        (None, Some(s), Some(e)) => (s, e),
        (None, Some(_), None) => return Err("--start-date needs --end-date".into()),
        (None, None, Some(_)) => return Err("--end-date needs --start-date".into()),
        (Some(_), _, _) => {
            return Err("use either --date OR --start-date/--end-date, not both".into())
        }
        (None, None, None) => {
            return Err("--date (single) or --start-date/--end-date (range) required".into())
        }
    };
    let start_day =
        parse_iso_date(&start).ok_or_else(|| format!("bad start-date '{start}' (YYYY-MM-DD)"))?;
    let end_day =
        parse_iso_date(&end).ok_or_else(|| format!("bad end-date '{end}' (YYYY-MM-DD)"))?;
    if start_day > end_day {
        return Err(format!("start-date {start} is after end-date {end}"));
    }
    Ok((start_day..=end_day).map(format_iso_date).collect())
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn parse_iso_date(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y = it.next()?.parse::<i64>().ok()?;
    let m = it.next()?.parse::<i64>().ok()?;
    let d = it.next()?.parse::<i64>().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let day = days_from_civil(y, m, d);
    let (cy, cm, cd) = civil_from_days(day);
    (cy == y && cm == m && cd == d).then_some(day) // rejects e.g. 2026-02-30
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * if m > 2 { m - 3 } else { m + 9 } + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn format_iso_date(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}
