//! `cohort_score` — the weekly wallet-cohort scoring run (spec 042 WCG-9/10).
//!
//! Replays recorded raw logs (the spec 028 `*_hyperliquid_positions.log`
//! census among them) through [`WalletScorer`](mp_features::WalletScorer),
//! classifies every address as of the latest observation, atomically writes
//! the snapshot `data/cohorts/{UTC date}.json`, and appends the cohort diff
//! to `journal/cohort_changes.jsonl`. Idempotent (WCG-10): re-running on the
//! same data reproduces a byte-identical snapshot.
//!
//! Usage:
//!   cohort_score --log <raw event log> [--log <...>]
//!                [--out-dir data/cohorts] [--journal journal/cohort_changes.jsonl]
//!                [--config features.toml] [--as-of-ns <ns>] [--json]
//!
//! Pure offline replay (PD-3/WCG-1): no wall clock, no network. `now_ns` is
//! the maximum recv_ts_ns observed unless `--as-of-ns` overrides it.

use mp_core::log::LogReader;
use mp_features::config::FeaturesConfig;
use mp_features::{Cohort, WalletMetrics, WalletScorer};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn values(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
        }
        i += 1;
    }
    out
}

/// UTC calendar date from epoch-ns (no chrono dependency; Hinnant's civil
/// algorithm). Deterministic, pure.
fn date_str_from_ns(ns: i64) -> String {
    let days = ns.div_euclid(86_400_000_000_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") || values(&args, "--log").is_empty() {
        eprintln!(
            "usage: cohort_score --log <file> [--log <file>...] \
             [--out-dir DIR] [--journal FILE] [--config FILE] [--as-of-ns NS] [--json]"
        );
        return if values(&args, "--log").is_empty() {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        };
    }

    let logs: Vec<PathBuf> = values(&args, "--log")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let out_dir = flag(&args, "--out-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/cohorts"));
    let journal = flag(&args, "--journal")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("journal/cohort_changes.jsonl"));

    // Config is optional: absent → spec defaults (WCG defaults), so the
    // offline grade equals the live model's grade (one-code-path, FEA-4).
    let cfg_inner = flag(&args, "--config").map(|p| -> Result<_, String> {
        let text = std::fs::read_to_string(&p).map_err(|e| format!("read {p}: {e}"))?;
        let fc: FeaturesConfig = toml::from_str(&text).map_err(|e| format!("parse {p}: {e}"))?;
        Ok(fc.cohort.inner)
    });
    let cohort_cfg = match cfg_inner {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            eprintln!("cohort_score: {e}");
            return ExitCode::FAILURE;
        }
        None => mp_features::CohortConfig::default(),
    };

    // Replay: feed EVERY event; the scorer consumes only WhalePosition bodies
    // and fails closed on non-finite frames (WCG-1/3).
    let mut scorer = WalletScorer::new();
    let mut events: usize = 0;
    let mut positions: usize = 0;
    let mut now_ns: Option<i64> = None;
    for path in &logs {
        let mut reader = match LogReader::open(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("cohort_score: open {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        };
        for ev in reader.by_ref() {
            match ev {
                Ok(envelope) => {
                    now_ns =
                        Some(now_ns.map_or(envelope.recv_ts_ns, |m| m.max(envelope.recv_ts_ns)));
                    events += 1;
                    if scorer.feed(&envelope) {
                        positions += 1;
                    }
                }
                Err(e) => {
                    eprintln!("cohort_score: read {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    let as_of_ns: i64 = match flag(&args, "--as-of-ns") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("cohort_score: --as-of-ns must be an integer");
                return ExitCode::from(2);
            }
        },
        None => match now_ns {
            Some(n) => n,
            None => {
                eprintln!("cohort_score: no events in the given logs");
                return ExitCode::FAILURE;
            }
        },
    };

    let cohorts: BTreeMap<String, Cohort> = scorer
        .classify(as_of_ns, &cohort_cfg)
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
    let metrics: BTreeMap<String, WalletMetrics> = scorer
        .metrics()
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect();

    // Previous snapshot = newest existing file in out-dir by NAME (dates sort
    // lexicographically); diff journals only what changed (WCG-9).
    let previous = std::fs::read_dir(&out_dir).ok().and_then(|rd| {
        rd.filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .max()
    });
    let old_map = previous
        .as_deref()
        .and_then(mp_features::load_snapshot)
        .map(|(_, m)| m);

    let snapshot_path = out_dir.join(format!("{}.json", date_str_from_ns(as_of_ns)));
    if let Err(e) = mp_features::save_snapshot(&snapshot_path, &(as_of_ns, cohorts.clone())) {
        eprintln!("cohort_score: save snapshot: {e}");
        return ExitCode::FAILURE;
    }
    let changed = match mp_features::journal_changes(
        &journal,
        old_map.as_ref(),
        &cohorts,
        &metrics,
        as_of_ns,
    ) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("cohort_score: journal: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Summary (human or --json).
    let mut per_cohort = BTreeMap::new();
    for c in cohorts.values() {
        *per_cohort.entry(c.as_str()).or_insert(0usize) += 1;
    }
    let summary = serde_json::json!({
        "scored_at_ns": as_of_ns,
        "snapshot": snapshot_path.display().to_string(),
        "addresses": cohorts.len(),
        "per_cohort": per_cohort,
        "changed": changed,
        "events_replayed": events,
        "whale_positions": positions,
        "logs": logs.len(),
    });
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    } else {
        println!(
            "cohort_score: {} addresses across {} cohorts → {} ({} changed, journaled to {})",
            cohorts.len(),
            per_cohort.len(),
            snapshot_path.display(),
            changed,
            journal.display()
        );
        for (c, n) in &per_cohort {
            println!("  {c}: {n}");
        }
    }
    ExitCode::SUCCESS
}
