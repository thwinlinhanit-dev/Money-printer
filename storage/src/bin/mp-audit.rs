//! Data audit CLI (spec 024 INT-3).  Scans raw event logs and produces
//! machine-readable verdicts.  Exit code: 0 = all clean, 1 = findings present.
//!
//! Run:
//!   cargo run -p mp-storage --bin mp-audit -- --data-dir data --venue binance --symbol BTCUSDT
//!   cargo run -p mp-storage --bin mp-audit -- --data-dir data --venue binance --symbol BTCUSDT --date 20260725
//!   cargo run -p mp-storage --bin mp-audit -- --data-dir data  (all logs)

use mp_core::Venue;
use mp_storage::audit::{
    audit_raw_log, discover_raw_logs, scorecard, AuditConfig, DailyScorecard, RawLogAudit,
};
use mp_storage::promotion::check_promotion;
use std::collections::BTreeMap;
use std::path::Path;

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
                println!(
                    "       └─ [{code}] {detail}",
                    code = f.code,
                    detail = f.detail
                );
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

    // Check promotion: REQUIRED_CONSECUTIVE_CLEAN_DAYS consecutive clean days
    // (lib gate `check_promotion` — the same logic that gates promotion reads).
    if !json_output && !scorecards.is_empty() {
        let verdict = check_promotion(&scorecards);
        println!("\n--- Promotion Gate ---");
        println!(
            "Consecutive clean days: {} (required {})",
            verdict.consecutive_clean, verdict.required
        );
        if verdict.promoted {
            println!(
                "Result: ✅ PROMOTABLE  window {}..{}",
                verdict.window_start.as_deref().unwrap_or("?"),
                verdict.window_end.as_deref().unwrap_or("?")
            );
        } else {
            let why = verdict
                .first_failure
                .map(|date| format!("first break {date}"))
                .unwrap_or_else(|| "no qualifying clean window yet".to_string());
            println!("Result: ❌ NOT YET ({why})");
        }
    }

    if json_output {
        let verdict = check_promotion(&scorecards);
        let summary = serde_json::json!({
            "scorecards": scorecards,
            "promotion": verdict,
            "all_clean": all_clean,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
    }

    std::process::exit(if all_clean { 0 } else { 1 });
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}
