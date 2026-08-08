//! `mp-ops` — operational CLI for the Money Printer system (SPEC-011).
//!
//! Subcommands:
//!   compact --date YYYY-MM-DD --venue bybit --symbol BTCUSDT
//!           Compacts a raw event log into partitioned Parquet files.
//!           Date also accepts YYYYMMDD (backward compatible).
//!   band-accuracy-decay --trend PATH [--runs-dir DIR] [--dedupe-ns N] [--telegram]
//!           OPS-13 drift/decay watch over the RES-4 band-accuracy trend
//!           journal (research/band_accuracy/band_accuracy.jsonl): prints a
//!           JSON verdict {decayed: bool, alert: {...}}; exit 2 on a corrupt
//!           journal (fail-closed), never a fabricated verdict. With
//!           --runs-dir, the weekly verdict is journaled to
//!           <runs-dir>/index.jsonl as its own `band_accuracy_decay` record
//!           line (RES-4 tracker, append-only W-6), correlated to the study's
//!           run record by run_id + week from the latest trend line. With
//!           --telegram, a fired alert is routed through the framework's
//!           dedupe + quiet-hours batching (OPS-9) and delivered to Telegram
//!           (TELEGRAM_BOT_TOKEN/TELEGRAM_CHAT_ID, MP_OPS_TELEGRAM_URL,
//!           MP_OPS_TELEGRAM_DIR, MP_OPS_QUIET_START_MIN/END_MIN): sent now,
//!           batched to journal/telegram/batch.jsonl during quiet hours, or
//!           gated as "unconfigured" without credentials.
//!   telegram-flush [--dir DIR] [--wait]
//!           Drain the P3 quiet-hours batch ledger to Telegram (fail-closed:
//!           any send failure keeps the batch and exits 2). With --wait, the
//!           drain is held until quiet hours end — sleeping only while the
//!           window is active (MP_OPS_QUIET_START_MIN/END_MIN; MP_OPS_SLEEP
//!           overrides the sleeper, a command receiving the seconds, so tests
//!           never block) — the weekly wrapper's quiet-hours wait, moved
//!           inside the binary. Every successful send is appended to
//!           journal/telegram/delivered.jsonl ({id, delivered_ts_ns}) — the
//!           delivery log the monthly report renders (OPS-6): what WAS
//!           delivered, as opposed to what is still pending.
//!   p1-webhook --id ID --detail TEXT [--ts-ns N]
//!           P1 egress edge (owner decision 2026-08-06, audit 08-04 #3):
//!           posts a P1 dispatch to the owner-configured webhook sink
//!           (MP_OPS_P1_WEBHOOK) via curl — TLS is the host's, so https
//!           works (audit 08-04 #9, never forced cleartext). The channel is
//!           WIRED but dead until credentials exist: URL unset ⇒ the command
//!           fails loudly (exit 2) with the "dead until creds" reason — never
//!           a silent drop, never a fake send. --ts-ns injects the dispatch
//!           timestamp (default now; PD-3 edge clock).
//!   telegram-stale [--dir DIR] [--threshold-hours N] [--dedupe-ns N] [--telegram]
//!           OPS-14 near-real-time watch on the quiet-hours batch ledger:
//!           raises telegram-stale (P2) when a dispatch sits queued longer
//!           than one quiet window (default 24h — a missed telegram-flush),
//!           printing a JSON verdict {stale, pending, alert} (pending = the
//!           number of dispatches in the batch); exit 2 on a corrupt batch
//!           (fail-closed), never a fabricated verdict. With --telegram, a
//!           fired P2 is sent immediately (P2 always breaks through quiet
//!           hours, OPS-9).
//!
//! Usage:
//!   cargo run --package mp-ops --bin mp-ops -- compact --date 2026-07-15 --venue bybit --symbol BTCUSDT
//!   cargo run --package mp-ops --bin mp-ops -- band-accuracy-decay --trend research/band_accuracy/band_accuracy.jsonl
//!   cargo run --package mp-ops --bin mp-ops -- band-accuracy-decay --trend research/band_accuracy/band_accuracy.jsonl --runs-dir runs --telegram
//!   cargo run --package mp-ops --bin mp-ops -- telegram-flush --wait
//!   cargo run --package mp-ops --bin mp-ops -- telegram-stale --telegram
//!   MP_OPS_P1_WEBHOOK=https://hooks.example.com/alert cargo run --package mp-ops --bin mp-ops -- \
//!     p1-webhook --id recon-diverged --detail 'BTCUSDT position mismatch'

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, SymbolTable, Venue};
use mp_ops::{
    append_batch, append_run_record, band_accuracy_decay_alert, flush_batch,
    load_band_accuracy_trend, load_telegram_batch, post_telegram, stale_batch_alert, Alert,
    AlertRouter, Dispatch, QuietHours, RouteOutcome, Severity, TelegramConfig,
};
use mp_storage::promotion::check_promotion;
use mp_storage::{audit_raw_log, compactor, AuditConfig, DailyScorecard, RawLogAudit};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const COMPACTOR_VERSION: &str = "0.1.0";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("missing {name}"))
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .collect()
}

fn parse_venue(s: &str) -> Result<Venue, String> {
    match s {
        "bybit" => Ok(Venue::Bybit),
        "binance" => Ok(Venue::BinanceFutures),
        "okx" => Ok(Venue::Okx),
        "hyperliquid" => Ok(Venue::Hyperliquid),
        "coinbase" => Ok(Venue::Coinbase),
        "kraken" | "kraken_futures" => Ok(Venue::KrakenFutures),
        "deribit" => Ok(Venue::Deribit),
        "fred" => Ok(Venue::Fred),
        _ => Err(format!("unknown venue: {s}")),
    }
}

