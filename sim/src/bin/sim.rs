//! `sim` CLI (SIM-9/10/11): backtest, walk-forward, plateau, Monte-Carlo, and
//! the replay-live determinism diff — over a recorded event log, writing
//! tracker runs (`runs/index.jsonl`).
//!
//! Usage:
//!   sim backtest    --log <event.log> --strategy coinflip|null --seed N \
//!                   --run-id <ulid> --runs-dir <dir> [--coverage F] [--git-sha S]
//!   sim wf          --log <event.log> --strategy … --seed N --train-ns T --test-ns T --step-ns T
//!   sim plateau     --base <expectancy> --point <delta:expectancy> …
//!   sim mc          --log <event.log> --strategy … --seed N --resamples R
//!   sim replay-live --log <live.log> --log-b <replay.log> --strategy … --seed N
//!
//! `replay-live` exits 1 on ANY decision-log divergence — that is a P1 bug
//! (SIM-11). The run id is caller-supplied (a ULID at the ops layer, SIM-10)
//! so this binary reads no wall clock at all (PD-3, even at the edge).

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, Venue};
use mp_features::catalog::Cvd;
use mp_features::FeatureEngine;
use mp_sim::{
    monte_carlo, plateau_ok, walk_forward, Backtester, RunRecord, SimConfig, WalkForwardParams,
};
use mp_strategies::{CoinFlipStrategy, NullStrategy, Strategy};
use std::io::Write;
use std::process::ExitCode;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("missing {name}"))
}

fn read_log(path: &str) -> Result<Vec<EventEnvelope>, String> {
    let reader =
        LogReader::open(std::path::Path::new(path)).map_err(|e| format!("open log {path}: {e}"))?;
    let mut out = Vec::new();
    for ev in reader {
        out.push(ev.map_err(|e| format!("read log {path}: {e}"))?);
    }
    Ok(out)
}

fn strategy_named(name: &str) -> Result<Box<dyn Strategy>, String> {
    match name {
        "coinflip" => Ok(Box::new(CoinFlipStrategy::new())),
        "null" => Ok(Box::new(NullStrategy)),
        other => Err(format!("unknown strategy: {other} (coinflip|null)")),
    }
}

fn engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit)));
    e
}

