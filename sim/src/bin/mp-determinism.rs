//! `mp-determinism` — the spec 018 daily determinism check (MOD-9..11).
//!
//! Replays a recorded day through the PRODUCTION runtime (features →
//! strategy → risk, sim/src/determinism.rs) and proves the decision path is
//! reproducible: two fresh runs must be byte-identical, and when a live/paper
//! decision-log summary exists for the day the replay must match it. Runs in
//! the daily pipeline after the scorecard; writes the gate artifact
//! `data/scorecards/{date}.determinism.json` (spec 018 MOD-9: a diff blocks
//! promotion via `mp-ops promote`).
//!
//! Usage:
//!   mp-determinism --date 2026-08-13 --required hyperliquid:BTC --required hyperliquid:ETH \
//!                  [--config sim/determinism.toml] [--features-config features/features.toml] \
//!                  [--data-dir data] [--live-log <ReplaySummary.json>] [--write] [--version]
//!
//! Exit codes: 0 = check passed; 1 = check FAILED (decision log diverged —
//! determinism-diff, spec 009); 2 = command/config/data error. The pipeline
//! treats 1 as the day being flagged (no cold writes, alert raised), 2 as a
//! broken invocation.

use mp_core::Venue;
use mp_features::FeaturesConfig;
use mp_sim::{check_day, DeterminismConfig, ReplaySummary};
use mp_storage::{app_version, load_logs_merged, write_determinism, DeterminismArtifact};
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .collect()
}

/// Evidence timestamp for the artifact (the check is an ops edge; the stamp
/// is metadata, never a decision input — PD-3).
fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