fn date_to_nanos(date: &str) -> Result<(i64, i64), String> {
    // Accept both YYYYMMDD and YYYY-MM-DD.
    let norm = date.replace('-', "");
    if norm.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let y: i64 = norm[0..4].parse().map_err(|_| "bad year")?;
    let m: u32 = norm[4..6].parse().map_err(|_| "bad month")?;
    let d: u32 = norm[6..8].parse().map_err(|_| "bad day")?;

    // Days since Unix epoch for this date
    let days = days_from_epoch(y, m, d);
    let day_start_ns = (days as i64) * 86_400_000_000_000;
    let day_end_ns = day_start_ns + 86_400_000_000_000;
    Ok((day_start_ns, day_end_ns))
}

fn days_from_epoch(y: i64, m: u32, d: u32) -> u64 {
    // Count days from 1970-01-01
    let mut total = 0u64;
    for year in 1970..y {
        total += if is_leap(year) { 366 } else { 365 };
    }
    let months = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for &days in &months[..m as usize - 1] {
        total += days as u64;
    }
    total += (d - 1) as u64;
    total
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn compute_source_hash(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let hash = crc32fast::hash(&data);
    Ok(format!("{:08x}", hash))
}

fn read_log_with_symbols(path: &Path) -> Result<(Vec<EventEnvelope>, SymbolTable), String> {
    let mut reader = LogReader::open(path).map_err(|e| format!("open log {path:?}: {e}"))?;
    let mut events = Vec::new();
    for ev in &mut reader {
        events.push(ev.map_err(|e| format!("read log {path:?}: {e}"))?);
    }
    let metas = reader.symbols().to_vec();
    let symbols = SymbolTable::from_metas(metas);
    Ok((events, symbols))
}

fn audit_config(args: &[String], venue: Venue, symbol: &str) -> Result<AuditConfig, String> {
    let mut config = AuditConfig::single(venue, symbol);
    if let Some(seconds) = flag(args, "--max-gap-sec") {
        let seconds: i64 = seconds
            .parse()
            .map_err(|_| "--max-gap-sec must be an integer")?;
        if seconds <= 0 {
            return Err("--max-gap-sec must be positive".into());
        }
        config.max_gap_ns = seconds.saturating_mul(1_000_000_000);
    }
    config.required_streams = flags(args, "--require-stream")
        .into_iter()
        .collect::<BTreeSet<_>>();
    Ok(config)
}

fn clean_audit(path: &Path, config: &AuditConfig) -> Result<RawLogAudit, String> {
    let audit = audit_raw_log(path, config);
    if audit.is_clean() {
        Ok(audit)
    } else {
        let reasons = audit
            .findings
            .iter()
            .map(|finding| format!("{}: {}", finding.code, finding.detail))
            .collect::<Vec<_>>()
            .join("; ");
        Err(format!("quarantined raw log {}: {reasons}", path.display()))
    }
}

fn cmd_compact(args: &[String]) -> Result<String, String> {
    let date_raw = need(args, "--date")?;
    // Normalize: strip dashes for raw log filename, build dashed for partition paths.
    let date_flat = date_raw.replace('-', "");
    if date_flat.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let date_dashed = format!(
        "{}-{}-{}",
        &date_flat[0..4],
        &date_flat[4..6],
        &date_flat[6..8]
    );
    let venue_str = need(args, "--venue")?;
    let symbol = need(args, "--symbol")?;
    let venue = parse_venue(&venue_str)?;

    let raw_path = Path::new("data")
        .join("raw")
        .join(format!("{date_flat}_{venue_str}_{symbol}.log"));
    let cold_root = Path::new("data").join("cold");

    if !raw_path.exists() {
        return Err(format!("raw log not found: {}", raw_path.display()));
    }

    // INT-4: no compaction is allowed to turn contaminated raw data into a
    // trustworthy-looking cold dataset.  The audit runs before reading events.
    let config = audit_config(args, venue, &symbol)?;
    let audit = clean_audit(&raw_path, &config)?;

    tracing::info!(path = %raw_path.display(), "compacting raw log");

    let (day_start_ns, day_end_ns) = date_to_nanos(&date_flat)?;
    let source_hash = compute_source_hash(&raw_path)?;
    let (events, symbols) = read_log_with_symbols(&raw_path)?;

    let created_ts_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64;

    std::fs::create_dir_all(&cold_root).map_err(|e| format!("create {cold_root:?}: {e}"))?;

    // Compact through the verified INT-4 gate (spec 024 08-03 decision): the
    // same `compact_day_verified` that refuses quarantined logs drives ingress.
    let stats = compactor::compact_day_verified(
        &cold_root,
        venue,
        &date_dashed,
        day_start_ns,
        day_end_ns,
        events,
        &symbols,
        &source_hash,
        COMPACTOR_VERSION,
        created_ts_ns,
        &audit,
    )
    .map_err(|e| format!("compact_day_verified failed: {e}"))?;

    let mut extra = String::new();
    if stats.positions_files_written + stats.positions_files_skipped > 0 {
        extra.push_str(&format!(
            ", {} positions files ({} rows)",
            stats.positions_files_written, stats.position_rows
        ));
    }
    if stats.macro_files_written + stats.macro_files_skipped > 0 {
        extra.push_str(&format!(
            ", {} macro files ({} rows)",
            stats.macro_files_written, stats.macro_rows
        ));
    }
    if stats.options_files_written + stats.options_files_skipped > 0 {
        extra.push_str(&format!(
            ", {} options files ({} rows)",
            stats.options_files_written, stats.option_rows
        ));
    }
    Ok(format!(
        "compact done: {} trade files written ({} rows), {} files skipped{extra}",
        stats.trades_files_written, stats.trade_rows, stats.trades_files_skipped
    ))
}

fn cmd_audit(args: &[String]) -> Result<String, String> {
    let date = need(args, "--date")?.replace('-', "");
    if date.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let venue_name = need(args, "--venue")?;
    let symbol = need(args, "--symbol")?;
    let venue = parse_venue(&venue_name)?;
    let path = Path::new("data")
        .join("raw")
        .join(format!("{date}_{venue_name}_{symbol}.log"));
    let audit = audit_raw_log(&path, &audit_config(args, venue, &symbol)?);
    serde_json::to_string_pretty(&audit).map_err(|error| error.to_string())
}

/// Lightweight scorecard file schema (data/scorecards/YYYY-MM-DD.json).
/// Written by `mp-ops scorecard`; read by `mp-ops promote`.  Deliberately does
/// NOT carry the full findings Vec — legacy days hold millions of findings and
/// serializing them balloons the file to GB scale (the same defect fixed in
/// mp-audit --json on 2026-08-04).  The gate only needs date, clean flags and
/// counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScorecardFile {
    date: String,
    promotable: bool,
    recordings: Vec<ScorecardFileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScorecardFileEntry {
    venue: String,
    symbol: String,
    clean: bool,
    #[serde(default)]
    event_count: u64,
    #[serde(default)]
    coverage: f64,
    #[serde(default)]
    findings: usize,
    #[serde(default)]
    blocking_findings: usize,
}

/// Lightweight per-recording entry (no findings Vec — see `ScorecardFile`).
fn light_entry(venue: &Venue, symbol: &str, audit: &RawLogAudit) -> ScorecardFileEntry {
    let mut blocking = 0usize;
    for f in &audit.findings {
        if mp_storage::audit::is_blocking_finding(&f.code) {
            blocking += 1;
        }
    }
    ScorecardFileEntry {
        venue: venue.slug().to_owned(),
        symbol: symbol.to_owned(),
        clean: audit.is_clean(),
        event_count: audit.event_count,
        coverage: audit.coverage,
        findings: audit.findings.len(),
        blocking_findings: blocking,
    }
}

fn cmd_promote(args: &[String]) -> Result<String, String> {
    let dir = flag(args, "--scorecards-dir").unwrap_or_else(|| "data/scorecards".to_string());
    let required = flags(args, "--required")
        .into_iter()
        .collect::<BTreeSet<_>>();
    let path = PathBuf::from(&dir);
    if !path.is_dir() {
        return Err(format!("scorecards directory not found: {dir}"));
    }
    // Load every YYYY-MM-DD.json scorecard, sorted by date.
    let mut files: Vec<ScorecardFile> = Vec::new();
    for entry in std::fs::read_dir(&path)
        .map_err(|e| format!("read {dir}: {e}"))?
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_string();
        // Accept YYYY-MM-DD.json and YYYYMMDD.json (normalize on load).
        let stem = match name.rsplit_once('.') {
            Some((s, "json")) => s.to_string(),
            _ => continue,
        };
        if !(stem.len() == 10 && stem.contains('-') || stem.len() == 8) {
            continue;
        }
        let text = std::fs::read_to_string(entry.path())
            .map_err(|e| format!("read {}: {e}", entry.path().display()))?;
        // Strip a UTF-8 BOM: the daily pipeline writes scorecards via
        // PowerShell Set-Content -Encoding UTF8, which emits one.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let mut card: ScorecardFile = serde_json::from_str(text)
            .map_err(|e| format!("parse {}: {e}", entry.path().display()))?;
        // Normalize date to YYYY-MM-DD so ordering and the verdict are stable.
        if card.date.len() == 8 && !card.date.contains('-') {
            card.date = format!(
                "{}-{}-{}",
                &card.date[0..4],
                &card.date[4..6],
                &card.date[6..8]
            );
        }
        files.push(card);
    }
    files.sort_by(|a, b| a.date.cmp(&b.date));
    if files.is_empty() {
        return Err(format!("no scorecards found in {dir}"));
    }
    if !required.is_empty() {
        // Filter to days that include every required venue:symbol recording.
        // Invariant: the daily pipeline always generates scorecards with the
        // FULL required set (a missing raw log audits as unreadable_log =>
        // clean=false => promotable=false), so no day is ever silently
        // dropped from the streak here.
        files.retain(|card| {
            required.iter().all(|req| {
                let (venue, symbol) = req.split_once(':').unwrap_or((req.as_str(), ""));
                card.recordings
                    .iter()
                    .any(|r| r.venue == venue && r.symbol == symbol)
            })
        });
        if files.is_empty() {
            return Err("no scorecards cover the required venue:symbol set".into());
        }
    }
    // Feed the gate only what it reads: date + promotable.
    let scorecards: Vec<DailyScorecard> = files
        .into_iter()
        .map(|f| DailyScorecard {
            date: f.date,
            recordings: vec![],
            promotable: f.promotable,
        })
        .collect();
    let verdict = check_promotion(&scorecards);
    let summary = if verdict.promoted {
        format!(
            "PROMOTED: {} consecutive clean days {}..{}",
            verdict.consecutive_clean,
            verdict.window_start.as_deref().unwrap_or("?"),
            verdict.window_end.as_deref().unwrap_or("?")
        )
    } else {
        let why = verdict
            .first_failure
            .as_deref()
            .map(|d| format!("first break {d}"))
            .unwrap_or_else(|| "no qualifying clean window yet".to_string());
        format!(
            "NOT YET: {} consecutive clean day(s) of {} required ({why})",
            verdict.consecutive_clean, verdict.required
        )
    };
    // Return the JSON verdict as the command's message: `main` prints it
    // exactly once (the tracing line is silenced by RUST_LOG=off in callers).
    serde_json::to_string(&serde_json::json!({
        "promoted": verdict.promoted,
        "consecutive_clean": verdict.consecutive_clean,
        "required": verdict.required,
        "window_start": &verdict.window_start,
        "window_end": &verdict.window_end,
        "first_failure": &verdict.first_failure,
        "scorecards": scorecards.len(),
        "summary": summary,
    }))
    .map_err(|e| e.to_string())
}