fn run_backtest(
    events: &[EventEnvelope],
    strategy: &str,
    seed: u64,
    coverage: f64,
) -> Result<Backtester, String> {
    let mut bt = Backtester::new(
        engine(),
        strategy_named(strategy)?,
        SimConfig::default(),
        seed,
    );
    bt.run_checked(events, coverage)
        .map_err(|e| format!("run refused: {e}"))?;
    Ok(bt)
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args
        .first()
        .cloned()
        .ok_or("usage: sim backtest|wf|plateau|mc|replay-live …")?;
    let rest = &args[1..];

    match cmd.as_str() {
        "backtest" => {
            let events = read_log(&need(rest, "--log")?)?;
            let strategy = need(rest, "--strategy")?;
            let seed: u64 = need(rest, "--seed")?.parse().map_err(|_| "bad --seed")?;
            let coverage: f64 = flag(rest, "--coverage")
                .map_or(Ok(1.0), |c| c.parse())
                .map_err(|_| "bad --coverage")?;
            let bt = run_backtest(&events, &strategy, seed, coverage)?;

            // Tracker record (SIM-10): reproducible from the index alone.
            let run_id = need(rest, "--run-id")?;
            let runs_dir = need(rest, "--runs-dir")?;
            let git_sha = flag(rest, "--git-sha").unwrap_or_else(|| "unknown".into());
            let config_text = format!("strategy={strategy};seed={seed};cfg=default");
            let (from, to) = (
                events.first().map(|e| e.recv_ts_ns).unwrap_or(0),
                events.last().map(|e| e.recv_ts_ns).unwrap_or(0),
            );
            let rec = RunRecord::new(
                run_id.clone(),
                git_sha,
                &config_text,
                from,
                to,
                vec![],
                bt.decision_log().hash(),
                bt.summary(),
            );
            std::fs::create_dir_all(&runs_dir).map_err(|e| e.to_string())?;
            let mut idx = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(format!("{runs_dir}/index.jsonl"))
                .map_err(|e| e.to_string())?;
            writeln!(idx, "{}", rec.to_jsonl()).map_err(|e| e.to_string())?;
            let s = bt.summary();
            println!(
                "run {run_id}: trades={} expectancy={:+.6} stress2x={:+.6} maxDD={:.2} log_hash={}",
                s.trades,
                s.expectancy,
                s.stress_expectancy_2x,
                s.max_drawdown,
                bt.decision_log().hash()
            );
            Ok(ExitCode::SUCCESS)
        }
        "wf" => {
            let events = read_log(&need(rest, "--log")?)?;
            let strategy = need(rest, "--strategy")?;
            let seed: u64 = need(rest, "--seed")?.parse().map_err(|_| "bad --seed")?;
            let p = WalkForwardParams {
                train_ns: need(rest, "--train-ns")?
                    .parse()
                    .map_err(|_| "bad --train-ns")?,
                test_ns: need(rest, "--test-ns")?
                    .parse()
                    .map_err(|_| "bad --test-ns")?,
                step_ns: need(rest, "--step-ns")?
                    .parse()
                    .map_err(|_| "bad --step-ns")?,
            };
            let mut err = None;
            let windows = walk_forward(&events, p, |_train, test| {
                match run_backtest(test, &strategy, seed, 1.0) {
                    Ok(bt) => bt.summary(),
                    Err(e) => {
                        err.get_or_insert(e);
                        mp_sim::MetricsSummary {
                            trades: 0,
                            expectancy: 0.0,
                            stress_expectancy_2x: 0.0,
                            max_drawdown: 0.0,
                        }
                    }
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
            for w in &windows {
                println!(
                    "window test=[{},{}): trades={} oos_expectancy={:+.6} stress2x={:+.6}",
                    w.test_start_ns,
                    w.test_end_ns,
                    w.oos.trades,
                    w.oos.expectancy,
                    w.oos.stress_expectancy_2x
                );
            }
            println!("windows={}", windows.len());
            Ok(ExitCode::SUCCESS)
        }
        "plateau" => {
            let base: f64 = need(rest, "--base")?.parse().map_err(|_| "bad --base")?;
            let mut points = Vec::new();
            for (i, a) in rest.iter().enumerate() {
                if a == "--point" {
                    let v = rest.get(i + 1).ok_or("--point needs <delta:expectancy>")?;
                    let (d, e) = v
                        .split_once(':')
                        .ok_or("point must be <delta:expectancy>")?;
                    points.push((
                        d.parse().map_err(|_| "bad delta")?,
                        e.parse().map_err(|_| "bad expectancy")?,
                    ));
                }
            }
            if plateau_ok(base, &points) {
                println!("plateau: OK");
                Ok(ExitCode::SUCCESS)
            } else {
                println!("plateau: FAIL (sign flip within ±30% — curve-fit suspect)");
                Ok(ExitCode::FAILURE)
            }
        }
        "mc" => {
            let events = read_log(&need(rest, "--log")?)?;
            let strategy = need(rest, "--strategy")?;
            let seed: u64 = need(rest, "--seed")?.parse().map_err(|_| "bad --seed")?;
            let resamples: u32 = flag(rest, "--resamples")
                .map_or(Ok(1000), |r| r.parse())
                .map_err(|_| "bad --resamples")?;
            let bt = run_backtest(&events, &strategy, seed, 1.0)?;
            let mc = monte_carlo(bt.trade_pnls(), resamples, seed, 86_400_000_000_000);
            println!(
                "mc: resamples={} p50_maxDD={:.4} p95_maxDD={:.4} worst={:.4}",
                mc.resamples, mc.p50_max_dd, mc.p95_max_dd, mc.worst_max_dd
            );
            Ok(ExitCode::SUCCESS)
        }
        "replay-live" => {
            let live = read_log(&need(rest, "--log")?)?;
            let replay = read_log(&need(rest, "--log-b")?)?;
            let strategy = need(rest, "--strategy")?;
            let seed: u64 = need(rest, "--seed")?.parse().map_err(|_| "bad --seed")?;
            let a = run_backtest(&live, &strategy, seed, 1.0)?;
            let b = run_backtest(&replay, &strategy, seed, 1.0)?;
            match a.decision_log().first_divergence(b.decision_log()) {
                None => {
                    println!(
                        "replay-live: identical decision logs (hash={})",
                        a.decision_log().hash()
                    );
                    Ok(ExitCode::SUCCESS)
                }
                Some(idx) => {
                    // A divergence is a P1 (SIM-11): print where and fail.
                    eprintln!(
                        "replay-live: DIVERGENCE at decision {idx} — P1.\n  live:   {}\n  replay: {}",
                        a.decision_log().lines().get(idx).map(String::as_str).unwrap_or("<end>"),
                        b.decision_log().lines().get(idx).map(String::as_str).unwrap_or("<end>"),
                    );
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        other => Err(format!("unknown command: {other}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sim: {e}");
            ExitCode::from(2)
        }
    }
}
