//! Cross-venue gap detector CLI (spec 026, CVG-12). Sibling of mp-audit.
//!
//! Run:
//!   cargo run -p mp-storage --bin mp-cross-venue -- --data-dir data --date 2026-08-04 --config cross_venue.toml
//!   cargo run -p mp-storage --bin mp-cross-venue -- --config cross_venue.toml --check-config
//!   cargo run -p mp-storage --bin mp-cross-venue -- --version
//!
//! Reads ONLY cold Parquet + manifests (CVG-1: no network). Writes a separate
//! `cold/cross_venue/date=…/findings.json` (CVG-8); never edits per-venue
//! manifests (CVG-6) and never relaxes the INT-5 promotion gate (CVG-7).

use mp_storage::{config_hash, detect, parse_config, version_string, write_findings};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("{}", version_string());
        return;
    }
    let config_path = flag(&args, "--config").unwrap_or_else(|| "cross_venue.toml".to_string());
    let toml_str = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read config '{config_path}': {e}");
            std::process::exit(2);
        }
    };
    let cfg = match parse_config(&toml_str) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: config '{config_path}' parse failed: {e}");
            std::process::exit(1);
        }
    };
    if args.iter().any(|a| a == "--check-config") {
        println!(
            "config OK: {config_path} ({} cohort(s), min_cohort={})",
            cfg.symbol_cohorts.len(),
            cfg.min_cohort
        );
        return;
    }
    let data_dir = flag(&args, "--data-dir").unwrap_or_else(|| "data".to_string());
    let date = match flag(&args, "--date") {
        Some(d) => d,
        None => {
            eprintln!("error: --date YYYY-MM-DD is required (or pass --check-config / --version)");
            std::process::exit(2);
        }
    };
    let venue_filter = flag(&args, "--venue");
    let symbol_filter = flag(&args, "--symbol");
    let cold_root = Path::new(&data_dir).join("cold");
    let ver = version_string();
    let chash = config_hash(&toml_str);
    let findings = match detect(&cold_root, &date, &cfg, &ver, &chash) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: detect failed: {e}");
            std::process::exit(1);
        }
    };
    let mut shown = 0usize;
    for f in &findings.findings {
        if let Some(ref v) = venue_filter {
            if f.venue != *v {
                continue;
            }
        }
        if let Some(ref s) = symbol_filter {
            if f.symbol != *s {
                continue;
            }
        }
        shown += 1;
        println!(
            "{:?}  {}  {}/{}  gap#{}  [{}..{})  {}",
            f.classification, date, f.venue, f.symbol, f.gap_index, f.from_ns, f.to_ns, f.evidence
        );
    }
    match write_findings(&cold_root, &findings) {
        Ok(p) => eprintln!(
            "wrote {} ({} findings, {} shown)",
            p.display(),
            findings.findings.len(),
            shown
        ),
        Err(e) => {
            eprintln!("error: write failed: {e}");
            std::process::exit(1);
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}