fn cmd_scorecard(args: &[String]) -> Result<String, String> {
    let date = need(args, "--date")?.replace('-', "");
    if date.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let required = flags(args, "--required");
    if required.is_empty() {
        return Err("scorecard requires one or more --required venue:symbol entries".into());
    }
    let mut entries = Vec::new();
    for item in required {
        let (venue_name, symbol) = item
            .split_once(':')
            .ok_or_else(|| format!("invalid --required {item}; expected venue:symbol"))?;
        let venue = parse_venue(venue_name)?;
        let path = Path::new("data")
            .join("raw")
            .join(format!("{date}_{venue_name}_{symbol}.log"));
        let audit = audit_raw_log(&path, &audit_config(args, venue, symbol)?);
        entries.push((venue, symbol.to_owned(), audit));
    }
    let dashed = format!("{}-{}-{}", &date[0..4], &date[4..6], &date[6..8]);
    // Lightweight file: verdict + per-recording counts only, never the full
    // findings Vec (legacy days balloon to GB otherwise — 2026-08-04).
    let promotable = !entries.is_empty() && entries.iter().all(|(_, _, a)| a.is_clean());
    let file = ScorecardFile {
        date: dashed,
        promotable,
        recordings: entries
            .iter()
            .map(|(venue, symbol, audit)| light_entry(venue, symbol, audit))
            .collect(),
    };
    serde_json::to_string_pretty(&file).map_err(|error| error.to_string())
}

