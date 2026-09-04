//! `signals` — signal catalog CLI (spec 025). Manage the feature-level funnel:
//! register signals, apply grading batches (promotion needs evidence), run
//! decay re-tests (demotion is automatic), kill with justification.
//!
//! Usage:
//!   signals --file <catalog.json> register --id <sig> --hypothesis <text>
//!           --params-hash <h> [--now-ns N]
//!   signals --file <catalog.json> list
//!   signals --file <catalog.json> grade --id <sig> --run-id <r> --n <n>
//!           --win-rate <w> --avg-excess <e> [--horizon-ns H] [--min-n 30]
//!           [--now-ns N] [--human]
//!   signals --file <catalog.json> retest --id <sig> [--now-ns N]
//!   signals --file <catalog.json> kill --id <sig> --why <justification>
//!
//! `--now-ns` is optional; when absent the binary uses the sanctioned
//! `WallClock` (the live edge — replay paths must pass a fixed `--now-ns`,
//! PD-3/CONV-5). The catalog file is loaded, mutated, and atomically replaced
//! (never appended): one catalog, one file.

use mp_core::{Clock, WallClock};
use mp_features::signal_catalog::{GradeSnapshot, SignalCatalog};
use std::path::Path;
use std::process::ExitCode;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("missing {name}"))
}

fn now_ns(args: &[String]) -> Result<i64, String> {
    match flag(args, "--now-ns") {
        Some(v) => v.parse().map_err(|_| "bad --now-ns".to_string()),
        None => Ok(WallClock.now_ns()),
    }
}

