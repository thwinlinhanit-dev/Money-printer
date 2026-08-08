//! `mp-materialize` — offline feature materialization (spec 016 /
//! POST_CLEAN_PLAN Phase 2). Runs the feature engine (mp-features, the same
//! code a live runner uses) over recorded event logs and writes every
//! FeatureUpdate to the FeatureStore Parquet layout
//! `{out}/{feature}/ver=N/venue={v}/symbol={s}/{date}.parquet` with the FEA-6
//! footer metadata.
//!
//! Usage:
//!   mp-materialize --log <event.log> [--log <more.log> ...] \
//!       [--config <features.toml>] [--out <dir>] [--git-sha <sha>]
//!
//! `--log` is repeatable (multi-venue/multi-symbol corpora merge by
//! `(recv_ts_ns, stream_seq)`, EVT-5). Argument order is IRRELEVANT: logs are
//! canonically sorted and exact duplicates dropped before processing (MAT-5).
//! `--config` optional (defaults to `FeaturesConfig::default()`); the
//! canonical params hash is written into every Parquet footer and forces a new
//! `ver=N` directory on any change (FEA-6/W-6). `--git-sha` records which
//! engine build produced the file ("unknown" if omitted). Exit 0 on success,
//! 2 on any error.
//!
//! Deterministic (MAT-5): no wall clock (PD-3), rows sorted, idempotent
//! re-runs against the same logs/config produce byte-identical Parquet. Every
//! run persists the shared symbol table as an immutable content-addressed
//! snapshot at `{out}/symbols/{hash}.json` (the mapping behind the numeric
//! `symbol_id` columns) and records the hash in every Parquet footer
//! (`symbols_hash`).
//!
//! RAM guard: total on-disk bytes of the `--log` inputs is checked BEFORE any
//! log is read against `MP_MATERIALIZE_MAX_BYTES` (default 16 GiB ≈ a few
//! full-set days; the merge holds all events in RAM). Exceeded ⇒ fail closed
//! with guidance — slice per day or raise the cap deliberately.

use mp_features::FeaturesConfig;
use mp_storage::materialize_logs;
use std::path::PathBuf;
use std::process::ExitCode;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn all_flags(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
                i += 1;
            }
        }
        i += 1;
    }
    out
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut logs: Vec<PathBuf> = all_flags(&args, "--log")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    // Canonical input set (MAT-5): same rule as materialize_logs, so the
    // printed count matches what is actually processed.
    logs.sort();
    logs.dedup();
    if logs.is_empty() {
        return Err(
            "usage: mp-materialize --log <event.log> [--log <more.log> ...] \
             [--config <features.toml>] [--out <dir>] [--git-sha <sha>]"
                .into(),
        );
    }
    let out = flag(&args, "--out").unwrap_or_else(|| "data/features".into());
    let git_sha = flag(&args, "--git-sha").unwrap_or_else(|| "unknown".into());

    let cfg = match flag(&args, "--config") {
        Some(path) => {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("read config {path}: {e}"))?;
            FeaturesConfig::from_toml(&text).map_err(|e| format!("parse config {path}: {e}"))?
        }
        None => FeaturesConfig::default(),
    };
    let params_hash = cfg.params_hash().map_err(|e| e.to_string())?;
    println!(
        "mp-materialize: {} log(s) -> {out} (params_hash={params_hash}, git={git_sha})",
        logs.len()
    );

    let stats = materialize_logs(std::path::Path::new(&out), &cfg, &logs, &git_sha)?;
    println!(
        "materialized: events={} updates={} rows={} files={} nan_suppressed={} features={} symbols={} symbols_hash={}",
        stats.events_read,
        stats.updates,
        stats.rows_written,
        stats.files_written,
        stats.nan_suppressed,
        stats.features.join(","),
        stats.symbols,
        stats.symbols_hash
    );
    if !stats.symbols_hash.is_empty() {
        let root = PathBuf::from(&out);
        let snap = root
            .join("symbols")
            .join(format!("{}.json", stats.symbols_hash));
        println!("symbols snapshot: {}", snap.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("mp-materialize: {e}");
            ExitCode::from(2)
        }
    }
}
