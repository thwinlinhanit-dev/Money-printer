//! `footprint` — offline orderflow signal study (spec 004 footprint catalog +
//! spec 017 grading, MIGRATED 2026-09-07 to the research-lab observation flow
//! per spec 054 REL-31). Runs the live `FeatureEngine` + `Screener` over one
//! recorded event log, journals every rule hit with its snapshot (the JSONL
//! hit journal remains the WRITE-side fire log), then bridges every hit onto
//! an identity-stamped `SignalObservation` whose forward outcomes (gross AND
//! net of the cost model, MFE/MAE) are attached by the Phase-4 outcome engine
//! from the SAME tape and persisted as Parquet (W-6 guard, identity footer).
//! Per-rule evaluation reports print the tiered gate verdict (`evaluate` /
//! `decide`, spec 054 REL-15..19/24..29).
//!
//! RELIQUISHED: the legacy JSONL forward-return backfill (gross-only, no
//! identity) is retired — spec 017's amendment 2026-09-07 records the
//! adjudication: the outcome engine's entry convention (last mark at-or-
//! before the fire time, REL-14) governs research artifacts; GRD-4's
//! "first trade strictly after" stays binding for EXECUTION/fill studies.
//!
//! Usage:
//!   footprint --log <event.log> --run-id <id> --out-dir <dir>
//!             --params-hash <hash>          (part of the research identity)
//!             [--round-trip-cost 0.00075]   (fraction; sim default taker+maker)
//!             [--tf-ns 60000000000]
//!             [--buckets "small:0:25000,mid:25000:100000,whale:100000:inf"]
//!             [--rule "whale_imb:footprint.imb.60s.whale:ge:0.4"]...
//!
//! Deterministic: the journal date partition comes from a `SimClock` advanced
//! to each hit's event time; observation ids derive from the identity
//! fingerprint + ts + hit sequence (no wall clock anywhere, PD-3).
//!
//! Identity: one per rule — `signal_id = rule_id`,
//! `params_hash = "<--params-hash>|<rule spec>"` (the rule IS a parameter),
//! `cost_model_hash = "rt-<round_trip_cost>"`. Direction defaults to Long (a
//! screener hit carries no side); the params-hash discloses it via the
//! documented convention (spec 054 W-5 2026-09-07).

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, SimClock};
use mp_features::catalog::{BarDelta, Cvd, FootprintDelta, FootprintImbalance, FundingRate};
use mp_features::hit_journal::{hit_to_observation, HitJournal};
use mp_features::signal_identity::SignalResearchIdentity;
use mp_features::{Cond, FeatureEngine, Op, Rule, Screener};
use mp_storage::observation_store::partitioned_write;
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
            bits[1].parse().map_err(|_| format!("bad min in '{part}'"))?,
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
    let params_hash = need(&args, "--params-hash")?;
    let round_trip_cost: f64 = flag(&args, "--round-trip-cost")
        .map_or(Ok(0.00075), |v| v.parse().map_err(|_| "bad --round-trip-cost"))?;
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
            if let Some((price, _, _, _, _)) = e.body.trade_view() {
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

    // ---- journal (write-side fire log, retained) + observation bridge -------
    // REL-31: hits journal to JSONL as before, then EACH hit becomes an
    // identity-stamped SignalObservation (refusing venue-less hits, R-1);
    // outcomes (gross AND net, MFE/MAE) attach from the trade tape with the
    // Phase-4 engine's no-lookahead semantics, and the whole set persists as
    // Parquet. The legacy gross-only backfill JSONL is NO LONGER PRODUCED.
    let clock = SimClock::new(events[0].recv_ts_ns);
    let hits_dir = Path::new(&out_dir).join("hits");
    let mut journal =
        HitJournal::open(&hits_dir, &clock).map_err(|e| format!("open journal: {e}"))?;
    let cost_model_hash = format!("rt-{round_trip_cost}");
    // One identity per rule: the rule spec IS part of the params (W-5).
    let mut identities: BTreeMap<String, SignalResearchIdentity> = BTreeMap::new();
    for spec in &rule_specs {
        let rule_id = spec.split(':').next().unwrap_or(spec).to_string();
        identities.insert(
            rule_id.clone(),
            SignalResearchIdentity::new(
                rule_id,
                1, // feature_version (engine catalog version at write time)
                format!("{params_hash}|{spec}"),
                cost_model_hash.clone(),
            ),
        );
    }

    let mut observations: Vec<mp_features::SignalObservation> = Vec::new();
    let mut per_rule: BTreeMap<String, (u64, u64)> = BTreeMap::new(); // rule → (hits, with outcomes)
    for (seq, hit) in hits.iter().enumerate() {
        clock.set(hit.ts_ns);
        journal
            .record(hit.clone())
            .map_err(|e| format!("journal: {e}"))?;
        let identity = identities.get(&hit.rule_id).ok_or_else(|| {
            format!("internal: no identity for rule {}", hit.rule_id)
        })?;
        let obs = hit_to_observation(hit, identity, events[0].recv_ts_ns, seq as u64)
            .map_err(|e| format!("observation bridge: {e}"))?;
        let e = per_rule.entry(hit.rule_id.clone()).or_default();
        e.0 += 1;
        observations.push(obs);
    }
    // Phase-4 outcomes AFTER the replay (no lookahead, REL-13/14): the tape
    // must cover the whole horizon or the outcome does not exist.
    let horizons = [HOUR_NS, 4 * HOUR_NS];
    let observations = mp_features::outcome::attach_outcomes(
        &observations,
        &tape,
        &horizons,
        round_trip_cost,
    );
    for o in &observations {
        if !o.outcomes.is_empty() {
            if let Some(e) = per_rule.get_mut(&o.identity.signal_id) {
                e.1 += 1;
            }
        }
    }
    // REL-34 layout note: partitioned_write appends `observations/<fp>` to
    // the ROOT itself — pass out_dir directly (the sim CLI convention); a
    // pre-joined `out_dir/observations` double-nests the identity dirs.
    let paths = partitioned_write(Path::new(&out_dir), &observations)
        .map_err(|e| format!("observation write: {e}"))?;
    let obs_dir = Path::new(&out_dir).join("observations");

    // ---- evaluation reports (per rule, per horizon) --------------------------
    for (rule_id, identity) in &identities {
        let rule_obs: Vec<&mp_features::SignalObservation> = observations
            .iter()
            .filter(|o| &o.identity.signal_id == rule_id)
            .collect();
        if rule_obs.is_empty() {
            println!("rule {rule_id}: no hits");
            continue;
        }
        let owned: Vec<mp_features::SignalObservation> =
            rule_obs.into_iter().cloned().collect();
        println!("rule {rule_id} (identity {}):", identity.fingerprint());
        for &h in &horizons {
            let rep = mp_features::evaluate(&owned, h, mp_features::DEFAULT_MIN_N);
            let dec = mp_features::decide(&rep);
            println!(
                "  horizon {:>6}s: n={} tier={} gross_exp={:+.6} net_exp={:+.6} win={:.3} p25={:.4} p50={:.4} p75={:.4} | {}",
                h / 1_000_000_000,
                rep.n,
                rep.tier.label(),
                rep.gross_expectancy,
                rep.net_expectancy,
                rep.win_rate,
                rep.p25,
                rep.p50,
                rep.p75,
                if rep.gate_passed {
                    format!("GATE PASS — {}", dec.reasons.join("; "))
                } else {
                    format!("GATE REFUSED — {}", dec.reasons.join("; "))
                }
            );
            println!("    decision: {}", dec.to_json());
        }
    }

    // ---- summary ------------------------------------------------------------
    let mut bucketed: BTreeMap<String, (f64, f64, u64)> = BTreeMap::new();
    for (name, min, max) in &buckets {
        bucketed.insert(name.clone(), (0.0, 0.0, 0));
        let _ = (min, max);
    }
    for ev in &events {
        if let Some((price, qty, side, _, _)) = ev.body.trade_view() {
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
    for (rule, (total, with_outcomes)) in &per_rule {
        println!("    {rule}: hits={total} with_outcomes={with_outcomes}");
    }
    println!(
        "  observations: {} recorded, {} parquet file(s) in {} (round_trip_cost={round_trip_cost})",
        observations.len(),
        paths.len(),
        obs_dir.display()
    );
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
