//! `footprint` — offline orderflow signal study (spec 004 footprint catalog +
//! spec 017 grading). Runs the live `FeatureEngine` + `Screener` over one
//! recorded event log, journals every rule hit with its snapshot, then
//! backfills forward returns (1h/4h) from the SAME tape with strict
//! no-look-ahead semantics (spec 017 GRD-2/GRD-4), so the Rust arm and the
//! Python grading arm read identical numbers.
//!
//! Usage:
//!   footprint --log <event.log> --run-id <id> --out-dir <dir>
//!             [--tf-ns 60000000000]
//!             [--buckets "small:0:25000,mid:25000:100000,whale:100000:inf"]
//!             [--rule "whale_imb:footprint.imb.60s.whale:ge:0.4"]...
//!
//! Deterministic: the journal date partition comes from a `SimClock` advanced
//! to each hit's event time (no wall clock anywhere, PD-3).

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, MarketEvent, SimClock};
use mp_features::catalog::{BarDelta, Cvd, FootprintDelta, FootprintImbalance, FundingRate};
use mp_features::hit_journal::{HitJournal, HitRecord};
use mp_features::{Cond, FeatureEngine, Op, Rule, Screener};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

const HOUR_NS: i64 = 3_600_000_000_000;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("missing {name}"))
}

/// Parse "name:min:max" bucket defs (max "inf" ⇒ f64::MAX).
fn parse_buckets(s: &str) -> Result<Vec<(String, f64, f64)>, String> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let bits: Vec<&str> = part.trim().split(':').collect();
        if bits.len() != 3 {
            return Err(format!("bad bucket def '{part}' (want name:min:max)"));
        }
        let max = if bits[2] == "inf" {
            f64::MAX
        } else {
            bits[2]
                .parse()
                .map_err(|_| format!("bad max in '{part}'"))?
        };
        out.push((
            bits[0].to_string(),
            bits[1].parse().map_err(|_| format!("bad min in '{part}"))?,
            max,
        ));
    }
    if out.is_empty() {
        return Err("need at least one bucket".into());
    }
    Ok(out)
}

/// Parse "rule_id:feature:op:threshold" (op ∈ gt|ge|lt|le).
fn parse_rules(specs: &[String]) -> Result<Vec<Rule>, String> {
    let mut out = Vec::new();
    for spec in specs {
        let bits: Vec<&str> = spec.split(':').collect();
        if bits.len() != 4 {
            return Err(format!(
                "bad rule '{spec}' (want rule_id:feature:op:threshold)"
            ));
        }
        let op = match bits[2] {
            "gt" => Op::Gt,
            "ge" => Op::Ge,
            "lt" => Op::Lt,
            "le" => Op::Le,
            other => return Err(format!("bad op '{other}'")),
        };
        let threshold: f64 = bits[3]
            .parse()
            .map_err(|_| format!("bad threshold in '{spec}'"))?;
        out.push(Rule {
            id: bits[0].into(),
            conds: vec![Cond {
                feature: bits[1].into(),
                op,
                threshold,
            }],
        });
    }
    if out.is_empty() {
        return Err("need at least one --rule".into());
    }
    Ok(out)
}

/// First trade price STRICTLY after `ts_ns` (GRD-4: entry is never the hit's
/// own price — no look-ahead to the trigger tick). Returns None if the tape
/// has no later trade.
fn first_after(tape: &[(i64, f64)], ts_ns: i64) -> Option<f64> {
    let i = tape.partition_point(|&(t, _)| t <= ts_ns);
    tape.get(i).map(|&(_, p)| p)
}

/// Last trade price at or before `ts_ns` (step asof — no interpolation).
fn asof(tape: &[(i64, f64)], ts_ns: i64) -> Option<f64> {
    let i = tape.partition_point(|&(t, _)| t <= ts_ns);
    if i == 0 {
        None
    } else {
        Some(tape[i - 1].1)
    }
}

/// Forward return over [hit, hit+horizon] with the strict no-look-ahead entry
/// (spec 017 GRD-4). `None` unless the tape covers the whole horizon (a tape
/// that stops early must not grade a shorter window — PD-5 honesty).
fn forward_return(tape: &[(i64, f64)], ts_ns: i64, horizon_ns: i64) -> Option<f64> {
    let entry = first_after(tape, ts_ns)?;
    let exit = asof(tape, ts_ns + horizon_ns)?;
    if tape.last().map(|&(t, _)| t).unwrap_or(0) < ts_ns + horizon_ns {
        return None;
    }
    if entry <= 0.0 {
        return None;
    }
    Some(exit / entry - 1.0)
}

