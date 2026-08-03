//! Legacy raw-log migration CLI (2026-08-03 audit fix).  Rewrites schema-1
//! event logs (the 07-18..07-29 capture) into current-schema files under a
//! separate output directory, preserving the `{date}_{venue}_{symbol}.log`
//! naming so `mp-audit --data-dir <out>` can audit the migrated copies.
//!
//! W-6: originals are never modified or deleted — this writes NEW verified
//! files; only the human deletes the originals after verification.
//!
//! Run:
//!   cargo run -p mp-storage --bin mp-migrate -- --data-dir data --out-dir data-migrated
//!   cargo run -p mp-storage --bin mp-migrate -- --data-dir data --out-dir data-migrated --date 20260719
//!   cargo run -p mp-storage --bin mp-migrate -- --dry-run   (print what would migrate)

use mp_storage::audit::discover_raw_logs;
use mp_storage::{migrate_log, MigrateOutcome};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = flag(&args, "--data-dir").unwrap_or_else(|| "data".to_string());
    let out_dir = flag(&args, "--out-dir").unwrap_or_else(|| format!("{data_dir}-migrated"));
    let venue_filter = flag(&args, "--venue");
    let symbol_filter = flag(&args, "--symbol");
    let date_filter = flag(&args, "--date");
    let dry_run = args.iter().any(|a| a == "--dry-run");

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

    let out_raw = PathBuf::from(&out_dir).join("raw");
    let mut migrated = 0u64;
    let mut already = 0u64;
    let mut failed = 0u64;

    for log in &logs {
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

        let fname = log
            .path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dst = out_raw.join(&fname);

        let status = if dry_run {
            "WOULD-MIGRATE".to_string()
        } else {
            match migrate_log(&log.path, &dst) {
                Ok(MigrateOutcome::Migrated {
                    events,
                    legacy_events,
                }) => {
                    migrated += 1;
                    format!("MIGRATED events={events} legacy={legacy_events}")
                }
                Ok(MigrateOutcome::AlreadyCurrent) => {
                    already += 1;
                    "ALREADY-CURRENT".to_string()
                }
                Err(e) => {
                    failed += 1;
                    format!("ERROR {e}")
                }
            }
        };
        println!(
            "{status:42}  {date}  {venue:>12}/{symbol:<12}  {path}",
            date = log.date,
            venue = log.venue_str,
            symbol = log.symbol,
            path = log.path.display(),
        );
    }

    println!(
        "\nsummary: migrated={migrated} already_current={already} failed={failed} out={}",
        out_raw.display()
    );
    std::process::exit(if failed > 0 { 1 } else { 0 });
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}