fn run_args(args: &[String]) -> Result<ExitCode, String> {
    if args.iter().any(|a| a == "--version") {
        println!("{}", app_version("mp-determinism"));
        return Ok(ExitCode::SUCCESS);
    }
    let date_raw = flag(args, "--date").ok_or(
        "usage: mp-determinism --date YYYY-MM-DD --required venue:symbol ... [--write]",
    )?;
    let date = date_raw.replace('-', "");
    if date.len() != 8 {
        return Err("--date must be YYYY-MM-DD or YYYYMMDD".into());
    }
    let required = flags(args, "--required");
    if required.is_empty() {
        return Err("requires one or more --required venue:symbol entries".into());
    }
    let data_dir = flag(args, "--data-dir").unwrap_or_else(|| "data".to_string());
    let dashed = format!("{}-{}-{}", &date[0..4], &date[4..6], &date[6..8]);

    // 1. Resolve the day's raw logs from the required set (the gate's corpus).
    let mut log_paths = Vec::new();
    let mut req_pairs: Vec<(String, String)> = Vec::new();
    for item in &required {
        let (v, s) = item
            .split_once(':')
            .ok_or_else(|| format!("invalid --required {item}; expected venue:symbol"))?;
        log_paths.push(
            Path::new(&data_dir)
                .join("raw")
                .join(format!("{date}_{v}_{s}.log")),
        );
        req_pairs.push((v.to_string(), s.to_string()));
    }

    // 2. Load + remap + merge — the materializer's EXACT loader, so the replay
    //    consumes the same canonical event stream the feature store does.
    let loaded = load_logs_merged(&log_paths)?;
    if loaded.events.is_empty() {
        return Err(format!("no events recorded for {dashed} in the required set"));
    }

    // 3. Universe = the required (venue, symbol) pairs resolved onto the
    //    SHARED table (ids were re-interned across logs, EVT-8).
    let mut venues: Vec<Venue> = Vec::new();
    let mut symbols: Vec<mp_core::SymbolId> = Vec::new();
    for m in loaded.symbols.metas() {
        if req_pairs
            .iter()
            .any(|(v, s)| m.venue.slug() == *v && m.venue_symbol == *s)
        {
            if !venues.contains(&m.venue) {
                venues.push(m.venue);
            }
            if !symbols.contains(&m.symbol_id) {
                symbols.push(m.symbol_id);
            }
        }
    }
    if venues.is_empty() || symbols.is_empty() {
        return Err("required recordings produced no resolvable symbols in the shared table (corrupt logs?)".to_string());
    }

    // 4. Pinned runtime configs (determinism + features) — a config change
    //    invalidates live-vs-replay comparison by construction.
    let dcfg = match flag(args, "--config") {
        Some(p) => {
            let text = std::fs::read_to_string(&p)
                .map_err(|e| format!("read determinism config {p}: {e}"))?;
            DeterminismConfig::from_toml(&text)?
        }
        None => DeterminismConfig::defaults(),
    };
    let fe_cfg_path = flag(args, "--features-config").unwrap_or_else(|| "features/features.toml".into());
    let fe_text = std::fs::read_to_string(&fe_cfg_path)
        .map_err(|e| format!("read features config {fe_cfg_path}: {e}"))?;
    let fe_cfg = FeaturesConfig::from_toml(&fe_text).map_err(|e| e.to_string())?;

    // 5. Optional live/paper decision-log summary for the day (the paper
    //    session records the same ReplaySummary shape; absent until the live
    //    loop runs — MOD-9's comparison is then vacuously satisfied and the
    //    check proves self-determinism only).
    let live = match flag(args, "--live-log") {
        Some(p) => {
            let text = std::fs::read_to_string(&p)
                .map_err(|e| format!("read live log {p}: {e}"))?;
            Some(
                serde_json::from_str::<ReplaySummary>(&text)
                    .map_err(|e| format!("parse live log {p}: {e}"))?,
            )
        }
        None => None,
    };

    // 6. The check (two fresh runs + optional live identity).
    let v = check_day(&dashed, &loaded.events, &venues, &symbols, &dcfg, &fe_cfg, live.as_ref())?;

    // 7. Write the gate artifact (the promotion gate reads `passed` + `date`).
    if args.iter().any(|a| a == "--write") {
        let artifact = DeterminismArtifact {
            date: dashed.clone(),
            passed: v.passed,
            self_consistent: v.self_consistent,
            live_present: v.live_present,
            live_matches: v.live_matches,
            strategy: Some(v.strategy.clone()),
            seed: Some(v.seed),
            event_count: Some(v.event_count as u64),
            replayed_lines: Some(v.replayed_lines as u64),
            replayed_hash: Some(v.replayed_hash),
            reason: Some(v.reason.clone()),
            ts_ns: Some(now_ns()),
        };
        let path = write_determinism(&Path::new(&data_dir).join("scorecards"), &artifact)?;
        eprintln!("wrote {}", path.display());
    }

    // 8. Verdict — JSON on stdout, passed → 0, failed → 1 (the pipeline flags
    //    the day and raises determinism-diff, spec 009).
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "date": v.date,
            "strategy": v.strategy,
            "seed": v.seed,
            "event_count": v.event_count,
            "self_consistent": v.self_consistent,
            "replayed_hash": format!("{:016x}", v.replayed_hash),
            "replayed_lines": v.replayed_lines,
            "live_present": v.live_present,
            "live_matches": v.live_matches,
            "divergence_line": v.divergence_line,
            "passed": v.passed,
            "reason": v.reason,
        }))
        .map_err(|e| e.to_string())?
    );
    if v.passed {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run_args(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("mp-determinism: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::log::EventLogWriter;
    use mp_core::{EventEnvelope, InstrumentKind, MarketEvent, Side, SymbolId, SymbolMeta};

    fn fixture_log(dir: &std::path::Path, n: u64) {
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        let path = dir.join("raw/20260813_hyperliquid_BTC.log");
        let (mut writer, _) = EventLogWriter::open(&path).unwrap();
        writer
            .write_symbols(&[SymbolMeta::new(
                SymbolId(0),
                mp_core::Venue::Hyperliquid,
                "BTC",
                "BTC",
                "USD",
                InstrumentKind::Perp,
                0.1,
                0.1,
                1.0,
            )])
            .unwrap();
        for i in 1..=n {
            let ev = EventEnvelope::new(
                mp_core::Venue::Hyperliquid,
                SymbolId(0),
                i as i64 * 1_000_000_000,
                i as i64 * 1_000_000_000,
                i,
                MarketEvent::Trade {
                    price: 100.0,
                    qty: 1.0,
                    side: Side::Buy,
                    trade_id: i,
                },
            );
            writer.append(&ev).unwrap();
        }
        writer.sync().unwrap();
    }

    /// The binary end-to-end: a recorded day → passing verdict → artifact
    /// written where the promotion gate reads it (spec 018 MOD-9).
    #[test]
    fn mod_9_binary_writes_passing_artifact() {
        let dir = std::env::temp_dir().join(format!("mp-det-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        fixture_log(&dir, 100);
        let fe = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../features/features.toml")
            .canonicalize()
            .unwrap();
        let code = run_args(&[
            "--date".into(),
            "2026-08-13".into(),
            "--required".into(),
            "hyperliquid:BTC".into(),
            "--data-dir".into(),
            dir.to_string_lossy().into(),
            "--features-config".into(),
            fe.to_string_lossy().into(),
            "--write".into(),
        ])
        .expect("run succeeds");
        assert_eq!(code, ExitCode::SUCCESS, "a clean day must pass");
        let artifact: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("scorecards/2026-08-13.determinism.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(artifact["passed"], true);
        assert_eq!(artifact["self_consistent"], true);
        assert_eq!(artifact["strategy"], "carry-v1");
        assert!(artifact["event_count"].as_u64().unwrap() >= 100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing required log is a failed invocation (fail-closed: the gate
    /// must never see a fabricated pass on missing input).
    #[test]
    fn mod_9_binary_fails_closed_on_missing_log() {
        let dir = std::env::temp_dir().join(format!("mp-det-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let code = run_args(&[
            "--date".into(),
            "2026-08-13".into(),
            "--required".into(),
            "hyperliquid:BTC".into(),
            "--data-dir".into(),
            dir.to_string_lossy().into(),
        ]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(code.is_err(), "missing log must error, not pass");
    }
}
