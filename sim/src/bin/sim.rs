//! `sim` CLI (SIM-9/10/11/15): backtest, walk-forward, plateau, Monte-Carlo,
//! replay-live, and paper (live-feed tail / one-shot replay through the same
//! FillSimulator) — over recorded event logs, writing tracker runs
//! (`runs/index.jsonl`).
//!
//! Usage:
//!   sim backtest    --log <event.log> --strategy coinflip|null --seed N \
//!                   --run-id <ulid> --runs-dir <dir> [--coverage F] [--git-sha S]
//!   sim wf          --log <event.log> --strategy … --seed N --train-ns T --test-ns T --step-ns T [--min-trades N]
//!   sim plateau     --base <expectancy> --point <delta:expectancy> …
//!   sim mc          --log <event.log> --strategy … --seed N --resamples R
//!   sim replay-live --log <live.log> --log-b <replay.log> --strategy … --seed N
//!   sim paper       --log <live.log> --strategy … --seed N \
//!                   --run-id <ulid> --runs-dir <dir> [--chunk-size N] [--zero-intents]
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
use mp_features::catalog::{
    BookDepth, BookDepthKind, Cvd, FundingRate, LiqDist, LiqRate, LiqVol, TapeBpsDelta,
};
use mp_features::FeatureEngine;
use mp_features::LiqDelta;
use mp_sim::{
    bars_per_year as mp_bars_per_year, monte_carlo, plateau_ok, strategy_named, Backtester,
    MetricsSummary, PaperSession, RunRecord, SimConfig, WalkForwardParams, WindowResult,
};
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

/// The sim-side strategy-visible feature set (spec 004/006): every feature a
/// strategy can subscribe to in a backtest. The order-flow family is the
/// `book.depth.*` liquidity bands + the `tape.bps_delta` aggressive-tape
/// print (Cryexc/OpenMarket additions, 2026-08-13); `cvd.{venue}` and
/// `funding.rate` serve the earlier strategies. `tape.tps.{tf}` is a BAR
/// feature and is deliberately not registered here — the sim's `bar_tf_ns`
/// is 1ms, which would make a "trades-per-<1ms>" reading meaningless.
fn engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new(Venue::Hyperliquid)));
    e.register_tick(|| Box::new(FundingRate::new()));
    // Liquidity-band depth (default bands 0.5% / 2% / 10% of mid).
    for &pct in &[0.005, 0.02, 0.1] {
        e.register_tick(move || Box::new(BookDepth::new(pct, BookDepthKind::Gauge)));
        e.register_tick(move || Box::new(BookDepth::new(pct, BookDepthKind::Total)));
    }
    e.register_tick(|| Box::new(TapeBpsDelta::default()));
    // Liquidation flow (COL-29 real liq source, spec 004 §Liquidation flow):
    // rolling notional by side + rolling event rate over a 5-minute window,
    // plus liq price-distance from mid. No-op while no recording carries a
    // liquidation stream (hyperliquid today) — they light up when a bybit
    // recording joins.
    e.register_tick(|| Box::new(LiqVol::new(300_000_000_000, mp_core::Side::Buy)));
    e.register_tick(|| Box::new(LiqVol::new(300_000_000_000, mp_core::Side::Sell)));
    e.register_tick(|| Box::new(LiqRate::new(300_000_000_000)));
    e.register_tick(|| Box::new(LiqDist::default()));
    // Cross-venue liquidation-pressure divergence (spec 004 §Liquidation
    // flow): bybit vs binance — the two COL-29 liq-capable venues. No-op
    // until both legs record. Global registration: one instance must see
    // both venues (spec 023 FEA-20).
    e.register_global_tick(|| {
        Box::new(LiqDelta::new(
            300_000_000_000,
            Venue::Bybit,
            Venue::BinanceFutures,
        ))
    });
    e
}