/// OPS-13 drift/decay watch: load the RES-4 band-accuracy trend journal and
/// report whether the `liq.est_bands` validation quality has decayed over the
/// trailing weeks (coverage halved / MRE doubled vs the 12-week baseline).
///
/// Prints a JSON verdict — `{"decayed": bool, "alert": {...}|null}` with the
/// alert id/severity/detail/runbook when it fires (`alert` is `null` when the
/// trend is healthy) — and exits 0 either way. A corrupt journal is a failed
/// command (exit 2, CONV-8): the check never fabricates a verdict; a missing
/// journal is a "no data" month (RES-5, `decayed: false`). `--dedupe-ns` sets
/// the alert's dedupe window for a future stateful consumer (e.g. a Telegram
/// bot transport that routes through `AlertRouter`); a one-shot process has no
/// dedupe state of its own.
///
/// `--runs-dir` journals the weekly run's verdict to `<runs-dir>/index.jsonl`
/// (the shared RES-4/SIM-10 tracker) as a `band_accuracy_decay` record line,
/// correlated to the study's `whale_study` run record by the `run_id`/`week`
/// of the latest trend line (the week the study just graded) — the tracker
/// records the study AND its drift verdict per week. A clean verdict is
/// journaled too (the evidence the check ran); an empty trend has no run to
/// attach and writes nothing (RES-5). Fail-closed: an unwritable tracker is a
/// real failure (exit 2), never a silent gap in the record.
///
/// The weekly wrapper (`run_whale_study_weekly.sh`) runs this after each
/// study with `--runs-dir $RUNS_DIR --telegram`; the Telegram send remains a
/// deployment artifact.
fn cmd_band_accuracy_decay(args: &[String]) -> Result<String, String> {
    let trend = flag(args, "--trend")
        .unwrap_or_else(|| "research/band_accuracy/band_accuracy.jsonl".to_string());
    let dedupe_ns = match flag(args, "--dedupe-ns") {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| "--dedupe-ns must be an integer")?,
        None => 7 * 24 * 3600 * 1_000_000_000i64, // 7 days: weekly cadence
    };
    if dedupe_ns <= 0 {
        return Err("--dedupe-ns must be positive".into());
    }
    let rows = load_band_accuracy_trend(Path::new(&trend))
        .map_err(|e| format!("band-accuracy-decay: {e}"))?;
    let alert = band_accuracy_decay_alert(&rows, dedupe_ns);
    let mut verdict = serde_json::json!({
        "decayed": alert.is_some(),
        "alert": alert.as_ref().map(|a| serde_json::json!({
            "id": a.id,
            "severity": a.severity.as_str(),
            "detail": a.detail,
            "runbook": a.runbook,
            "dedupe_key": a.dedupe_key,
        })),
    });

    // Telegram edge (OPS-13/OPS-9): route a fired alert through the
    // framework's dedupe + quiet-hours batching, then deliver. Without a
    // fired alert there is nothing to send; without credentials the edge is
    // functionality-gated — logged as "unconfigured", exit 0 (never a silent
    // failure and never a fake send, same posture as the P1 webhook). An
    // edge failure (bad quiet-hours config, failed send/batch) is captured
    // and propagated AFTER the verdict is journaled below: the week's
    // verdict is evidence and must not be blanked by a delivery outage — the
    // record says "send_failed" honestly and the command still exits 2.
    let mut telegram_err: Option<String> = None;
    if args.iter().any(|a| a == "--telegram") {
        if let Some(alert) = alert {
            if let (Some(token), Some(chat_id)) = (
                std::env::var("TELEGRAM_BOT_TOKEN").ok(),
                std::env::var("TELEGRAM_CHAT_ID").ok(),
            ) {
                let cfg = TelegramConfig {
                    url: std::env::var("MP_OPS_TELEGRAM_URL")
                        .unwrap_or_else(|_| "https://api.telegram.org".to_string()),
                    token,
                    chat_id,
                };
                let now = now_ns();
                match quiet_hours_from_env() {
                    Ok(qh) => {
                        let mut router = AlertRouter::new(Some(qh));
                        match router.route(&alert, now) {
                            RouteOutcome::Sent(d) => match post_telegram(&d, &cfg) {
                                Ok(()) => verdict["telegram"] = serde_json::json!("sent"),
                                Err(e) => {
                                    telegram_err = Some(format!("telegram send failed: {e}"));
                                    verdict["telegram"] = serde_json::json!("send_failed");
                                }
                            },
                            RouteOutcome::Batched => {
                                let dir = PathBuf::from(
                                    std::env::var("MP_OPS_TELEGRAM_DIR")
                                        .unwrap_or_else(|_| "journal/telegram".to_string()),
                                );
                                match append_batch(&dir, &Dispatch::from_alert(&alert, now)) {
                                    Ok(()) => verdict["telegram"] = serde_json::json!("batched"),
                                    Err(e) => {
                                        telegram_err = Some(format!("telegram batch failed: {e}"));
                                        verdict["telegram"] = serde_json::json!("send_failed");
                                    }
                                }
                            }
                            RouteOutcome::Deduped => {
                                verdict["telegram"] = serde_json::json!("deduped");
                            }
                        }
                    }
                    Err(e) => {
                        telegram_err = Some(e);
                        verdict["telegram"] = serde_json::json!("send_failed");
                    }
                }
            } else {
                verdict["telegram"] = serde_json::json!("unconfigured");
            }
        } else {
            verdict["telegram"] = serde_json::json!("none");
        }
    }

    // RES-4 tracker (SIM-10 contract): journal the weekly verdict as its own
    // line in runs/index.jsonl, correlated to the study's run record by the
    // run_id + week of the latest trend line. The record mirrors the printed
    // verdict (decayed/alert/telegram) plus the correlation fields. An
    // unwritable tracker is a hard failure (exit 2) — evidence is never
    // silently dropped — but it does not suppress a telegram edge error.
    if let Some(runs_dir) = flag(args, "--runs-dir") {
        if let Some(last) = rows.last() {
            let mut record = verdict.clone();
            record["study"] = serde_json::json!("band_accuracy_decay");
            record["week"] = serde_json::json!(&last.week);
            record["run_id"] = match &last.run_id {
                Some(id) => serde_json::json!(id),
                None => serde_json::Value::Null,
            };
            append_run_record(Path::new(&runs_dir), &record)
                .map_err(|e| format!("band-accuracy-decay: {e}"))?;
        }
    }
    if let Some(e) = telegram_err {
        return Err(e);
    }
    serde_json::to_string(&verdict).map_err(|e| e.to_string())
}

