//! `whale_study` — RES-4 band-accuracy event study (spec 029 LIQ-6; spec 028
//! cross-link): replays recorded Hyperliquid raw logs (mark/OI + spec 028
//! `WhalePosition`s) through [`WhaleBandStudy`](mp_features::WhaleBandStudy)
//! and reports how well the estimated `liq.est_bands` levels match the REAL
//! liquidation prices recorded from whale positions.
//!
//! Usage:
//!   whale_study --log <raw event log> [--log <...>] [--config <features.toml>] [--json]
//!              [--run-id <ulid> --runs-dir <dir>] [--git-sha <sha>]
//!              [--leverage-calibration]
//!
//! `--leverage-calibration` (spec 029 LIQ-11) replaces the band-accuracy
//! study: replay the recorded spec 028 `WhalePosition`s and report the
//! notional-weighted leverage distribution calibrated onto the configured
//! tier set — the numbers that replace the documented assumption weights in
//! `features.toml`. `liq_price` is irrelevant here (a NaN sentinel must not
//! exclude a valid leverage sample).
//!
//! Every `--log` is an mp event log (`mp_core::log`). Each collector run
//! interns its own `SymbolId`s (EVT-8), so logs are first remapped onto one
//! canonical symbol space keyed by `(venue, venue_symbol)`, then merged in
//! `(recv_ts_ns, stream_seq)` order (EVT-5) and replayed. Pure offline replay
//! (PD-3): no wall clock, no network. The output is research evidence only —
//! bands stay out of strategies until this study clears (WHL-5/PD-4).
//!
//! `--config` is optional: when absent, the shipped `liq.est_bands` defaults
//! (LIQ-7) are used, so the offline grade is the live model's grade
//! (one-code-path, FEA-4).
//!
//! With `--run-id` + `--runs-dir` the study journals a SIM-10-style run
//! record to `<runs-dir>/index.jsonl` (append-only, W-6) — the RES-4
//! "tracker-style run records" requirement (spec 010). The record carries
//! run_id, git_sha, the canonical `liq.est_bands` params hash, the data
//! range, and the (n, mre, coverage) metrics, so the study is reproducible
//! from the record alone. Unlike the sim tracker (which hashes the literal
//! config text), the hash here covers the RESOLVED params — defaults the
//! file omits are hashed too, so two runs with the same effective model
//! grade identically.

use mp_core::fnv1a_64;
use mp_core::log::LogReader;
use mp_core::merge_sorted_events;
use mp_core::{EventEnvelope, MarketEvent, SymbolId, Venue};
use mp_features::config::{FeaturesConfig, LiqEstBandsParams};
use mp_features::{calibrate_leverage_weights, tier_leverages, WhaleBandStudy};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Read one log file fully: all events plus the FINAL symbol-table snapshot
/// (the table grows as symbol frames arrive, so the last one is complete —
/// EVT-8).
fn load_log(path: &Path) -> Result<(Vec<EventEnvelope>, Vec<mp_core::SymbolMeta>), String> {
    let mut reader = LogReader::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut events = Vec::new();
    for ev in reader.by_ref() {
        events.push(ev.map_err(|e| format!("read {}: {e}", path.display()))?);
    }
    let metas = reader.symbols().to_vec();
    Ok((events, metas))
}

/// Merge several logs' symbol id spaces onto ONE canonical space keyed by
/// `(venue, venue_symbol)` (the identity law, EVT-8). Different collector runs
/// intern ids independently, so the same coin can carry a different `SymbolId`
/// in the hyperliquid market log vs the `mp-whale` positions log — without
/// remapping, per-symbol band state would never meet its positions.
/// Deterministic: canonical ids are assigned in BTreeMap (sorted) order
/// (CONV-10). Returns the per-source local→canonical map.
fn canonical_symbols(
    logs: &[(Vec<EventEnvelope>, Vec<mp_core::SymbolMeta>)],
) -> Vec<HashMap<SymbolId, SymbolId>> {
    let mut canon: BTreeMap<(Venue, String), SymbolId> = BTreeMap::new();
    logs.iter()
        .map(|(_, metas)| {
            let mut local = HashMap::new();
            for m in metas {
                let next = canon.len() as u32;
                let id = *canon
                    .entry((m.venue, m.venue_symbol.clone()))
                    .or_insert(SymbolId(next));
                local.insert(m.symbol_id, id);
            }
            local
        })
        .collect()
}

