//! `sim` CLI (SIM-9/10/11/15): backtest, walk-forward, plateau, Monte-Carlo,
//! replay-live, and paper (live-feed tail / one-shot replay through the same
//! FillSimulator) — over recorded event logs, writing tracker runs
//! (`runs/index.jsonl`).
//!
//! Usage:
//!   sim backtest    --log <event.log> --strategy coinflip|null --seed N \
//!                   --run-id <ulid> --runs-dir <dir> [--coverage F] [--git-sha S]
//!   sim wf          --log <event.log> --strategy … --seed N --train-ns T --test-ns T --step-ns T
//!   sim plateau     --base <expectancy> --point <delta:expectancy> …
//!   sim mc          --log <event.log> --strategy … --seed N --resamples R
//!   sim replay-live --log <live.log> --log-b <replay.log> --strategy … --seed N
//!   sim paper       --log <live.log> --strategy … --seed N \
//!                   --run-id <ulid> --runs-dir <dir> [--chunk-size N]
//!   sim paper-tail  --log <live.log> --strategy … --seed N \
//!                   [--poll-ms M] [--max-idle-polls K] [--max-polls P]
//!                   --run-id <ulid> --runs-dir <dir>
//!
//! `replay-live` exits 1 on ANY decision-log divergence — that is a P1 bug
//! (SIM-11). `paper` runs the SAME FillSimulator as backtest over a live
//! (tailing) event feed (EXE-8/SIM-15); `paper-tail` re-reads the growing log
//! each poll and skips already-consumed frames (idempotent, v1 documented in
//! spec 005). The run id is caller-supplied (a ULID at the ops layer, SIM-10)
//! so this binary reads no wall clock at all (PD-3, even at the edge).

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, Venue};
use mp_features::catalog::{Cvd, FundingRate};
use mp_features::FeatureEngine;
use mp_sim::{
    monte_carlo, plateau_ok, Backtester, MetricsSummary, PaperSession, RunRecord, SimConfig,
    WalkForwardParams, WindowResult,
};
use mp_strategies::{CarryConfig, CarryV1, CoinFlipStrategy, NullStrategy, Strategy, Universe};
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

fn universe_from_events(events: &[EventEnvelope]) -> Universe {
    let mut venues = Vec::new();
    let mut symbols = Vec::new();
    for ev in events {
        if !venues.contains(&ev.venue) {
            venues.push(ev.venue);
        }
        if !symbols.contains(&ev.symbol) {
            symbols.push(ev.symbol);
        }
        if venues.len() > 3 && symbols.len() > 5 {
            break;
        }
    }
    Universe { venues, symbols }
}

fn strategy_named(
    name: &str,
    events: &[EventEnvelope],
    entry_threshold: Option<f64>,
    exit_threshold: Option<f64>,
) -> Result<Box<dyn Strategy>, String> {
    match name {
        "coinflip" => Ok(Box::new(CoinFlipStrategy::new())),
        "null" => Ok(Box::new(NullStrategy)),
        "carry-v1" => {
            let uni = universe_from_events(events);
            let mut cfg = CarryConfig::default();
            if let Some(et) = entry_threshold {
                cfg.entry_threshold = et;
            }
            if let Some(xt) = exit_threshold {
                cfg.exit_threshold = xt;
            } else if let Some(et) = entry_threshold {
                cfg.exit_threshold = et * 0.2;
            }
            Ok(Box::new(CarryV1::new(
                mp_core::StrategyId::new("carry-v1"),
                uni,
                cfg,
            )))
        }
        other => Err(format!(
            "unknown strategy: {other} (coinflip|null|carry-v1)"
        )),
    }
}

fn engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new(Venue::Bybit)));
    e.register_tick(|| Box::new(FundingRate::new()));
    e
}

fn run_backtest(
    events: &[EventEnvelope],
    strategy: &str,
    seed: u64,
    coverage: f64,
    carry_entry: Option<f64>,
    carry_exit: Option<f64>,
) -> Result<Backtester, String> {
    let cfg = SimConfig {
        min_coverage: coverage,
        bar_tf_ns: 1_000_000,
        latency_ns: 0,
        fill_model: mp_sim::FillModel::L0BarFill,
        ..SimConfig::default()
    };
    let mut bt = Backtester::new(
        engine(),
        strategy_named(strategy, events, carry_entry, carry_exit)?,
        cfg,
        seed,
    );
    bt.run_checked(events, coverage)
        .map_err(|e| format!("run refused: {e}"))?;
    Ok(bt)
}

