//! `accumulation_replay` — RES-4 event-study feeder for the accumulation
//! detector (spec 045 ACC-5/ACC-7).
//!
//! Replays recorded raw logs through `engine_from_config` (the ONE code path,
//! FEA-4) with the detector's input families active, feeds every
//! FeatureUpdate to [`AccumulationDetector`](mp_features::AccumulationDetector),
//! and journals every ScreenerHit via HitJournal (W-6 daily JSONL). The hit
//! records are what `research/` grades into forward returns (+4h/+12h/+24h)
//! and the signal catalog consumes for promotion (n ≥ 30, ACC-5).
//!
//! Usage:
//!   accumulation_replay --log <file> [--log <file>...] [--config features.toml]
//!                       [--hits-dir research/out/acc_hits] [--json]
//!
//! Deterministic offline replay (PD-3): no wall clock; hits derive only from
//! recorded events. Fail-closed legs (missing netflow data today) simply
//! produce zero hits — an honest n=0 study input, never a fabricated one.

use mp_core::log::{merge_sorted_events, LogReader};
use mp_features::config::FeaturesConfig;
use mp_features::{AccumulationDetector, HitJournal};
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let logs: Vec<PathBuf> = values(&args, "--log")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    if logs.is_empty() {
        eprintln!(
            "usage: accumulation_replay --log <file> [--log ...] \
             [--config features.toml] [--hits-dir DIR] [--json]"
        );
        return ExitCode::from(2);
    }

    let config_path = flag(&args, "--config").unwrap_or_else(|| "features.toml".into());
    let cfg_text = match std::fs::read_to_string(&config_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("accumulation_replay: read {config_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut cfg: FeaturesConfig = match toml::from_str(&cfg_text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("accumulation_replay: parse {config_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The study evaluates the detector with its FULL input set regardless of
    // live-file toggles: oi_regime on, cohort on (snapshot from file), and
    // netflow_flow honored as-configured (absent data ⇒ suppressed leg ⇒
    // zero compound hits, ACC-6 AND semantics — never degraded silently).
    cfg.oi_regime.enabled = true;
    cfg.cohort.enabled = true;

    let mut engine = match mp_features::engine_from_config(&cfg) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("accumulation_replay: build engine: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut detector = AccumulationDetector::new(cfg.accumulation.inner.clone());

    // Load + globally merge all logs by (recv_ts_ns, seq).
    let mut sources = Vec::new();
    for path in &logs {
        let mut reader = match LogReader::open(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("accumulation_replay: open {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        };
        let mut evs = Vec::new();
        for ev in reader.by_ref() {
            match ev {
                Ok(e) => evs.push(e),
                Err(e) => {
                    eprintln!("accumulation_replay: read {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
            }
        }
        sources.push(evs.into_iter());
    }
    let stream = merge_sorted_events(sources);

    let clock = mp_core::SimClock::new(0);
    let hits_dir =
        PathBuf::from(flag(&args, "--hits-dir").unwrap_or_else(|| "research/out/acc_hits".into()));
    let mut journal = match HitJournal::open(&hits_dir, &clock) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("accumulation_replay: open hit journal: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (mut events, mut updates, mut hits) = (0usize, 0usize, 0usize);
    let inputs = detector.input_features();
    let mut input_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut all_name_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for ev in stream {
        events += 1;
        for u in engine.on_event(&ev) {
            updates += 1;
            *all_name_counts.entry(u.name.clone()).or_insert(0) += 1;
            if inputs.contains(&u.name) {
                *input_counts.entry(u.name.clone()).or_insert(0) += 1;
            }
            if let Some(hit) = detector.on_update(&u, u.ts_ns) {
                if journal.record(hit).is_ok() {
                    hits += 1;
                }
            }
        }
    }

    // Which detector legs were ALIVE vs starved — the honest study record.
    let leg_status: serde_json::Map<String, serde_json::Value> = inputs
        .iter()
        .map(|n| {
            let c = input_counts.get(n).copied().unwrap_or(0);
            (n.clone(), serde_json::json!(c))
        })
        .collect();
    // Top emitted families (diagnostic: proves what the stream carried).
    let mut top: Vec<(String, usize)> = all_name_counts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let top_names: Vec<serde_json::Value> = top
        .iter()
        .take(15)
        .map(|(n, c)| serde_json::json!({"name": n, "updates": c}))
        .collect();

    let summary = serde_json::json!({
        "logs": logs.len(),
        "events": events,
        "feature_updates": updates,
        "screener_hits": hits,
        "hits_dir": hits_dir.display().to_string(),
        "detector_input_updates": leg_status,
        "top_update_names": top_names,
        "note": if hits == 0 {
            "zero hits — at least one sub-signal leg had no data (ACC-6 AND \
             semantics); this is the honest n=0 RES-4 study input"
        } else {
            "hits journaled; grade via research/grading.py forward returns"
        },
    });
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    } else {
        println!(
            "accumulation_replay: {} events → {} updates → {} hits (journal in {})",
            events,
            updates,
            hits,
            hits_dir.display()
        );
    }
    ExitCode::SUCCESS
}