/// Append one SIM-10 run record (JSONL) to `<runs-dir>/index.jsonl` — the
/// RES-4 tracker, append-only (W-6). Shared by the band-accuracy study and
/// the leverage-calibration mode.
fn append_run_record(runs_dir: &str, record: &serde_json::Value) -> Result<(), String> {
    std::fs::create_dir_all(runs_dir).map_err(|e| format!("create {runs_dir}: {e}"))?;
    let mut idx = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{runs_dir}/index.jsonl"))
        .map_err(|e| format!("open runs index: {e}"))?;
    writeln!(
        idx,
        "{}",
        serde_json::to_string(record).map_err(|e| e.to_string())?
    )
    .map_err(|e| format!("write runs index: {e}"))
}

/// Shared reproducibility context for a report (RES-4/SIM-10): the canonical
/// resolved-params hash + the merged data range.
struct RunMeta<'a> {
    config_hash: &'a str,
    data_from_ns: i64,
    data_to_ns: i64,
}

/// LIQ-11 leverage calibration: replay the recorded spec 028 `WhalePosition`s
/// and report the notional-weighted leverage distribution calibrated onto the
/// CONFIGURED tier set (defaults when no `--config` — the live model's set,
/// one-code-path FEA-4). The weights are the numbers that replace the
/// documented assumption weights in `features.toml`.
///
/// A position counts if `leverage`, `size`, and `entry` are finite and
/// sensible; `liq_price` is deliberately NOT a filter (a NaN sentinel must
/// not exclude a valid leverage sample). Fail-closed (CONV-8): anything else
/// is skipped, never invented. Output is research evidence (WHL-5/PD-4).
fn run_leverage_calibration(
    merged: &[EventEnvelope],
    params: &LiqEstBandsParams,
    meta: RunMeta<'_>,
    git_sha: &str,
    run_id: Option<&str>,
    runs_dir: Option<&str>,
    json: bool,
) -> Result<ExitCode, String> {
    let config_hash = meta.config_hash;
    let data_from_ns = meta.data_from_ns;
    let data_to_ns = meta.data_to_ns;
    let mut samples: Vec<(f64, f64)> = Vec::new();
    let mut positions_seen = 0u64;
    for ev in merged {
        let MarketEvent::WhalePosition {
            size,
            entry,
            leverage,
            ..
        } = &ev.body
        else {
            continue;
        };
        if ev.venue != Venue::Hyperliquid {
            continue;
        }
        positions_seen += 1;
        if leverage.is_finite()
            && *leverage > 0.0
            && size.is_finite()
            && *size != 0.0
            && entry.is_finite()
            && *entry > 0.0
        {
            // Notional proxy: |size| × entry (coin units × entry price) — the
            // amount at risk, matching how the model spreads OI across tiers.
            samples.push((*leverage, size.abs() * *entry));
        }
    }
    let tiers = calibrate_leverage_weights(&samples, &tier_leverages(&params.leverage_tiers));
    let n: u64 = tiers.iter().map(|t| t.count).sum();
    let total_notional: f64 = tiers.iter().map(|t| t.notional).sum();
    let sum_weights: f64 = tiers.iter().map(|t| t.weight).sum();

    let tiers_json: Vec<serde_json::Value> = tiers
        .iter()
        .map(|t| {
            serde_json::json!({
                "leverage": t.leverage,
                "weight": t.weight,
                "count": t.count,
                "notional": t.notional,
            })
        })
        .collect();

    let mut report = serde_json::json!({
        "study": "leverage_calibration",
        "git_sha": git_sha,
        "n": n,
        "positions_seen": positions_seen,
        "total_notional": total_notional,
        "maintenance_buffer": params.maintenance_buffer,
        "tiers": tiers_json,
        "sum_weights": sum_weights,
        "config_hash": config_hash,
        "data_from_ns": data_from_ns,
        "data_to_ns": data_to_ns,
    });
    let journaled = if let (Some(run_id), Some(runs_dir)) = (run_id, runs_dir) {
        let record = serde_json::json!({
            "study": "leverage_calibration",
            "run_id": run_id,
            "git_sha": git_sha,
            "config_hash": config_hash,
            "data_from_ns": data_from_ns,
            "data_to_ns": data_to_ns,
            "n": n,
            "positions_seen": positions_seen,
            "total_notional": total_notional,
            "maintenance_buffer": params.maintenance_buffer,
            "tiers": tiers_json,
        });
        append_run_record(runs_dir, &record)?;
        Some(record)
    } else {
        None
    };
    // The report echoes the record's run_id so a reader can verify the journal
    // entry from the report alone (same contract as the band-accuracy study).
    if let Some(rec) = &journaled {
        report["run_id"] = rec["run_id"].clone();
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        println!("leverage_calibration: spec 028 real leverage distribution (spec 029 LIQ-11)");
        println!(
            "  positions_seen={positions_seen} valid_samples={n} total_notional={total_notional:.2}"
        );
        for t in &tiers {
            println!(
                "  leverage {:>7.1} : weight={:.6} count={} notional={:.2}",
                t.leverage, t.weight, t.count, t.notional
            );
        }
        println!("  sum(weights)={sum_weights:.6}");
    }
    Ok(ExitCode::SUCCESS)
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut log_paths: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--log" {
            let p = args.get(i + 1).ok_or("missing path after --log")?.clone();
            log_paths.push(PathBuf::from(p));
            i += 2;
        } else {
            i += 1;
        }
    }
    if log_paths.is_empty() {
        return Err(
            "usage: whale_study --log <raw event log> [--log <...>] [--config <features.toml>] [--json] [--run-id <ulid> --runs-dir <dir>] [--git-sha <sha>] [--leverage-calibration]"
                .into(),
        );
    }
    let json = args.iter().any(|a| a == "--json");
    let leverage_calibration = args.iter().any(|a| a == "--leverage-calibration");
    let run_id = flag(&args, "--run-id");
    let runs_dir = flag(&args, "--runs-dir");
    if run_id.is_some() != runs_dir.is_some() {
        return Err("--run-id and --runs-dir must be given together".into());
    }
    let git_sha = flag(&args, "--git-sha").unwrap_or_else(|| "unknown".into());
    let params: LiqEstBandsParams = match flag(&args, "--config") {
        Some(cfg_path) => {
            let text = std::fs::read_to_string(&cfg_path)
                .map_err(|e| format!("read {}: {e}", cfg_path))?;
            let cfg =
                FeaturesConfig::from_toml(&text).map_err(|e| format!("parse {}: {e}", cfg_path))?;
            cfg.liq_est_bands
        }
        None => LiqEstBandsParams::default(),
    };

    // Load, remap onto a canonical symbol space, merge in recv order (EVT-5).
    let mut loaded: Vec<(Vec<EventEnvelope>, Vec<mp_core::SymbolMeta>)> = Vec::new();
    for p in &log_paths {
        loaded.push(load_log(p)?);
    }
    let remap = canonical_symbols(&loaded);
    // Fail-closed remap (CONV-8): an event whose symbol is NOT in its own
    // log's final table (torn/corrupt log) keeps a local id that could
    // COLLIDE with a different instrument in another log — silently pairing a
    // position with the wrong symbol's band state. Skip it and say so.
    let mut n_unmapped = 0u64;
    for ((events, _), local) in loaded.iter_mut().zip(&remap) {
        // retain_mut: we rewrite each kept event's symbol (Vec::retain's
        // closure is read-only on this toolchain; retain_mut is the mutable
        // filter).
        events.retain_mut(|ev| match local.get(&ev.symbol) {
            Some(&canonical) => {
                ev.symbol = canonical;
                true
            }
            None => {
                n_unmapped += 1;
                false
            }
        });
    }
    let sources: Vec<_> = loaded
        .into_iter()
        .map(|(events, _)| events.into_iter())
        .collect();
    let merged = merge_sorted_events(sources);

    // Shared reproducibility fields (RES-4/SIM-10): the canonical resolved
    // params hash + the merged data range — computed once for both modes.
    let canonical_params =
        serde_json::to_string(&params).map_err(|e| format!("params serialize: {e}"))?;
    let config_hash = format!("{:016x}", fnv1a_64(canonical_params.as_bytes()));
    let data_from_ns = merged.first().map(|e| e.recv_ts_ns).unwrap_or(0);
    let data_to_ns = merged.last().map(|e| e.recv_ts_ns).unwrap_or(0);

    // LIQ-11 mode: the recorded spec 028 leverage distribution instead of the
    // band-accuracy study (skips the per-symbol band replay entirely).
    if leverage_calibration {
        return run_leverage_calibration(
            &merged,
            &params,
            RunMeta {
                config_hash: &config_hash,
                data_from_ns,
                data_to_ns,
            },
            &git_sha,
            run_id.as_deref(),
            runs_dir.as_deref(),
            json,
        );
    }

    // Replay (pure, deterministic) + count what we saw.
    let mut study = WhaleBandStudy::from_params(&params);
    let (mut n_mark, mut n_oi, mut n_whale) = (0u64, 0u64, 0u64);
    for ev in &merged {
        match &ev.body {
            MarketEvent::MarkPrice { .. } => n_mark += 1,
            MarketEvent::OpenInterest { .. } => n_oi += 1,
            MarketEvent::WhalePosition { .. } => n_whale += 1,
            _ => {}
        }
        study.on_event(ev);
    }
    let long = study.long_accuracy();
    let short = study.short_accuracy();
    let total = study.accuracy();

    // Metrics + events objects are built ONCE and shared by the journal record
    // and the JSON report, so the two outputs can never drift.
    let metrics = serde_json::json!({
        "observations": total.n,
        "long": { "n": long.n, "mean_relative_error": long.mean_relative_error, "coverage": long.coverage },
        "short": { "n": short.n, "mean_relative_error": short.mean_relative_error, "coverage": short.coverage },
        "total": { "n": total.n, "mean_relative_error": total.mean_relative_error, "coverage": total.coverage },
    });
    let events_seen = serde_json::json!({ "mark": n_mark, "open_interest": n_oi, "whale_positions": n_whale, "unmapped_skipped": n_unmapped });

    // SIM-10 run record (RES-4): journaled to runs/index.jsonl, append-only
    // (W-6). Reproducible from the record alone — run_id + git_sha + resolved
    // params hash + data range + metrics (same contract as the sim tracker).
    let journaled = if let (Some(run_id), Some(runs_dir)) = (run_id.as_deref(), runs_dir.as_deref())
    {
        let record = serde_json::json!({
            "study": "whale_study",
            "run_id": run_id,
            "git_sha": git_sha,
            "config_hash": config_hash,
            "data_from_ns": data_from_ns,
            "data_to_ns": data_to_ns,
            "events": events_seen,
            "observations": metrics["observations"],
            "long": metrics["long"],
            "short": metrics["short"],
            "total": metrics["total"],
        });
        append_run_record(runs_dir, &record)?;
        Some(record)
    } else {
        None
    };
    if json {
        let mut report = serde_json::json!({
            "study": "whale_study",
            "git_sha": git_sha,
            "params": {
                "maintenance_buffer": params.maintenance_buffer,
                "leverage_tiers": params.leverage_tiers.iter().map(|t| {
                    serde_json::json!({ "leverage": t.leverage, "weight": t.weight })
                }).collect::<Vec<_>>(),
            },
            "events": events_seen,
            "observations": metrics["observations"],
            "long": metrics["long"],
            "short": metrics["short"],
            "total": metrics["total"],
        });
        // Only present when this run was journaled (no null placeholders), and
        // the journal record's run_id/config_hash are echoed so a reader can
        // verify the record from the report alone.
        if let Some(rec) = &journaled {
            report["run_id"] = rec["run_id"].clone();
            report["config_hash"] = rec["config_hash"].clone();
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        if let (Some(run_id), Some(runs_dir)) = (run_id.as_deref(), runs_dir.as_deref()) {
            println!("  run: {run_id} (config_hash={config_hash}, git={git_sha}, data=[{data_from_ns},{data_to_ns}) journaled to {runs_dir}/index.jsonl");
        }
        println!(
            "whale_study: RES-4 band accuracy (spec 028 real liq prices vs spec 029 est bands)"
        );
        println!("  events: mark={n_mark} open_interest={n_oi} whale_positions={n_whale} (unmapped skipped: {n_unmapped})");
        println!(
            "  observations: {} (long={} short={})",
            total.n, long.n, short.n
        );
        println!(
            "  long : n={} mre={:.6} coverage={:.3}",
            long.n, long.mean_relative_error, long.coverage
        );
        println!(
            "  short: n={} mre={:.6} coverage={:.3}",
            short.n, short.mean_relative_error, short.coverage
        );
        println!(
            "  total: n={} mre={:.6} coverage={:.3}",
            total.n, total.mean_relative_error, total.coverage
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("whale_study: {e}");
            ExitCode::from(2)
        }
    }
}