/// Write a tracker run record + print the summary line (SIM-10).
fn record_run(
    runs_dir: &str,
    run_id: &str,
    git_sha: &str,
    config_text: &str,
    bt: &Backtester,
    events: &[EventEnvelope],
) -> Result<(), String> {
    let (from, to) = (
        events.first().map(|e| e.recv_ts_ns).unwrap_or(0),
        events.last().map(|e| e.recv_ts_ns).unwrap_or(0),
    );
    let rec = RunRecord::new(
        run_id.to_string(),
        git_sha.to_string(),
        config_text,
        from,
        to,
        vec![],
        bt.decision_log().hash(),
        bt.summary(),
    );
    std::fs::create_dir_all(runs_dir).map_err(|e| e.to_string())?;
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
    Ok(())
}

/// One-shot paper replay: the recorded log fed through the paper path in
/// batches — EXE-8's code-shape (same FillSimulator), SIM-15's batching.
fn run_paper(
    events: &[EventEnvelope],
    strategy: &str,
    seed: u64,
    chunk: usize,
) -> Result<Backtester, String> {
    let cfg = SimConfig {
        min_coverage: 1.0,
        bar_tf_ns: 1_000_000,
        latency_ns: 0,
        fill_model: mp_sim::FillModel::L0BarFill,
        ..SimConfig::default()
    };
    let bt = Backtester::new(
        engine(),
        strategy_named(strategy, events, None, None)?,
        cfg,
        seed,
    );
    let mut session = PaperSession::new(bt);
    for batch in events.chunks(chunk.max(1)) {
        session
            .push_batch(batch.to_vec())
            .map_err(|e| format!("paper feed refused: {e}"))?;
    }
    session
        .close()
        .map_err(|e| format!("paper close refused: {e}"))
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args
        .first()
        .cloned()
        .ok_or("usage: sim backtest|wf|plateau|mc|replay-live|paper|paper-tail …")?;
    let rest = &args[1..];

    match cmd.as_str() {
        "backtest" => {
            let events = read_log(&need(rest, "--log")?)?;
            let strategy = need(rest, "--strategy")?;
            let seed: u64 = need(rest, "--seed")?.parse().map_err(|_| "bad --seed")?;
            let coverage: f64 = flag(rest, "--coverage")
                .map_or(Ok(1.0), |c| c.parse())
                .map_err(|_| "bad --coverage")?;
            let carry_entry: Option<f64> = flag(rest, "--carry-entry")
                .map(|v| v.parse().map_err(|_| "bad --carry-entry"))
                .transpose()?;
            let carry_exit: Option<f64> = flag(rest, "--carry-exit")
                .map(|v| v.parse().map_err(|_| "bad --carry-exit"))
                .transpose()?;
            let bt = run_backtest(&events, &strategy, seed, coverage, carry_entry, carry_exit)?;

            // Tracker record (SIM-10): reproducible from the index alone.
            let run_id = need(rest, "--run-id")?;
            let runs_dir = need(rest, "--runs-dir")?;
            let git_sha = flag(rest, "--git-sha").unwrap_or_else(|| "unknown".into());
            let config_text = format!("strategy={strategy};seed={seed};cfg=default");
            record_run(&runs_dir, &run_id, &git_sha, &config_text, &bt, &events)?;
            Ok(ExitCode::SUCCESS)
        }
        "paper" => {
            let events = read_log(&need(rest, "--log")?)?;
            let strategy = need(rest, "--strategy")?;
            let seed: u64 = need(rest, "--seed")?.parse().map_err(|_| "bad --seed")?;
            let chunk: usize = flag(rest, "--chunk-size")
                .map_or(Ok(10_000), |c| c.parse())
                .map_err(|_| "bad --chunk-size")?;
            let run_id = need(rest, "--run-id")?;
            let runs_dir = need(rest, "--runs-dir")?;
            let git_sha = flag(rest, "--git-sha").unwrap_or_else(|| "unknown".into());
            let bt = run_paper(&events, &strategy, seed, chunk)?;
            let config_text = format!("paper;strategy={strategy};seed={seed};chunk={chunk}");
            record_run(&runs_dir, &run_id, &git_sha, &config_text, &bt, &events)?;
            Ok(ExitCode::SUCCESS)
        }
        "paper-tail" => {
            let log = need(rest, "--log")?;
            let strategy = need(rest, "--strategy")?;
            let seed: u64 = need(rest, "--seed")?.parse().map_err(|_| "bad --seed")?;
            let run_id = need(rest, "--run-id")?;
            let runs_dir = need(rest, "--runs-dir")?;
            let git_sha = flag(rest, "--git-sha").unwrap_or_else(|| "unknown".into());
            let poll_ms: u64 = flag(rest, "--poll-ms")
                .map_or(Ok(5_000), |p| p.parse())
                .map_err(|_| "bad --poll-ms")?;
            let max_idle_polls: u32 = flag(rest, "--max-idle-polls")
                .map_or(Ok(12), |p| p.parse())
                .map_err(|_| "bad --max-idle-polls")?;
            let max_polls: u32 = flag(rest, "--max-polls")
                .map_or(Ok(u32::MAX), |p| p.parse())
                .map_err(|_| "bad --max-polls")?;

            // Live tail: re-read the growing log each poll; PaperSession skips
            // already-consumed frames, so this is idempotent (SIM-15). The
            // loop is time-free (PD-3): it sleeps a fixed duration and counts
            // polls — no wall-clock reads on this decision path.
            //
            // Universe (audit 2026-08-08, P2): the strategy must be built from
            // the symbols actually present in the log, NOT `&[]` — otherwise a
            // universe-gated strategy (`carry-v1` gates every update on
            // `universe.symbols.contains(..)`) emits zero intents and the
            // session silently certifies an empty, useless run. We read the
            // log's current contents once up front to discover the universe and
            // seed the session; the close path re-reads it for `record_run`, so
            // today's tail is not extra I/O beyond what the CLI already does.
            let initial = read_log(&log)?;
            let cfg = SimConfig {
                min_coverage: 1.0,
                bar_tf_ns: 1_000_000,
                latency_ns: 0,
                fill_model: mp_sim::FillModel::L0BarFill,
                ..SimConfig::default()
            };
            let mut session = PaperSession::new(Backtester::new(
                engine(),
                strategy_named(&strategy, &initial, None, None)?,
                cfg,
                seed,
            ));
            // Seed the session with the already-present frames; subsequent
            // polls re-read the whole log and dedup on the merge key (SIM-15),
            // so seeding here is idempotent with the tail loop below.
            session
                .push_batch(initial)
                .map_err(|e| format!("paper feed refused: {e}"))?;
            let mut idle = 0u32;
            for poll in 0..max_polls {
                let mut batch = Vec::new();
                let reader = LogReader::open(std::path::Path::new(&log))
                    .map_err(|e| format!("open log {log}: {e}"))?;
                for ev in reader {
                    match ev {
                        Ok(ev) => batch.push(ev),
                        Err(e) => eprintln!("tail warn: {e}"),
                    }
                }
                let before = session.consumed;
                session
                    .push_batch(batch)
                    .map_err(|e| format!("paper feed refused: {e}"))?;
                let grew = session.consumed > before;
                println!(
                    "paper-tail poll {poll}: consumed={} dup={} idle={idle}",
                    session.consumed, session.duplicates
                );
                if grew {
                    idle = 0;
                } else {
                    idle += 1;
                    if idle >= max_idle_polls {
                        println!(
                            "paper-tail: no new frames for {max_idle_polls} polls — ending session"
                        );
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
            }
            let bt = session
                .close()
                .map_err(|e| format!("paper close refused (SIM-4): {e}"))?;
            let events = read_log(&log)?;
            let config_text = format!(
                "paper-tail;strategy={strategy};seed={seed};poll_ms={poll_ms};idle={max_idle_polls}"
            );
            record_run(&runs_dir, &run_id, &git_sha, &config_text, &bt, &events)?;
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
            // Build default strategy to read its param grid
            let default_strat = strategy_named(&strategy, &events, None, None)?;
            let param_space = default_strat.params();
            let combos = mp_sim::param_combinations(&param_space.grid);
            println!(
                "param_grid: {} combos from {:?}",
                combos.len(),
                param_space.grid.keys().collect::<Vec<_>>()
            );

            // Base sim config (same as run_backtest)
            let base_cfg = SimConfig {
                min_coverage: 1.0,
                bar_tf_ns: 1_000_000,
                latency_ns: 0,
                fill_model: mp_sim::FillModel::L0BarFill,
                ..SimConfig::default()
            };

            let first = events[0].recv_ts_ns;
            let last = events[events.len() - 1].recv_ts_ns;
            let mut train_start = first;
            let mut windows = Vec::new();
            while train_start + p.train_ns + p.test_ns <= last + 1 {
                let test_start = train_start + p.train_ns;
                let test_end = test_start + p.test_ns;
                let train = mp_sim::slice_by_recv(&events, train_start, test_start);
                let test = mp_sim::slice_by_recv(&events, test_start, test_end);

                // Grid search on train — pick params with best in-sample expectancy
                let mut best_exp = f64::NEG_INFINITY;
                let mut best_params = None;
                for combo in &combos {
                    let strat = default_strat.with_params(combo);
                    let mut bt = Backtester::new(engine(), strat, base_cfg, seed);
                    if bt.run_checked(train, 1.0).is_err() {
                        continue;
                    }
                    let exp = bt.summary().expectancy;
                    if exp > best_exp {
                        best_exp = exp;
                        best_params = Some(combo.clone());
                    }
                }

                // Run best params on test (OOS)
                let oos = if let Some(ref bp) = best_params {
                    let strat = default_strat.with_params(bp);
                    let mut bt = Backtester::new(engine(), strat, base_cfg, seed);
                    match bt.run_checked(test, 1.0) {
                        Ok(()) => bt.summary(),
                        Err(e) => {
                            eprintln!("OOS run failed: {e}");
                            MetricsSummary::default()
                        }
                    }
                } else {
                    eprintln!("no train window succeeded");
                    MetricsSummary::default()
                };

                println!(
                    "window test=[{},{}): in_exp={:+.6} best_params={:?} oos_trades={} oos_exp={:+.6} oos_stress2x={:+.6}",
                    test_start, test_end, best_exp, best_params, oos.trades, oos.expectancy, oos.stress_expectancy_2x
                );
                windows.push(WindowResult {
                    train_start_ns: train_start,
                    test_start_ns: test_start,
                    test_end_ns: test_end,
                    oos,
                });
                train_start += p.step_ns;
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
            let block_ns: i64 = flag(rest, "--block-ns")
                .map_or(Ok(86_400_000_000_000), |b| b.parse())
                .map_err(|_| "bad --block-ns")?;
            let carry_entry: Option<f64> = flag(rest, "--carry-entry")
                .map(|v| v.parse().map_err(|_| "bad --carry-entry"))
                .transpose()?;
            let carry_exit: Option<f64> = flag(rest, "--carry-exit")
                .map(|v| v.parse().map_err(|_| "bad --carry-exit"))
                .transpose()?;
            let bt = run_backtest(&events, &strategy, seed, 1.0, carry_entry, carry_exit)?;
            let mc = monte_carlo(bt.trade_pnls(), resamples, seed, block_ns);
            println!(
                "mc: resamples={} block_ns={} p50_maxDD={:.4} p95_maxDD={:.4} worst={:.4}",
                mc.resamples, block_ns, mc.p50_max_dd, mc.p95_max_dd, mc.worst_max_dd
            );
            Ok(ExitCode::SUCCESS)
        }
        "replay-live" => {
            let live = read_log(&need(rest, "--log")?)?;
            let replay = read_log(&need(rest, "--log-b")?)?;
            let strategy = need(rest, "--strategy")?;
            let seed: u64 = need(rest, "--seed")?.parse().map_err(|_| "bad --seed")?;
            let a = run_backtest(&live, &strategy, seed, 1.0, None, None)?;
            let b = run_backtest(&replay, &strategy, seed, 1.0, None, None)?;
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