fn run_backtest(
    events: &[EventEnvelope],
    strategy: &str,
    seed: u64,
    coverage: f64,
    carry_entry: Option<f64>,
    carry_exit: Option<f64>,
    bar_tf_ns: i64,
) -> Result<Backtester, String> {
    let cfg = SimConfig {
        min_coverage: coverage,
        bar_tf_ns,
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
            // SWG-5 bar replay: bar timeframe for L0 bar fills + bar-return
            // Sharpe sampling. Default 1ms keeps legacy behavior; daily =
            // 86_400_000_000_000, 4h = 14_400_000_000_000.
            let bar_tf_ns: i64 = flag(rest, "--bar-tf-ns")
                .map_or(Ok(1_000_000), |v| v.parse())
                .map_err(|_| "bad --bar-tf-ns")?;
            let bt = run_backtest(
                &events,
                &strategy,
                seed,
                coverage,
                carry_entry,
                carry_exit,
                bar_tf_ns,
            )?;

            // Tracker record (SIM-10): reproducible from the index alone.
            let run_id = need(rest, "--run-id")?;
            let runs_dir = need(rest, "--runs-dir")?;
            let git_sha = flag(rest, "--git-sha").unwrap_or_else(|| "unknown".into());
            let config_text = format!(
                "strategy={strategy};seed={seed};cfg=default;coverage={coverage};carry_entry={carry_entry:?};carry_exit={carry_exit:?}"
            );
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
            // PAP-4: --zero-intents overrides the strategy to NullStrategy,
            // so the session still runs (bars built, events consumed) but
            // emits zero intents. The PS1 script passes this when the
            // kill-latch is tripped.
            let effective_strategy = if flag(rest, "--zero-intents").is_some() {
                "null"
            } else {
                strategy.as_str()
            };
            let bt = run_paper(&events, effective_strategy, seed, chunk)?;
            let config_text = if flag(rest, "--zero-intents").is_some() {
                format!("paper;strategy={strategy};seed={seed};chunk={chunk};zero_intents=true")
            } else {
                format!("paper;strategy={strategy};seed={seed};chunk={chunk}")
            };
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
                // SWG-5 purged split: gap between train and test slices. 0
                // keeps the legacy contiguous behavior.
                embargo_ns: flag(rest, "--embargo-ns")
                    .map_or(Ok(0), |v| v.parse())
                    .map_err(|_| "bad --embargo-ns")?,
            };
            // SIM-9 integrity: a grid combo must actually trade to be eligible
            // for selection. Without the bar, every combo that never trades
            // scores exactly 0.0 and beats every combo that trades and loses
            // (0.0 > -exp), so the argmax picks a vacuous "don't trade" combo
            // and the OOS test is meaningless — the degeneracy observed on the
            // first real-data walk-forward (2026-08-15). The window is then
            // marked VACUOUS instead of reporting a false pass.
            let min_trades: u64 = flag(rest, "--min-trades")
                .map_or(Ok(10), |v| v.parse())
                .map_err(|_| "bad --min-trades")?;
            // Build default strategy to read its param grid
            let default_strat = strategy_named(&strategy, &events, None, None)?;
            let param_space = default_strat.params();
            let combos = mp_sim::param_combinations(&param_space.grid);
            println!(
                "param_grid: {} combos from {:?}",
                combos.len(),
                param_space.grid.keys().collect::<Vec<_>>()
            );

            // Base sim config (same as run_backtest). SWG-5 bar replay: --bar-tf-ns
            // picks the bar timeframe (daily/4h) for L0 bar fills + bar-return
            // Sharpe sampling; default 1ms keeps legacy behavior.
            let bar_tf_ns: i64 = flag(rest, "--bar-tf-ns")
                .map_or(Ok(1_000_000), |v| v.parse())
                .map_err(|_| "bad --bar-tf-ns")?;
            let base_cfg = SimConfig {
                min_coverage: 1.0,
                bar_tf_ns,
                ..SimConfig::default()
            };
            // SWG-5 Deflated Sharpe: bars per year for the bar-return Sharpe
            // annualization (365 daily, 2190 4h, 525600 60s). The 1ms bar
            // replay default matches the base_cfg bar_tf_ns.
            let bars_per_year: f64 = flag(rest, "--bars-per-year")
                .map_or_else(|| Ok(mp_bars_per_year(base_cfg.bar_tf_ns)), |v| v.parse())
                .map_err(|_| "bad --bars-per-year")?;

            let mut windows = Vec::new();
            // SWG-5: the harness rolls purged (train|embargo|test) windows.
            // Each window runs the grid on train, picks the best eligible combo
            // (SIM-9), and runs it OOS on test. The closure returns the OOS
            // summary; `p.embargo_ns` inserts a gap between train and test that
            // is in neither slice — blocking label-leakage across the boundary.
            let _ = mp_sim::walk_forward(
                &events,
                p,
                |_train_start, test_start, test_end, train, test| {
                    // Grid search on train — pick params with best in-sample
                    // expectancy among combos that actually trade enough to judge
                    // (SIM-9: a 0-trade combo scoring 0.0 must never beat a combo
                    // that trades and loses, nor count as a selection).
                    let (best_params, best_exp) =
                        mp_sim::pick_best_eligible(&combos, min_trades, |combo| {
                            let strat = default_strat.with_params(combo);
                            let mut bt = Backtester::new(engine(), strat, base_cfg, seed);
                            if bt.run_checked(train, 1.0).is_err() {
                                return None;
                            }
                            Some(bt.summary())
                        });

                    // Run best params on test (OOS). Three verdicts: SELECTED
                    // (ran clean), ERROR (the OOS run itself failed — must never
                    // read as a SELECTED 0-trade pass, audit M9), VACUOUS (nothing
                    // eligible on train — no selection, OOS meaningless).
                    let (oos, vacuous, error) = if let Some(ref bp) = best_params {
                        let strat = default_strat.with_params(bp);
                        let mut bt = Backtester::new(engine(), strat, base_cfg, seed);
                        match bt.run_checked(test, 1.0) {
                            Ok(()) => (bt.summary(), false, false),
                            Err(e) => {
                                eprintln!("OOS run failed: {e}");
                                (MetricsSummary::default(), false, true)
                            }
                        }
                    } else {
                        eprintln!(
                        "window VACUOUS: no grid combo reached min_trades={min_trades} on train — no selection, OOS meaningless"
                    );
                        (MetricsSummary::default(), true, false)
                    };
                    let verdict = if error {
                        "ERROR"
                    } else if vacuous {
                        "VACUOUS"
                    } else {
                        "SELECTED"
                    };

                    let shrp = oos
                        .sharpe
                        .map(|s| format!("{s:+.3}"))
                        .unwrap_or_else(|| "NA".into());
                    let dshr = oos
                        .deflated_sharpe(bars_per_year, combos.len() as u64)
                        .map(|d| format!("{d:+.3}"))
                        .unwrap_or_else(|| "NA".into());
                    println!(
                    "window test=[{},{}): verdict={verdict} in_exp={:+.6} best_params={:?} oos_trades={} oos_exp={:+.6} oos_stress2x={:+.6} oos_sharpe={shrp} oos_deflated_sharpe={dshr}",
                    test_start, test_end,
                    best_exp, best_params, oos.trades, oos.expectancy, oos.stress_expectancy_2x
                );
                    windows.push(WindowResult {
                        train_start_ns: test_start - p.train_ns - p.embargo_ns,
                        test_start_ns: test_start,
                        test_end_ns: test_end,
                        oos,
                        vacuous,
                        error,
                    });
                    oos
                },
            );
            let vacuous = windows.iter().filter(|w| w.vacuous).count();
            let error = windows.iter().filter(|w| w.error).count();
            println!(
                "wf: windows={} vacuous={} error={} selected={}",
                windows.len(),
                vacuous,
                error,
                windows.len() - vacuous - error
            );
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
            // SWG-5 bar replay: the Monte-Carlo path uses the same bar
            // timeframe as the backtest arm.
            let bar_tf_ns: i64 = flag(rest, "--bar-tf-ns")
                .map_or(Ok(1_000_000), |v| v.parse())
                .map_err(|_| "bad --bar-tf-ns")?;
            let bt = run_backtest(
                &events,
                &strategy,
                seed,
                1.0,
                carry_entry,
                carry_exit,
                bar_tf_ns,
            )?;
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
            let a = run_backtest(&live, &strategy, seed, 1.0, None, None, 1_000_000)?;
            let b = run_backtest(&replay, &strategy, seed, 1.0, None, None, 1_000_000)?;
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