/// Drain the P3 quiet-hours batch ledger (`journal/telegram/batch.jsonl`) to
/// Telegram. Fail-closed (CONV-8): any send failure or corrupt line exits
/// non-zero with the batch intact — alerts are never silently dropped. Every
/// successful send is recorded in the delivery log
/// (`journal/telegram/delivered.jsonl`) — the flushed records the monthly
/// report's delivery log renders (OPS-6): what WAS delivered, as opposed to
/// what is still pending.
///
/// `--wait` first holds until quiet hours end (OPS-9), then drains — the
/// weekly wrapper's wait, moved inside the binary. It sleeps only while quiet
/// hours are actually active: a flush that starts outside the window (or
/// after it, e.g. a wrapper that began late) drains immediately instead of
/// sleeping ~24h for a wrap-around window.
fn cmd_telegram_flush(args: &[String]) -> Result<String, String> {
    let (Some(token), Some(chat_id)) = (
        std::env::var("TELEGRAM_BOT_TOKEN").ok(),
        std::env::var("TELEGRAM_CHAT_ID").ok(),
    ) else {
        return Err("telegram-flush: TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID must be set".into());
    };
    let cfg = TelegramConfig {
        url: std::env::var("MP_OPS_TELEGRAM_URL")
            .unwrap_or_else(|_| "https://api.telegram.org".to_string()),
        token,
        chat_id,
    };
    let dir = match flag(args, "--dir") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(
            std::env::var("MP_OPS_TELEGRAM_DIR").unwrap_or_else(|_| "journal/telegram".to_string()),
        ),
    };
    if args.iter().any(|a| a == "--wait") {
        let qh = quiet_hours_from_env()?;
        let now = now_ns();
        if qh.contains(now) {
            let secs = u64::from(qh.minutes_until_end(now)) * 60;
            println!("telegram-flush: in quiet hours — flushing in {secs}s");
            sleep_secs(secs);
        }
    }
    // now_ns() stamps the delivered records — the delivery log is evidence
    // (PD-3: the CLI is an ops edge; the timestamp is the injected clock).
    let flushed = flush_batch(&dir, &cfg, now_ns())?;
    serde_json::to_string(&serde_json::json!({ "flushed": flushed })).map_err(|e| e.to_string())
}