fn read_log(path: &Path) -> Result<(Vec<EventEnvelope>, u64), String> {
    let reader = LogReader::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    let mut out = Vec::new();
    let mut synthetic = 0u64;
    for ev in reader {
        let ev = ev.map_err(|e| format!("read {path:?}: {e}"))?;
        if ev.provenance.stream.is_empty() && ev.provenance.subscription.is_empty() {
            synthetic += 1;
        }
        out.push(ev);
    }
    Ok((out, synthetic))
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let log = need(&args, "--log")?;
    let run_id = need(&args, "--run-id")?;
    let out_dir = need(&args, "--out-dir")?;
    let tf_ns: i64 = flag(&args, "--tf-ns")
        .map_or(Ok(60_000_000_000), |v| v.parse().map_err(|_| "bad --tf-ns"))?;
    let buckets = flag(&args, "--buckets").map_or(
        Ok(vec![
            ("small".to_string(), 0.0, 25_000.0),
            ("mid".to_string(), 25_000.0, 100_000.0),
            ("whale".to_string(), 100_000.0, f64::MAX),
        ]),
        |s| parse_buckets(&s),
    )?;
    let rule_specs: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--rule")
        .map(|(i, _)| args[i + 1].clone())
        .collect();
    let rules = parse_rules(&rule_specs)?;

    let (events, synthetic) = read_log(Path::new(&log))?;
    if events.is_empty() {
        return Err(format!("log {log} has no events"));
    }
    let venue = events[0].venue;
    let label = format!("{venue:?}");

    // ---- engine + screener (same code as live: FEA-4) -----------------------
    let mut fe = FeatureEngine::new(tf_ns);
    for (name, min, max) in &buckets {
        let (name, min, max) = (name.clone(), *min, *max);
        let (dname, iname) = (name.clone(), name.clone());
        fe.register_tick(move || Box::new(FootprintDelta::new(tf_ns, &dname, min, max)))
            .register_tick(move || Box::new(FootprintImbalance::new(tf_ns, &iname, min, max)));
    }
    fe.register_tick(move || Box::new(Cvd::new(venue)))
        .register_tick(|| Box::new(FundingRate::new()))
        .register_bar(|| Box::new(BarDelta::new("60s")));
    if !fe.offline_only_features().is_empty() {
        return Err("engine refused: offline features registered (FEA-9)".into());
    }
    let mut screener = Screener::new(rules);
    screener.set_name_map(fe.name_map().clone());
    let known: std::collections::BTreeSet<String> = fe.name_map().values().cloned().collect();
    screener
        .validate_features(&known)
        .map_err(|e| format!("rule setup refused (PD-5): {e}"))?;

    // ---- replay -------------------------------------------------------------
    let tape: Vec<(i64, f64)> = events
        .iter()
        .filter_map(|e| {
            if let MarketEvent::Trade { price, .. } = e.body {
                Some((e.recv_ts_ns, price))
            } else {
                None
            }
        })
        .collect();
    let mut hits = Vec::new();
    for ev in &events {
        for u in fe.on_event(ev) {
            hits.extend(screener.on_update(&u));
        }
    }
    // End-of-stream: close partial bars (offline runner MUST, per bar.rs).
    for u in fe.finish(events.last().map(|e| e.recv_ts_ns).unwrap_or(0)) {
        hits.extend(screener.on_update(&u));
    }

    // ---- journal + forward-return backfill (spec 017) -----------------------
    let clock = SimClock::new(events[0].recv_ts_ns);
    let hits_dir = Path::new(&out_dir).join("hits");
    let mut journal =
        HitJournal::open(&hits_dir, &clock).map_err(|e| format!("open journal: {e}"))?;
    let mut backfill: Vec<HitRecord> = Vec::new();
    let mut per_rule: BTreeMap<String, (u64, u64)> = BTreeMap::new(); // rule → (hits, graded)
    for hit in &hits {
        clock.set(hit.ts_ns);
        journal
            .record(hit.clone())
            .map_err(|e| format!("journal: {e}"))?;
        let mut rec: HitRecord = hit.clone().into();
        rec.forward_return_1h = forward_return(&tape, hit.ts_ns, HOUR_NS);
        rec.forward_return_4h = forward_return(&tape, hit.ts_ns, 4 * HOUR_NS);
        let graded = rec.forward_return_1h.is_some();
        if graded {
            backfill.push(rec);
        }
        let e = per_rule.entry(hit.rule_id.clone()).or_default();
        e.0 += 1;
        if graded {
            e.1 += 1;
        }
    }
    journal
        .write_backfill(&backfill)
        .map_err(|e| format!("backfill: {e}"))?;

    // ---- summary ------------------------------------------------------------
    let mut bucketed: BTreeMap<String, (f64, f64, u64)> = BTreeMap::new();
    for (name, min, max) in &buckets {
        bucketed.insert(name.clone(), (0.0, 0.0, 0));
        let _ = (min, max);
    }
    for ev in &events {
        if let MarketEvent::Trade {
            price, qty, side, ..
        } = ev.body
        {
            let n = price * qty;
            for (name, min, max) in &buckets {
                if n >= *min && n < *max {
                    let e = bucketed.get_mut(name).unwrap();
                    e.2 += 1;
                    match side {
                        mp_core::Side::Buy => e.0 += qty,
                        mp_core::Side::Sell => e.1 += qty,
                    }
                    break;
                }
            }
        }
    }
    println!("footprint run {run_id} over {label} log {log}");
    println!(
        "  events={} trades={} synthetic_provenance={}",
        events.len(),
        tape.len(),
        synthetic
    );
    println!("  buckets (buy_qty / sell_qty / trades):");
    for (name, (b, s, n)) in &bucketed {
        println!("    {name}: buy={b:.2} sell={s:.2} trades={n}");
    }
    println!("  rules:");
    for (rule, (total, graded)) in &per_rule {
        println!("    {rule}: hits={total} graded_1h={graded}");
    }
    let nan_suppressed = fe.nan_suppressed();
    if nan_suppressed > 0 {
        println!("  WARN: {nan_suppressed} non-finite feature outputs suppressed (FEA-5)");
    }
    if synthetic > 0 {
        println!("  NOTE: log has schema-v1 events (synthetic provenance) — research only, never promotable (INT-1/spec 024)");
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("footprint: {e}");
            ExitCode::from(2)
        }
    }
}