fn load(path: &Path) -> Result<SignalCatalog, String> {
    if !path.exists() {
        return Ok(SignalCatalog::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("read catalog: {e}"))?;
    SignalCatalog::from_json(&text).map_err(|e| format!("parse catalog: {e}"))
}

fn save(path: &Path, c: &SignalCatalog) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, c.to_json().map_err(|e| e.to_string())?)
        .map_err(|e| format!("write catalog: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("replace catalog: {e}"))
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let file = need(&args, "--file")?;
    let cmd = args
        .iter()
        .find(|a| {
            matches!(
                a.as_str(),
                "register" | "list" | "grade" | "retest" | "kill" | "health"
            )
        })
        .cloned()
        .ok_or("usage: signals --file <catalog.json> register|list|grade|retest|kill|health …")?;
    let mut catalog = load(Path::new(&file))?;

    match cmd.as_str() {
        "register" => {
            let id = need(&args, "--id")?;
            let hypothesis = need(&args, "--hypothesis")?;
            let params_hash = need(&args, "--params-hash")?;
            catalog
                .register(id.clone(), hypothesis.clone(), params_hash.clone())
                .map_err(|e| format!("register {id}: {e}"))?;
            save(Path::new(&file), &catalog)?;
            println!("registered {id} (Hypothesis): {hypothesis} [params {params_hash}]");
        }
        "list" => {
            // Zero-Cost Mode (docs/ZERO_COST_MODE.md): --zero-cost filters
            // to only zero_cost_compatible signals.
            let zero_cost_only = args.iter().any(|a| a == "--zero-cost");
            let filtered: Vec<_> = catalog.signals.iter()
                .filter(|s| !zero_cost_only || s.zero_cost_compatible)
                .collect();
            if zero_cost_only {
                println!("Zero-Cost compatible signals ({}):", filtered.len());
            }
            for s in &filtered {
                println!(
                    "{:?} {:10} grades={} last={} weekly={} re-test_due={} zc={}",
                    s.stage,
                    s.id,
                    s.grades.len(),
                    if s.last_grade_ts_ns == 0 {
                        "never".into()
                    } else {
                        s.last_grade_ts_ns.to_string()
                    },
                    s.weekly_avg_excess.len(),
                    s.re_test_due(now_ns(&args)?),
                    s.zero_cost_compatible,
                );
            }
        }
        "grade" => {
            let id = need(&args, "--id")?;
            let now = now_ns(&args)?;
            let min_n: u64 =
                flag(&args, "--min-n").map_or(Ok(30), |v| v.parse().map_err(|_| "bad --min-n"))?;
            let human = args.iter().any(|a| a == "--human");
            let rec = catalog
                .get_mut(&id)
                .ok_or_else(|| format!("unknown signal {id}"))?;
            // REL-4: the grade is stamped with the record's CURRENT identity
            // fingerprint — a later identity change invalidates it.
            let g = GradeSnapshot {
                run_id: need(&args, "--run-id")?,
                created_ts_ns: now,
                horizon_ns: flag(&args, "--horizon-ns").map_or(Ok(3_600_000_000_000), |v| {
                    v.parse().map_err(|_| "bad --horizon-ns")
                })?,
                n: need(&args, "--n")?
                    .parse()
                    .map_err(|_| "bad --n".to_string())?,
                win_rate: need(&args, "--win-rate")?
                    .parse()
                    .map_err(|_| "bad --win-rate".to_string())?,
                avg_excess: need(&args, "--avg-excess")?
                    .parse()
                    .map_err(|_| "bad --avg-excess".to_string())?,
                identity: rec.identity().fingerprint(),
            };
            match rec.apply_grade(g, min_n, human, now) {
                Ok(()) => {
                    let stage = format!("{:?}", rec.stage);
                    save(Path::new(&file), &catalog)?;
                    println!("{id} now {stage}");
                }
                Err(e) => {
                    eprintln!("grade refused: {e} (PD-5 — a refused promotion is a valid result)");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        "retest" => {
            let id = need(&args, "--id")?;
            let now = now_ns(&args)?;
            let rec = catalog
                .get_mut(&id)
                .ok_or_else(|| format!("unknown signal {id}"))?;
            if rec.re_test_due(now) {
                println!("{id}: re-test due (last grade older than the interval)");
            }
            if rec.detect_decay() {
                save(Path::new(&file), &catalog)?;
                println!("{id}: DECAYED — auto-demoted to Hypothesis (RES-3)");
            } else {
                println!("{id}: no decay flagged (stage {:?})", rec.stage);
            }
        }
        "kill" => {
            let id = need(&args, "--id")?;
            let why = need(&args, "--why")?;
            let rec = catalog
                .get_mut(&id)
                .ok_or_else(|| format!("unknown signal {id}"))?;
            rec.kill(why).map_err(|e| format!("kill {id}: {e}"))?;
            save(Path::new(&file), &catalog)?;
            println!("{id} killed (terminal)");
        }
        "health" => {
            // Weekly Signal Health summary (Phase C13): graded, decayed,
            // sample counts, zero-cost compatibility. Graded = at least 1
            // grade. Decay = detect_decay() true. Stale = re_test_due() true.
            let now = now_ns(&args)?;
            let zero_cost_only = args.iter().any(|a| a == "--zero-cost");
            let signals: Vec<_> = catalog.signals.iter()
                .filter(|s| !zero_cost_only || s.zero_cost_compatible)
                .collect();
            let total = signals.len();
            let graded = signals.iter().filter(|s| !s.grades.is_empty()).count();
            let decayed = signals.iter().filter(|s| s.would_decay()).count();
            let stale = signals.iter().filter(|s| s.re_test_due(now)).count();
            let compatible = signals.iter().filter(|s| s.zero_cost_compatible).count();
            println!("Signal Health Summary{}:", if zero_cost_only { " (Zero-Cost)" } else { "" });
            println!("  Total: {total}  Graded: {graded}  Decayed: {decayed}  Stale: {stale}  Zero-Cost compatible: {compatible}");
            if decayed > 0 {
                println!("  Decayed signals:");
                for s in signals.iter().filter(|s| s.would_decay()) {
                    let last = if s.last_grade_ts_ns == 0 { "never".into() } else { s.last_grade_ts_ns.to_string() };
                    println!("    {} (stage {:?}, last_grade={})", s.id, s.stage, last);
                }
            }
            if stale > 0 {
                println!("  Stale (re-test due):");
                for s in signals.iter().filter(|s| s.re_test_due(now)) {
                    println!("    {} (grades={}, zc={})", s.id, s.grades.len(), s.zero_cost_compatible);
                }
            }
        }
        other => return Err(format!("unknown command {other}")),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("signals: {e}");
            ExitCode::from(2)
        }
    }
}