/// OPS-14 near-real-time staleness watch over the quiet-hours Telegram batch
/// ledger: raises `telegram-stale` (P2) when a dispatch has been queued
/// longer than one full quiet window (default 24h) — a missed
/// `telegram-flush` must alert in near-real-time (the hourly
/// `telegram-stale.timer`), not just at month-end in the report.
///
/// Prints a JSON verdict — `{"stale": bool, "pending": n, "alert": {...}|null}`
/// (`pending` = dispatches currently in the batch; the stale count lives in
/// the alert detail) — and exits 0 either way. A corrupt batch is a failed
/// command (exit 2,
/// CONV-8): the check never fabricates a verdict; a missing batch is the
/// healthy "nothing queued" state (`stale: false`). `--threshold-hours` sets
/// the quiet-window size (default 24); `--dedupe-ns` sets the alert's dedupe
/// window for a future stateful consumer (default = threshold). With
/// `--telegram`, a fired P2 is routed through the Bot API edge and sent
/// immediately — P2 always breaks through quiet hours (OPS-9), so the alert
/// escapes the very batch that is stuck and is never re-queued into it.
fn cmd_telegram_stale(args: &[String]) -> Result<String, String> {
    let dir = match flag(args, "--dir") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(
            std::env::var("MP_OPS_TELEGRAM_DIR").unwrap_or_else(|_| "journal/telegram".to_string()),
        ),
    };
    let threshold_ns = match flag(args, "--threshold-hours") {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| "--threshold-hours must be an integer")?
            .checked_mul(3_600_000_000_000)
            .ok_or("--threshold-hours out of range")?,
        None => 24 * 3_600_000_000_000i64,
    };
    if threshold_ns <= 0 {
        return Err("--threshold-hours must be positive".into());
    }
    let dedupe_ns = match flag(args, "--dedupe-ns") {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| "--dedupe-ns must be an integer")?,
        None => threshold_ns,
    };
    if dedupe_ns <= 0 {
        return Err("--dedupe-ns must be positive".into());
    }
    let now = now_ns();
    let rows = load_telegram_batch(&dir, now).map_err(|e| format!("telegram-stale: {e}"))?;
    let alert = stale_batch_alert(&rows, now, threshold_ns, dedupe_ns);
    let mut verdict = serde_json::json!({
        "stale": alert.is_some(),
        "pending": rows.len(),
        "alert": alert.as_ref().map(|a| serde_json::json!({
            "id": a.id,
            "severity": a.severity.as_str(),
            "detail": a.detail,
            "runbook": a.runbook,
            "dedupe_key": a.dedupe_key,
        })),
    });

    if args.iter().any(|a| a == "--telegram") {
        if let Some(alert) = alert {
            if let (Some(token), Some(chat_id)) = (
                std::env::var("TELEGRAM_BOT_TOKEN").ok(),
                std::env::var("TELEGRAM_CHAT_ID").ok(),
            ) {
                let cfg = TelegramConfig {
                    url: std::env::var("MP_OPS_TELEGRAM_URL")
                        .unwrap_or_else(|_| "https://api.telegram.org".to_string()),
                    token,
                    chat_id,
                };
                // P2 always breaks through quiet hours (OPS-9); a one-shot
                // router has no dedupe history, so the fired alert is Sent.
                let mut router = AlertRouter::new(None);
                match router.route(&alert, now) {
                    RouteOutcome::Sent(d) => match post_telegram(&d, &cfg) {
                        Ok(()) => verdict["telegram"] = serde_json::json!("sent"),
                        Err(e) => return Err(format!("telegram send failed: {e}")),
                    },
                    RouteOutcome::Batched | RouteOutcome::Deduped => {
                        unreachable!("a fresh no-quiet-hours router never batches or dedupes a P2")
                    }
                }
            } else {
                verdict["telegram"] = serde_json::json!("unconfigured");
            }
        } else {
            verdict["telegram"] = serde_json::json!("none");
        }
    }
    serde_json::to_string(&verdict).map_err(|e| e.to_string())
}
/// P1 egress edge (owner decision 2026-08-06, audit 08-04 #3): the shipped
/// call site for the P1 "phone-call webhook" channel. WIRED but dead until
/// credentials exist — with `MP_OPS_P1_WEBHOOK` set, a P1 dispatch (built
/// from `--id`/`--detail`, the same `Dispatch::from_alert` shape the rest of
/// the framework uses) is POSTed there via curl; unset, the command fails
/// loudly (exit 2) with the "dead until creds" reason — never a silent drop,
/// never a fake send. https is accepted (TLS is the host's curl, audit 08-04
/// #9); `--ts-ns` injects the dispatch timestamp (default now, PD-3 edge
/// clock) for deterministic tests.
fn cmd_p1_webhook(args: &[String]) -> Result<String, String> {
    let id = need(args, "--id")?;
    let detail = need(args, "--detail")?;
    if id.is_empty() || detail.is_empty() {
        return Err("p1-webhook: --id and --detail must be non-empty".into());
    }
    let url = std::env::var("MP_OPS_P1_WEBHOOK").map_err(|_| {
        "p1-webhook: MP_OPS_P1_WEBHOOK unset — P1 egress is dead until credentials are \
         provisioned (owner decision 2026-08-06); set the URL to activate the channel"
            .to_string()
    })?;
    let ts_ns = match flag(args, "--ts-ns") {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| "--ts-ns must be an integer")?,
        None => now_ns(),
    };
    let dispatch = Dispatch::from_alert(&Alert::new(id, Severity::P1, 0, detail), ts_ns);
    AlertRouter::post_p1_webhook(&dispatch, &url).map_err(|e| format!("p1-webhook: {e}"))?;
    // The URL is NOT echoed in the verdict: a webhook URL can embed a sink
    // key/token (healthchecks-style), and the Telegram edge already keeps
    // credentials out of output (PD-2).
    serde_json::to_string(&serde_json::json!({
        "egress": "sent",
        "id": dispatch.id,
        "severity": dispatch.severity.as_str(),
        "channel": "telegram_phone",
        "detail": dispatch.detail,
        "runbook": dispatch.runbook,
        "ts_ns": dispatch.ts_ns,
    }))
    .map_err(|e| e.to_string())
}

/// Sleep `secs` — the quiet-hours wait (`telegram-flush --wait`).
/// `MP_OPS_SLEEP` overrides the sleeper (a command receiving the seconds),
/// the same seam the weekly wrapper used, so e2e tests never block on a real
/// multi-hour sleep. The override is best-effort — an unspawnable override
/// falls through to flushing rather than failing the drain.
fn sleep_secs(secs: u64) {
    if secs == 0 {
        return;
    }
    match std::env::var("MP_OPS_SLEEP") {
        Ok(cmd) => {
            let _ = std::process::Command::new(cmd)
                .arg(secs.to_string())
                .status();
        }
        Err(_) => std::thread::sleep(std::time::Duration::from_secs(secs)),
    }
}

/// Wall-clock now — the CLI is an ops edge (alerting/telemetry), the same
/// sanctioned read opsd and the command journal use (PD-3: edges, not
/// decision paths).
fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

/// Quiet-hours window from env (MP_OPS_QUIET_START_MIN / MP_OPS_QUIET_END_MIN,
/// minutes-of-day), defaulting to the spec 009 policy window 22:00–07:00 UTC.
fn quiet_hours_from_env() -> Result<QuietHours, String> {
    let start = match std::env::var("MP_OPS_QUIET_START_MIN") {
        Ok(s) => s
            .parse::<u32>()
            .map_err(|_| "MP_OPS_QUIET_START_MIN must be an integer (minutes of day)")?,
        Err(_) => 22 * 60,
    };
    let end = match std::env::var("MP_OPS_QUIET_END_MIN") {
        Ok(s) => s
            .parse::<u32>()
            .map_err(|_| "MP_OPS_QUIET_END_MIN must be an integer (minutes of day)")?,
        Err(_) => 7 * 60,
    };
    if start >= 1440 || end > 1440 {
        return Err("quiet-hours minutes must be in 0..1440".into());
    }
    Ok(QuietHours {
        start_min: start,
        end_min: end,
    })
}

fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: mp-ops <subcommand> [options]");
        eprintln!(
            "Subcommands: compact, audit, scorecard, promote, band-accuracy-decay, telegram-flush, telegram-stale, p1-webhook"
        );
        return ExitCode::FAILURE;
    }

    let result = match args[1].as_str() {
        "compact" => cmd_compact(&args[2..]),
        "audit" => cmd_audit(&args[2..]),
        "scorecard" => cmd_scorecard(&args[2..]),
        "promote" => cmd_promote(&args[2..]),
        "band-accuracy-decay" => cmd_band_accuracy_decay(&args[2..]),
        "telegram-flush" => cmd_telegram_flush(&args[2..]),
        "telegram-stale" => cmd_telegram_stale(&args[2..]),
        "p1-webhook" => cmd_p1_webhook(&args[2..]),
        other => Err(format!("unknown subcommand: {other}")),
    };

    match result {
        Ok(msg) => {
            tracing::info!("{msg}");
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("Error: {e}");
            // Job subcommands exit 2 on failure — the same convention as the
            // Python research siblings (`run_band_accuracy.py`: "a failed job
            // (exit 2)"): a failed check is a distinct, grep-able status, not
            // a generic usage error. The weekly wrapper treats non-zero as
            // "check failed, see journald" and never fabricates a verdict.
            if matches!(
                args[1].as_str(),
                "band-accuracy-decay" | "telegram-flush" | "telegram-stale" | "p1-webhook"
            ) {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::log::EventLogWriter;
    use mp_core::{
        EventEnvelope, EventProvenance, InstrumentKind, MarketEvent, Side, SnapshotSource,
        SymbolId, SymbolMeta,
    };

    /// promote: a BOM-prefixed scorecard parses and a single DIRTY day yields
    /// `promoted=false` with the correct first_failure.
    #[test]
    fn promote_reads_bom_scorecards_and_reports_not_yet() {
        let dir = std::env::temp_dir().join(format!("mp-promote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let card = serde_json::json!({
            "date": "2026-08-03",
            "recordings": [
                {"venue": "binance", "symbol": "BTCUSDT", "clean": false,
                 "event_count": 1, "coverage": 1.0, "findings": 1, "blocking_findings": 1}
            ],
            "promotable": false
        });
        // PowerShell Set-Content -Encoding UTF8 writes a BOM prefix.
        let mut text = "\u{feff}".to_string();
        text.push_str(&serde_json::to_string(&card).unwrap());
        std::fs::write(dir.join("2026-08-03.json"), text).unwrap();
        let verdict = cmd_promote(&["--scorecards-dir".into(), dir.to_string_lossy().into()]);
        let _ = std::fs::remove_dir_all(&dir);
        let ok = verdict.expect("promote should succeed on a parseable scorecard");
        assert!(ok.contains("NOT YET"), "dirty day must not promote: {ok}");
    }

    /// promote: seven consecutive promotable scorecards pass the gate.
    #[test]
    fn promote_passes_after_seven_clean_days() {
        let dir = std::env::temp_dir().join(format!("mp-promote-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for day in 1..=7 {
            let card = serde_json::json!({
                "date": format!("2026-08-{day:02}"),
                "recordings": [
                    {"venue": "binance", "symbol": "BTCUSDT", "clean": true,
                     "event_count": 1, "coverage": 1.0, "findings": 0, "blocking_findings": 0}
                ],
                "promotable": true
            });
            std::fs::write(
                dir.join(format!("2026-08-{day:02}.json")),
                serde_json::to_string(&card).unwrap(),
            )
            .unwrap();
        }
        let verdict = cmd_promote(&["--scorecards-dir".into(), dir.to_string_lossy().into()]);
        let _ = std::fs::remove_dir_all(&dir);
        let ok = verdict.expect("promote should succeed");
        assert!(
            ok.contains("PROMOTED"),
            "seven clean days must promote: {ok}"
        );
    }

    #[test]
    fn int_4_compaction_refuses_quarantined_log() {
        let path = std::env::temp_dir().join(format!("mp-int-compact-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (mut writer, _) = EventLogWriter::open(&path).unwrap();
        writer
            .write_symbols(&[SymbolMeta::new(
                SymbolId(0),
                Venue::BinanceFutures,
                "BTCUSDT",
                "BTC",
                "USDT",
                InstrumentKind::Perp,
                0.1,
                0.1,
                1.0,
            )])
            .unwrap();
        let event = EventEnvelope::new(
            Venue::Hyperliquid,
            SymbolId(0),
            1,
            1,
            1,
            MarketEvent::Trade {
                price: 1.0,
                qty: 1.0,
                side: Side::Buy,
                trade_id: 1,
            },
        )
        .with_provenance(EventProvenance {
            stream: "trade".into(),
            subscription: "x".into(),
            connection_id: 1,
            snapshot_source: SnapshotSource::None,
        });
        writer.append(&event).unwrap();
        writer.sync().unwrap();
        let err = clean_audit(
            &path,
            &AuditConfig::single(Venue::BinanceFutures, "BTCUSDT"),
        )
        .unwrap_err();
        assert!(err.contains("quarantined raw log"));
        let _ = std::fs::remove_file(path);
    }

    /// One `band_accuracy.jsonl` trend line (the exact shape the research job
    /// appends; provenance echoes omitted are allowed by the parser).
    fn trend_line(week: u32, coverage: f64, mre: f64) -> String {
        format!(
            "{{\"week\":\"2026-W{week:02}\",\"n\":100,\"mean_relative_error\":{mre},\"coverage\":{coverage}}}"
        )
    }

    /// The same line carrying the job's `run_id` echo — the run record of the
    /// `whale_study` run that graded that week (the join key to the SIM-10
    /// record in runs/index.jsonl).
    fn trend_line_with_run_id(week: u32, coverage: f64, mre: f64, run_id: &str) -> String {
        format!(
            "{{\"week\":\"2026-W{week:02}\",\"run_id\":\"{run_id}\",\"n\":100,\"mean_relative_error\":{mre},\"coverage\":{coverage}}}"
        )
    }

    #[test]
    fn ops_13_mp_ops_decay_subcommand_reports_verdict() {
        let dir = std::env::temp_dir().join(format!("mpops13-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let trend = dir.join("band_accuracy.jsonl");

        // 8 healthy weeks (94% coverage, 2% MRE) + 4 collapsed weeks (25%
        // coverage, 12% MRE): trailing 4-wk mean 0.25 < half of 12-wk mean
        // 0.71, MRE 0.12 > double 0.053 — decayed.
        let mut lines = (1..=8)
            .map(|w| trend_line(w, 0.94, 0.02))
            .collect::<Vec<_>>();
        lines.extend((9..=12).map(|w| trend_line(w, 0.25, 0.12)));
        std::fs::write(&trend, lines.join("\n") + "\n").unwrap();
        let out = cmd_band_accuracy_decay(&["--trend".into(), trend.to_string_lossy().into()])
            .expect("decayed verdict");
        assert!(out.contains("\"decayed\":true"), "{out}");
        assert!(out.contains("\"id\":\"band-accuracy-decay\""), "{out}");
        assert!(out.contains("\"severity\":\"P3\""), "{out}");

        // Steady healthy trend ⇒ no alert.
        let healthy = (1..=12)
            .map(|w| trend_line(w, 0.94, 0.02))
            .collect::<Vec<_>>();
        std::fs::write(&trend, healthy.join("\n") + "\n").unwrap();
        let out = cmd_band_accuracy_decay(&["--trend".into(), trend.to_string_lossy().into()])
            .expect("healthy verdict");
        assert!(out.contains("\"decayed\":false"), "{out}");

        // Missing journal (study not run yet) ⇒ no-data month, not an error.
        let out = cmd_band_accuracy_decay(&[
            "--trend".into(),
            dir.join("nope.jsonl").to_string_lossy().into(),
        ])
        .expect("missing journal verdict");
        assert!(out.contains("\"decayed\":false"), "{out}");

        // Corrupt journal ⇒ fail closed (CONV-8), never a fabricated verdict.
        std::fs::write(&trend, "not json\n").unwrap();
        let err = cmd_band_accuracy_decay(&["--trend".into(), trend.to_string_lossy().into()])
            .expect_err("corrupt journal must fail");
        assert!(err.contains("line 1"), "error names the line: {err}");

        // Bad --dedupe-ns values are rejected, not silently absorbed.
        let bad = cmd_band_accuracy_decay(&[
            "--trend".into(),
            trend.to_string_lossy().into(),
            "--dedupe-ns".into(),
            "abc".into(),
        ])
        .expect_err("non-integer dedupe must fail");
        assert!(bad.contains("must be an integer"), "{bad}");
        let zero = cmd_band_accuracy_decay(&[
            "--trend".into(),
            trend.to_string_lossy().into(),
            "--dedupe-ns".into(),
            "0".into(),
        ])
        .expect_err("non-positive dedupe must fail");
        assert!(zero.contains("must be positive"), "{zero}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ops_13_mp_ops_decay_journals_verdict_to_runs_index() {
        let dir = std::env::temp_dir().join(format!("mpops13r-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let trend = dir.join("band_accuracy.jsonl");
        let runs_dir = dir.join("runs");
        let runs = runs_dir.to_string_lossy().into_owned();

        // Decayed trend (8 healthy + 4 collapsed weeks) with run_id echoes:
        // the latest graded week is 2026-W12, run 01JBA...0001.
        let mut lines = (1..=8)
            .map(|w| trend_line_with_run_id(w, 0.94, 0.02, "01JBA0TEST0000000000000001"))
            .collect::<Vec<_>>();
        lines.extend(
            (9..=12).map(|w| trend_line_with_run_id(w, 0.25, 0.12, "01JBA0TEST0000000000000001")),
        );
        std::fs::write(&trend, lines.join("\n") + "\n").unwrap();

        let out = cmd_band_accuracy_decay(&[
            "--trend".into(),
            trend.to_string_lossy().into(),
            "--runs-dir".into(),
            runs.clone(),
        ])
        .expect("decayed verdict with journaling");
        assert!(out.contains("\"decayed\":true"), "{out}");

        // The verdict line landed in runs/index.jsonl, correlated by run_id +
        // week to the weekly run's whale_study record (RES-4 tracker).
        let idx = std::fs::read_to_string(runs_dir.join("index.jsonl")).unwrap();
        let rec: serde_json::Value =
            serde_json::from_str(idx.lines().next().expect("one verdict line")).unwrap();
        assert_eq!(rec["study"], "band_accuracy_decay");
        assert_eq!(rec["week"], "2026-W12");
        assert_eq!(rec["run_id"], "01JBA0TEST0000000000000001");
        assert_eq!(rec["decayed"], true);
        assert_eq!(rec["alert"]["id"], "band-accuracy-decay");

        // A healthy trend still journals its verdict (decayed=false, alert
        // null) — the clean verdict is the evidence the check ran.
        let healthy = (1..=12)
            .map(|w| trend_line_with_run_id(w, 0.94, 0.02, "01JBA0TEST0000000000000002"))
            .collect::<Vec<_>>();
        std::fs::write(&trend, healthy.join("\n") + "\n").unwrap();
        cmd_band_accuracy_decay(&[
            "--trend".into(),
            trend.to_string_lossy().into(),
            "--runs-dir".into(),
            runs.clone(),
        ])
        .expect("healthy verdict with journaling");
        let idx = std::fs::read_to_string(runs_dir.join("index.jsonl")).unwrap();
        let rec: serde_json::Value =
            serde_json::from_str(idx.lines().nth(1).expect("second verdict line")).unwrap();
        assert_eq!(rec["decayed"], false);
        assert_eq!(rec["alert"], serde_json::Value::Null);
        assert_eq!(rec["run_id"], "01JBA0TEST0000000000000002");

        // Missing journal (no graded week yet ⇒ no run to attach, RES-5):
        // the verdict is printed but NOT journaled.
        let before = std::fs::read_to_string(runs_dir.join("index.jsonl")).unwrap();
        cmd_band_accuracy_decay(&[
            "--trend".into(),
            dir.join("nope.jsonl").to_string_lossy().into(),
            "--runs-dir".into(),
            runs.clone(),
        ])
        .expect("no-data verdict");
        let after = std::fs::read_to_string(runs_dir.join("index.jsonl")).unwrap();
        assert_eq!(before, after, "no run to correlate ⇒ no verdict line");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
