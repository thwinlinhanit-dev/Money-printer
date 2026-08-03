//! Data audit CLI (spec 024 INT-3).  Scans raw event logs and produces
//! machine-readable verdicts.  Exit code: 0 = all clean, 1 = findings present.
//!
//! Run:
//!   cargo run -p mp-storage --bin mp-audit -- --data-dir data --venue binance --symbol BTCUSDT
//!   cargo run -p mp-storage --bin mp-audit -- --data-dir data --venue binance --symbol BTCUSDT --date 20260725
//!   cargo run -p mp-storage --bin mp-audit -- --data-dir data  (all logs)

use mp_core::Venue;
use mp_storage::audit::{audit_raw_log, scorecard, AuditConfig, DailyScorecard, RawLogAudit};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = flag(&args, "--data-dir").unwrap_or_else(|| "data".to_string());
    let venue_filter = flag(&args, "--venue");
    let symbol_filter = flag(&args, "--symbol");
    let date_filter = flag(&args, "--date");
    let json_output = args.iter().any(|a| a == "--json");

    let raw_dir = Path::new(&data_dir).join("raw");
    if !raw_dir.is_dir() {
        eprintln!("error: raw directory does not exist: {}", raw_dir.display());
        std::process::exit(2);
    }

    let logs = discover_raw_logs(&raw_dir);
    if logs.is_empty() {
        eprintln!("no raw logs found in {}", raw_dir.display());
        std::process::exit(2);
    }

    let mut all_clean = true;
    // Group by date for scorecards.
    let mut by_date: BTreeMap<String, Vec<(Venue, String, RawLogAudit)>> = BTreeMap::new();

    for log in &logs {
        // Apply filters.
        if let Some(ref v) = venue_filter {
            if log.venue_str != *v {
                continue;
            }
        }
        if let Some(ref s) = symbol_filter {
            if log.symbol != *s {
                continue;
            }
        }
        if let Some(ref d) = date_filter {
            if log.date != *d {
                continue;
            }
        }

        let venue = match Venue::from_slug(&log.venue_str) {
            Some(v) => v,
            None => {
                eprintln!(
                    "SKIP {}: unknown venue '{}'",
                    log.path.display(),
                    log.venue_str
                );
                continue;
            }
        };
        let config = AuditConfig::single(venue, &log.symbol);
        let audit = audit_raw_log(&log.path, &config);

        if json_output {
            let entry = serde_json::json!({
                "file": log.path.display().to_string(),
                "date": log.date,
                "venue": log.venue_str,
                "symbol": log.symbol,
                "clean": audit.is_clean(),
                "audit": audit,
            });
            println!("{}", serde_json::to_string(&entry).unwrap_or_default());
        } else {
            let status = if audit.is_clean() { "CLEAN" } else { "DIRTY" };
            println!(
                "{status:5}  {date}  {venue:>12}/{symbol:<12}  events={events:<8}  coverage={coverage:.4}  findings={findings}  {path}",
                date = log.date,
                venue = log.venue_str,
                symbol = log.symbol,
                events = audit.event_count,
                coverage = audit.coverage,
                findings = audit.findings.len(),
                path = log.path.display(),
            );
            for f in &audit.findings {
                println!("       └─ [{code}] {detail}", code = f.code, detail = f.detail);
            }
        }

        if !audit.is_clean() {
            all_clean = false;
        }

        by_date
            .entry(log.date.clone())
            .or_default()
            .push((venue, log.symbol.clone(), audit));
    }

    // Print daily scorecards.
    if !json_output && by_date.len() > 1 {
        println!("\n--- Daily Scorecards ---");
    }
    let mut scorecards: Vec<DailyScorecard> = Vec::new();
    for (date, entries) in &by_date {
        let card = scorecard(date.as_str(), entries.clone());
        scorecards.push(card.clone());
        if !json_output {
            let mark = if card.promotable { "✅" } else { "❌" };
            println!(
                "{mark} {date}  recordings={count}  promotable={p}",
                count = card.recordings.len(),
                p = card.promotable,
            );
        }
    }

    // Check promotion: 7 consecutive clean days.
    if !json_output && scorecards.len() >= 7 {
        let consecutive = longest_clean_run(&scorecards);
        println!(
            "\n--- Promotion Gate ---\nLongest consecutive clean days: {consecutive}\nRequired: 7\nResult: {}",
            if consecutive >= 7 {
                "✅ PROMOTABLE"
            } else {
                "❌ NOT YET"
            }
        );
    }

    if json_output {
        let summary = serde_json::json!({
            "scorecards": scorecards,
            "longest_clean_run": longest_clean_run(&scorecards),
            "all_clean": all_clean,
        });
        println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
    }

    std::process::exit(if all_clean { 0 } else { 1 });
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Parsed raw log filename: `{date}_{venue}_{symbol}.log`.
struct RawLog {
    path: PathBuf,
    date: String,
    venue_str: String,
    symbol: String,
}

fn discover_raw_logs(raw_dir: &Path) -> Vec<RawLog> {
    let mut logs = Vec::new();
    let Ok(entries) = std::fs::read_dir(raw_dir) else {
        return logs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        // Expected: YYYYMMDD_venue_SYMBOL
        let parts: Vec<&str> = stem.splitn(3, '_').collect();
        if parts.len() != 3 {
            continue;
        }
        // Validate date is 8 digits.
        if parts[0].len() != 8 || !parts[0].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        logs.push(RawLog {
            path,
            date: parts[0].to_string(),
            venue_str: parts[1].to_string(),
            symbol: parts[2].to_string(),
        });
    }
    logs.sort_by(|a, b| a.date.cmp(&b.date).then(a.venue_str.cmp(&b.venue_str)).then(a.symbol.cmp(&b.symbol)));
    logs
}

fn longest_clean_run(scorecards: &[DailyScorecard]) -> usize {
    let mut best = 0usize;
    let mut current = 0usize;
    for card in scorecards {
        if card.promotable {
            current += 1;
            best = best.max(current);
        } else {
            current = 0;
        }
    }
    best
}
