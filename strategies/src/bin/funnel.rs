//! `funnel` CLI (STR-3/5): operate the promotion funnel from the shell.
//!
//! Usage:
//!   funnel <state.json> register <id> [--hypothesis-complete]
//!   funnel <state.json> promote <stage> [--i-am-human] [--evidence <run_id:ts_ns>]...
//!   funnel <state.json> demote <stage> --reason <text>
//!   funnel <state.json> kill --believed <t> --data-said <t> --lesson <t> --reason <t>
//!   funnel <state.json> show
//!
//! Promotion through G3/G4 refuses without `--i-am-human` (STR-3: agents
//! prepare evidence, never click). Every transition appends one JSONL line to
//! `<state.json>.journal` (STR-5). Binary edge: the wall clock is read here
//! (via the sanctioned `WallClock`) only to check evidence staleness — never
//! inside the funnel logic itself.

use mp_core::{Clock, StrategyId, WallClock};
use mp_strategies::{Autopsy, EvidenceRef, FunnelState, Stage};
use std::io::Write;
use std::process::ExitCode;

fn stage_of(s: &str) -> Option<Stage> {
    Some(match s {
        "idea" => Stage::Idea,
        "hypothesis" => Stage::Hypothesis,
        "backtest" => Stage::Backtest,
        "walkforward" => Stage::WalkForward,
        "paper" => Stage::Paper,
        "livesmall" => Stage::LiveSmall,
        "livescaled" => Stage::LiveScaled,
        _ => return None,
    })
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn load(path: &str) -> Result<FunnelState, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {path}: {e}"))
}

fn save(path: &str, st: &FunnelState) -> Result<(), String> {
    let json = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("write {path}: {e}"))
}

fn journal(path: &str, line: &str) -> Result<(), String> {
    let jpath = format!("{path}.journal");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jpath)
        .map_err(|e| format!("open {jpath}: {e}"))?;
    writeln!(f, "{line}").map_err(|e| e.to_string())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (state_path, cmd) = match (args.first(), args.get(1)) {
        (Some(p), Some(c)) => (p.clone(), c.clone()),
        _ => return Err("usage: funnel <state.json> register|promote|demote|kill|show ...".into()),
    };
    let rest = &args[2..];

    match cmd.as_str() {
        "register" => {
            let id = rest.first().ok_or("register needs <id>")?;
            let hyp = rest.iter().any(|a| a == "--hypothesis-complete");
            let st = FunnelState::register(StrategyId::new(id.clone()), hyp);
            save(&state_path, &st)?;
            println!("registered {id} at Idea");
            Ok(())
        }
        "promote" => {
            let to = rest
                .first()
                .and_then(|s| stage_of(s))
                .ok_or("promote needs a stage (hypothesis|backtest|walkforward|paper|livesmall|livescaled)")?;
            let human = rest.iter().any(|a| a == "--i-am-human");
            let mut evidence = Vec::new();
            for (i, a) in rest.iter().enumerate() {
                if a == "--evidence" {
                    let v = rest.get(i + 1).ok_or("--evidence needs <run_id:ts_ns>")?;
                    let (run_id, ts) = v
                        .rsplit_once(':')
                        .ok_or("evidence must be <run_id:ts_ns>")?;
                    evidence.push(EvidenceRef {
                        run_id: run_id.to_string(),
                        created_ts_ns: ts.parse().map_err(|_| "bad evidence ts_ns")?,
                    });
                }
            }
            let now_ns = WallClock.now_ns(); // binary edge (STR-4 staleness)
            let mut st = load(&state_path)?;
            let t = st
                .promote(to, human, evidence, now_ns)
                .map_err(|e| e.to_string())?;
            save(&state_path, &st)?;
            journal(&state_path, &t.to_jsonl())?;
            println!("promoted {} -> {:?}", t.id.0, t.to);
            Ok(())
        }
        "demote" => {
            let to = rest
                .first()
                .and_then(|s| stage_of(s))
                .ok_or("demote needs a stage")?;
            let reason = flag_value(rest, "--reason").ok_or("demote needs --reason")?;
            let mut st = load(&state_path)?;
            let t = st.demote(to, reason).map_err(|e| e.to_string())?;
            save(&state_path, &st)?;
            journal(&state_path, &t.to_jsonl())?;
            println!("demoted {} -> {:?}", t.id.0, t.to);
            Ok(())
        }
        "kill" => {
            let autopsy = Autopsy {
                believed: flag_value(rest, "--believed").ok_or("kill needs --believed")?,
                data_said: flag_value(rest, "--data-said").ok_or("kill needs --data-said")?,
                lesson: flag_value(rest, "--lesson").ok_or("kill needs --lesson")?,
            };
            let reason = flag_value(rest, "--reason").ok_or("kill needs --reason")?;
            let mut st = load(&state_path)?;
            let t = st.kill(&autopsy, reason).map_err(|e| e.to_string())?;
            // AUTOPSY.md lands next to the state file (STR-6, kept forever W-6).
            let dir = std::path::Path::new(&state_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            std::fs::write(dir.join("AUTOPSY.md"), autopsy.to_markdown(&st.id))
                .map_err(|e| e.to_string())?;
            save(&state_path, &st)?;
            journal(&state_path, &t.to_jsonl())?;
            println!("killed {} (autopsy written)", t.id.0);
            Ok(())
        }
        "show" => {
            let st = load(&state_path)?;
            println!(
                "{}: {:?} (evidence: {})",
                st.id.0,
                st.stage,
                st.evidence.len()
            );
            Ok(())
        }
        other => Err(format!("unknown command: {other}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("funnel: {e}");
            ExitCode::from(2)
        }
    }
}
