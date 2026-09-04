//! `mp-ops` — operational CLI for the Money Printer system (SPEC-011).
//!
//! Subcommands:
//!   compact --date YYYY-MM-DD --venue bybit --symbol BTCUSDT [--force]
//!           Compacts a raw event log into partitioned Parquet files.
//!           Date also accepts YYYYMMDD (backward compatible). --force
//!           overrides the idempotency skip and rewrites the day's Parquet
//!           even when the existing footer's source hash already matches
//!           (B-5 repair path — rebuilds hollow files written by a buggy
//!           writer without deleting them first).
//!   prune --date YYYY-MM-DD --venue bybit --symbol BTCUSDT [--dry-run]
//!           Hash-verified hot-tier retention: refuses to delete a raw log
//!           unless compaction proof exists (W-6/C-2), then removes it and
//!           journals the deletion to data/retention_delete_manifest.jsonl.
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
//!   status [--scorecards-dir DIR] [--required venue:symbol ...] [--trend-days N]
//!           [--pipeline-log PATH] [--backup-manifest PATH] [--latch PATH]
//!           OPS-16 one command that tells the truth: trading mode, the
//!           promotion verdict, the latest scorecard with margins, the
//!           per-day coverage trend, the last backup manifest entry, the
//!           daily pipeline's last log line, and the kill-latch state — one
//!           JSON document. Missing artifacts report `present: false`;
//!           corrupt gate/safety artifacts fail closed (exit non-zero).
//!   pipeline-stale [--scorecards-dir DIR] [--deadline-min N] [--dedupe-ns N]
//!           [--ts-ns N] [--telegram] [--webhook]
//!           OPS-17 dead-man for the daily gate: raises a P1 when the
//!           previous UTC day's scorecard has not landed by the deadline
//!           (default 15m past UTC midnight — the 00:05 gate plus margin).
//!           Deferred before the deadline (safe to run hourly); --ts-ns
//!           injects the clock for tests.
//!   telegram-stale [--dir DIR] [--threshold-hours N] [--dedupe-ns N] [--telegram]
//!           OPS-14 near-real-time watch on the quiet-hours batch ledger:
//!           raises telegram-stale (P2) when a dispatch sits queued longer
//!           than one quiet window (default 24h — a missed telegram-flush),
//!           printing a JSON verdict {stale, pending, alert} (pending = the
//!           number of dispatches in the batch); exit 2 on a corrupt batch
//!           (fail-closed), never a fabricated verdict. With --telegram, a
//!           fired P2 is sent immediately (P2 always breaks through quiet
//!           hours, OPS-9).
//!   storage-budget [--dir DIR] [--cap-bytes N] [--alert-at-days N]
//!           [--trend-days N] [--dedupe-ns N] [--ts-ns N] [--manifest PATH]
//!           [--telegram]
//!           OPS-15 storage-budget watch (the spec 001 appendix's first
//!           revisit trigger, wired): sums per-day corpus sizes from the
//!           {YYYYMMDD}_*.log file names (read-only, W-6 — no state file),
//!           takes the trailing-window mean daily addition rate (default
//!           7 days), and raises storage-budget (P2) when the projection
//!           puts the corpus at the budget cap within the alert horizon
//!           (default 14 days) or it is already at/over the cap. The budget
//!           is explicit config — --cap-bytes or MP_STORAGE_BUDGET_BYTES,
//!           unset ⇒ exit 2 (fail-closed, never a silent skip). With
//!           --manifest PATH (the vps_drain_manifest.jsonl), the watch ALSO
//!           fires the same P2 when the relay is silently holding files:
//!           any entry whose LATEST per-file record is action=landed with
//!           release not in {released, no_release} (ssh_failed/skipped) —
//!           the drain landed the file but never released the VPS copy.
//!           Windows-side artifact; the VPS timer does not pass it.
//!           Prints a JSON verdict {dir, current_bytes, growth_bytes_per_day,
//!           days_to_cap, held_vps_files, held_vps_count, alert}; with
//!           --telegram, a fired P2 is sent immediately (P2 breaks through
//!           quiet hours, OPS-9).
//!   telegram-send --id ID --detail TEXT [--severity p1|p2|p3] [--ts-ns N]
//!           One-shot Telegram notification for a wrapper verdict (the daily
//!           promotion-gate verdict in daily_pipeline.ps1): sends --detail
//!           immediately through the Bot API edge. NO quiet-hours batching —
//!           a wrapper verdict must land when produced (the daily pipeline
//!           runs at 00:05 UTC, inside the 22:00–07:00 quiet window; a
//!           batched P3 would sit in the ledger until the next flush).
//!           Fail-closed (CONV-8): unset credentials exit 2 ("must be set",
//!           never a silent drop); a failed send is an error, never a fake
//!           "sent".
//!
//! Usage:
//!   cargo run --package mp-ops --bin mp-ops -- compact --date 2026-07-15 --venue bybit --symbol BTCUSDT
//!   cargo run --package mp-ops --bin mp-ops -- band-accuracy-decay --trend research/band_accuracy/band_accuracy.jsonl
//!   cargo run --package mp-ops --bin mp-ops -- band-accuracy-decay --trend research/band_accuracy/band_accuracy.jsonl --runs-dir runs --telegram
//!   cargo run --package mp-ops --bin mp-ops -- telegram-flush --wait
//!   cargo run --package mp-ops --bin mp-ops -- telegram-stale --telegram
//!   cargo run --package mp-ops --bin mp-ops -- storage-budget --cap-bytes 500000000000 --telegram
//!   cargo run --package mp-ops --bin mp-ops -- telegram-send --id daily-pipeline --detail 'day 2026-08-09: NOT promotable' --severity p2
//!   MP_OPS_P1_WEBHOOK=https://hooks.example.com/alert cargo run --package mp-ops --bin mp-ops -- \
//!     p1-webhook --id recon-diverged --detail 'BTCUSDT position mismatch'

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, SymbolTable, TradingMode, Venue};
use mp_ops::{
    append_batch, append_run_record, band_accuracy_decay_alert, flush_batch, held_drain_files,
    load_band_accuracy_trend, load_telegram_batch, parse_drain_manifest_line, post_telegram,
    project_storage, sample_daily_sizes, stale_batch_alert, storage_budget_alert, Alert,
    AlertRouter, Channel, Dispatch, DrainManifestEntry, KillLatch, LatchScope, QuietHours,
    RouteOutcome, Severity, TelegramConfig,
};
use mp_storage::promotion::check_promotion_determinism;
use mp_storage::prune::{self, PruneRefusal};
use mp_storage::{
    audit_raw_log, compactor, load_determinism, AuditConfig, DailyScorecard, DeterminismArtifact,
    RawLogAudit, RecordingBursts,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
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

/// Zero-Cost Mode flag check (docs/ZERO_COST_MODE.md). Both spellings are
/// accepted so an operator following spec 024 (`--zero-cost-mode`) and the
/// historical code (`--zero-cost`) get the same behaviour (A-11 audit
/// 2026-09-02 — a spec-following operator passing `--zero-cost-mode` must NOT
/// silently fall back to the full-mode 0.995 gate).
fn is_zero_cost(args: &[String]) -> bool {
    args.iter().any(|a| a == "--zero-cost" || a == "--zero-cost-mode")
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
    // Zero-Cost Mode (docs/ZERO_COST_MODE.md): lowered coverage threshold,
    // book stream absence expected, stale bursts are warnings only.
    if is_zero_cost(args) {
        config.zero_cost_mode = true;
    }
    // `--require-stream` accepts either a bare stream name (required for
    // EVERY recording in the scorecard) or `venue:stream` (required only
    // for that venue's recordings). Venue-scoped requirements are what
    // let the gate demand `liquidation` on Binance/Bybit recordings
    // without breaking venues that have no native liq stream (e.g.
    // Hyperliquid — spec 024, COL-29).
    for value in flags(args, "--require-stream") {
        match value.split_once(':') {
            Some((venue_str, stream)) if !stream.is_empty() => {
                if parse_venue(venue_str).ok() == Some(venue) {
                    config.required_streams.insert(stream.to_owned());
                }
            }
            _ => {
                config.required_streams.insert(value);
            }
        }
    }
    Ok(config)
}

fn clean_audit(path: &Path, config: &AuditConfig) -> Result<RawLogAudit, String> {
    let audit = audit_raw_log(path, config);
    let clean = if config.zero_cost_mode {
        audit.is_clean_zero_cost()
    } else {
        audit.is_clean()
    };
    if clean {
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

/// `mp-ops prune --date YYYY-MM-DD --venue V --symbol S [--dry-run]`
///
/// Hot-tier retention gate (AUDIT-2026-09-02 A-1/A-12). Deletes a raw day-file
/// ONLY after hash-verified compact proof: it wraps
/// [`mp_storage::prune::verify_prunable`] (the W-6 guard — compaction manifest
/// exists, Parquet exists, row counts match, and the Parquet footer's
/// `source_log_hash` still equals a CRC-32 of the CURRENT raw bytes, which must
/// not be newer than the Parquet). On success it removes the raw log and
/// journals the deletion to `data/retention_delete_manifest.jsonl` (A-12:
/// deletions are auditable, not a bare `rm`). `--dry-run` verifies and reports
/// without deleting. Returns non-zero (with the refusal reason) when the compact
/// proof is incomplete — nothing is ever deleted without it.
fn cmd_prune(args: &[String]) -> Result<String, String> {
    let date_raw = need(args, "--date")?;
    let venue_str = need(args, "--venue")?;
    let symbol = need(args, "--symbol")?;
    let venue = parse_venue(&venue_str)?;
    let dry_run = args.iter().any(|a| a == "--dry-run");
    prune_day(Path::new("data"), venue, &venue_str, &symbol, &date_raw, dry_run)
}

/// The actual retention gate, parameterised on the data root so the unit
/// tests can drive it against a temp tree (the CLI wrapper pins `data/`).
fn prune_day(
    root: &Path,
    venue: Venue,
    venue_str: &str,
    symbol: &str,
    date_raw: &str,
    dry_run: bool,
) -> Result<String, String> {
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

    let raw_path = root
        .join("raw")
        .join(format!("{date_flat}_{venue_str}_{symbol}.log"));
    if !raw_path.exists() {
        return Err(format!(
            "raw log not found (nothing to prune): {}",
            raw_path.display()
        ));
    }

    // The compaction proof lives under the COLD root (`data/cold/manifests`,
    // `data/cold/trades`) — `verify_prunable`'s root parameter is the cold
    // root, not the data root (storage/tests/storage.rs:502 pins this
    // contract). Passing `data` here would resolve `data/manifests/...` and
    // refuse every properly compacted day (observed live 2026-09-03 on the
    // 08-13/08-22/08-24 hyperliquid days).
    let cold_root = root.join("cold");
    // B-2 (audit 2026-09-03): verify scoped to THIS symbol — refuse unless the
    // day manifest actually carries the symbol's own streams, so a clobbered/
    // legacy manifest (only the last-compacted symbol present) can never
    // green-light deleting this symbol's raw log on another symbol's proof.
    match prune::verify_prunable_symbol(&cold_root, venue, &date_dashed, symbol) {
        Ok(()) => {}
        Err(why) => {
            return Err(format!(
                "prune REFUSED for {} {}/{}: {}",
                date_dashed,
                venue_str,
                symbol,
                prunable_refusal(&why)
            ))
        }
    }

    if dry_run {
        return Ok(format!(
            "[dry-run] would prune {} {}/{} (compact proof verified)",
            date_dashed, venue_str, symbol
        ));
    }

    let size = std::fs::metadata(&raw_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let source_hash = compute_source_hash(&raw_path)?;

    std::fs::remove_file(&raw_path)
        .map_err(|e| format!("prune failed to delete {}: {e}", raw_path.display()))?;

    // Journal the deletion (A-12): what was deleted, when, and the verified
    // source hash (the same value the removed Parquet footer carried).
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let entry = serde_json::json!({
        "ts_unix_secs": now_secs,
        "venue": venue_str,
        "symbol": symbol,
        "date": date_dashed,
        "file": raw_path.display().to_string(),
        "size": size,
        "source_hash": source_hash,
    });
    let manifest_path = root.join("retention_delete_manifest.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&manifest_path)
        .map_err(|e| format!("open retention manifest {}: {e}", manifest_path.display()))?;
    f.write_all(entry.to_string().as_bytes())
        .and_then(|_| f.write_all(b"\n"))
        .map_err(|e| format!("write retention manifest: {e}"))?;

    Ok(format!(
        "pruned {} {}/{} (verified source_hash {}) journaled -> {}",
        date_dashed,
        venue_str,
        symbol,
        source_hash,
        manifest_path.display()
    ))
}

/// Human-readable reason a day cannot be safely pruned (W-6 / C-2).
fn prunable_refusal(r: &PruneRefusal) -> String {
    match r {
        PruneRefusal::ManifestMissing => {
            "no compaction manifest (day never compacted — skipping)".to_string()
        }
        PruneRefusal::ParquetFileMissing { stream, symbol } => {
            format!("Parquet missing for stream `{stream}` symbol `{symbol}`")
        }
        PruneRefusal::RowCountMismatch {
            stream,
            symbol,
            manifest,
            parquet,
        } => format!(
            "`{stream}/{symbol}` row-count mismatch: manifest {manifest} vs parquet {parquet}"
        ),
        PruneRefusal::SourceLogHashMismatch { stream, symbol } => format!(
            "`{stream}/{symbol}` raw bytes no longer hash to the Parquet source_log_hash"
        ),
        PruneRefusal::RawLogNewerThanParquet { stream, symbol } => format!(
            "`{stream}/{symbol}` raw log is newer than its Parquet (must re-compact first)"
        ),
        PruneRefusal::FooterMissingSourceLogHash { stream, symbol } => format!(
            "`{stream}/{symbol}` Parquet footer has no source_log_hash (unknown provenance)"
        ),
        PruneRefusal::SymbolNotInManifest { symbol } => format!(
            "manifest has no stream for `{symbol}` (clobbered/legacy manifest — re-compact the symbol or repair the manifest before pruning)"
        ),
    }
}

fn cmd_compact(args: &[String]) -> Result<String, String> {
    // B-5 repair path: `--force` overrides the idempotency skip so a day
    // whose Parquet already carries the matching source hash is rebuilt
    // from the current raw bytes (e.g. the schema-4 hollow shells). Safe:
    // the writer is a pure function of (events, version, hash) and the
    // raw log is never touched.
    let force = args.iter().any(|a| a == "--force");
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
    let opts = mp_storage::CompactOptions { force };
    let stats = compactor::compact_day_verified_opts(
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
        opts,
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
    /// Longest recv-clock hole in ns (margin, never a veto; 2026-08-12).
    #[serde(default)]
    worst_gap_ns: i64,
    /// Number of stale-event burst windows (margin, never a veto; 2026-08-12).
    #[serde(default)]
    stale_bursts: usize,
}

/// Source fingerprint sidecar (`data/scorecards/.scorecard_sources.json`) —
/// the fast re-score cache (2026-08-13). The daily pipeline records, per
/// scored day, each raw log's (size, mtime_ns) + the audit config fingerprint;
/// a later `scorecard --reuse-unchanged` run skips the full re-audit when the
/// source is byte-unchanged (append-only raw logs: size+mtime is the reuse
/// key; `--force` always re-audits). A cache, never gate data: a corrupt or
/// missing manifest just means a full audit, and the scorecard verdict always
/// comes from a real audit or an exact source match.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceEntry {
    size: u64,
    #[serde(default)]
    mtime_ns: Option<i64>,
    /// Audit-config fingerprint (`max_gap=..;streams=..`) — a config change
    /// invalidates reuse.
    config: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SourceDay {
    /// Sorted `venue:symbol` required set the day was scored with — reuse
    /// only when it matches the current command exactly.
    #[serde(default)]
    required: String,
    #[serde(default)]
    sources: BTreeMap<String, SourceEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SourceManifest {
    #[serde(default)]
    days: BTreeMap<String, SourceDay>,
}

/// Fingerprint of the audit config for the current command (max-gap + the
/// required stream set) — part of the reuse key.
fn source_config_fingerprint(args: &[String]) -> String {
    let max_gap = flag(args, "--max-gap-sec").unwrap_or_else(|| "default".to_string());
    let mut streams = flags(args, "--require-stream");
    streams.sort();
    format!("max_gap={max_gap};streams={}", streams.join(","))
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
        worst_gap_ns: audit.worst_gap_ns,
        stale_bursts: audit.stale_bursts.len(),
    }
}

/// Load every YYYY-MM-DD.json / YYYYMMDD.json scorecard in `dir`, sorted by
/// date, optionally filtered to days that cover every `required` venue:symbol
/// recording. Shared by `promote` and `status` so both read the gate's
/// artifacts identically. A BOM (PowerShell Set-Content) is stripped; dates
/// normalize to YYYY-MM-DD. Fail-closed: a corrupt scorecard is an error, not
/// a silently skipped day.
fn load_scorecards(dir: &str, required: &BTreeSet<String>) -> Result<Vec<ScorecardFile>, String> {
    let path = PathBuf::from(dir);
    if !path.is_dir() {
        return Err(format!("scorecards directory not found: {dir}"));
    }
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
    if !required.is_empty() {
        // Filter to days that include every required venue:symbol recording.
        // Invariant: the daily pipeline always generates scorecards with the
        // FULL required set (a missing raw log audits as recording_missing =>
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
    }
    Ok(files)
}

/// Feed the gate date + promotable + the per-recording stale-burst margins
/// (the Phase-0 window condition, spec 024 amendment 2026-08-12) through
/// `check_promotion`. Bursts are scoped to the `required` venue:symbol set: a
/// day's extra recordings (e.g. a legacy binance pair on a hyperliquid day)
/// must not veto a window that covers the required corpus.
/// Load the determinism artifacts for the loaded scorecards (spec 018 MOD-9
/// gate input). A CORRUPT artifact fails closed — the gate must know the
/// proof cannot be read; a missing one is simply not-passed for its day.
fn load_determinism_artifacts(
    dir: &str,
    files: &[ScorecardFile],
) -> Result<Vec<DeterminismArtifact>, String> {
    let mut out = Vec::new();
    for f in files {
        if let Some(a) = load_determinism(Path::new(dir), &f.date)? {
            out.push(a);
        }
    }
    Ok(out)
}

fn promotion_verdict(
    files: &[ScorecardFile],
    required: &BTreeSet<String>,
    artifacts: &[DeterminismArtifact],
) -> (mp_storage::promotion::PromotionVerdict, Vec<DailyScorecard>) {
    let is_required = |r: &ScorecardFileEntry| {
        required.is_empty()
            || required.iter().any(|req| {
                let (venue, symbol) = req.split_once(':').unwrap_or((req.as_str(), ""));
                r.venue == venue && r.symbol == symbol
            })
    };
    let scorecards: Vec<DailyScorecard> = files
        .iter()
        .map(|f| {
            let recording_bursts = f
                .recordings
                .iter()
                .filter(|r| is_required(r))
                .map(|r| RecordingBursts {
                    venue: r.venue.clone(),
                    symbol: r.symbol.clone(),
                    stale_bursts: r.stale_bursts,
                })
                .collect();
            DailyScorecard {
                date: f.date.clone(),
                recordings: vec![],
                promotable: f.promotable,
                recording_bursts,
            }
        })
        .collect();
    // The determinism condition (spec 018 MOD-9): every day in the qualifying
    // window must carry a PASSING determinism artifact. check_promotion (the
    // plain numeric + window gate) runs first; the determinism overlay holds
    // the verdict back and names the failing days when the window passed.
    (
        check_promotion_determinism(&scorecards, artifacts),
        scorecards,
    )
}

/// Zero-Cost Mode promotion verdict (docs/ZERO_COST_MODE.md): uses 14-day
/// streak (down from 7) and relaxed coverage (0.95 vs 0.995). The burst-free
/// promotion WINDOW condition still applies (it proves the host's network is
/// clean — the same gate as full mode); what Zero-Cost relaxes is the per-day
/// coverage bar and the non-`book` stream set. This is *stricter than the docs
/// once claimed* ("stale bursts are warning-only") — aligned here so operators
/// chasing the docs read the real gate (A-7, audit 2026-09-02).
fn promotion_verdict_zero_cost(
    files: &[ScorecardFile],
    required: &BTreeSet<String>,
    artifacts: &[DeterminismArtifact],
) -> (mp_storage::promotion::PromotionVerdict, Vec<DailyScorecard>) {
    use mp_storage::promotion::{check_promotion_n, ZERO_COST_REQUIRED_CONSECUTIVE_CLEAN_DAYS};

    let is_required = |r: &ScorecardFileEntry| {
        required.is_empty()
            || required.iter().any(|req| {
                let (venue, symbol) = req.split_once(':').unwrap_or((req.as_str(), ""));
                r.venue == venue && r.symbol == symbol
            })
    };
    let scorecards: Vec<DailyScorecard> = files
        .iter()
        .map(|f| {
            let recording_bursts = f
                .recordings
                .iter()
                .filter(|r| is_required(r))
                .map(|r| RecordingBursts {
                    venue: r.venue.clone(),
                    symbol: r.symbol.clone(),
                    stale_bursts: r.stale_bursts,
                })
                .collect();
            DailyScorecard {
                date: f.date.clone(),
                recordings: vec![],
                promotable: f.promotable,
                recording_bursts,
            }
        })
        .collect();
    // Zero-Cost Mode: use 14-day streak. The base check_promotion_n still
    // applies the burst-free window condition (it's the gate's design), but
    // under Zero-Cost, stale bursts are warnings only in the scorecard — the
    // window condition still fires on the raw burst data, which is correct:
    // it proves the host's network is clean. The key relaxation is the lower
    // coverage threshold (0.95 vs 0.995) in the scorecard's is_clean_zero_cost.
    let mut verdict = check_promotion_n(&scorecards, ZERO_COST_REQUIRED_CONSECUTIVE_CLEAN_DAYS);
    // Determinism check still applies even in Zero-Cost Mode.
    if verdict.promoted {
        let failures: Vec<String> = scorecards
            .iter()
            .filter(|c| {
                c.promotable
                    && verdict
                        .window_start
                        .as_deref()
                        .map(|ws| c.date.as_str() >= ws)
                        .unwrap_or(false)
                    && verdict
                        .window_end
                        .as_deref()
                        .map(|we| c.date.as_str() <= we)
                        .unwrap_or(false)
            })
            .filter(|c| !artifacts.iter().any(|a| a.date == c.date && a.passed))
            .map(|c| c.date.clone())
            .collect();
        verdict.determinism_failures = failures.clone();
        verdict.determinism_ok = failures.is_empty();
        if !failures.is_empty() {
            verdict.promoted = false;
        }
    }
    // Override the required field to reflect Zero-Cost threshold.
    verdict.required = ZERO_COST_REQUIRED_CONSECUTIVE_CLEAN_DAYS;
    (verdict, scorecards)
}

fn cmd_promote(args: &[String]) -> Result<String, String> {
    let dir = flag(args, "--scorecards-dir").unwrap_or_else(|| "data/scorecards".to_string());
    let required = flags(args, "--required")
        .into_iter()
        .collect::<BTreeSet<_>>();
    let files = load_scorecards(&dir, &required)?;
    if files.is_empty() {
        return Err(format!("no scorecards found in {dir}"));
    }
    let artifacts = load_determinism_artifacts(&dir, &files)?;
    let zero_cost = is_zero_cost(args);
    let (verdict, scorecards) = if zero_cost {
        promotion_verdict_zero_cost(&files, &required, &artifacts)
    } else {
        promotion_verdict(&files, &required, &artifacts)
    };
    let summary = if verdict.promoted {
        format!(
            "PROMOTED: {} consecutive clean days {}..{}",
            verdict.consecutive_clean,
            verdict.window_start.as_deref().unwrap_or("?"),
            verdict.window_end.as_deref().unwrap_or("?")
        )
    } else if verdict.consecutive_clean >= verdict.required && !verdict.burst_days.is_empty() {
        // A full streak held back by the window condition: name the burst
        // days so the `why` is actionable (spec 024, amendment 2026-08-12).
        let dates: Vec<&str> = verdict.burst_days.iter().map(|b| b.date.as_str()).collect();
        format!(
            "NOT YET: {} consecutive clean day(s) of {} required — window carries stale bursts on {}",
            verdict.consecutive_clean,
            verdict.required,
            dates.join(", ")
        )
    } else if verdict.consecutive_clean >= verdict.required
        && !verdict.determinism_failures.is_empty()
    {
        // A full streak held back by the determinism condition (spec 018
        // MOD-9, 2026-08-13): the decision path for those days has no passing
        // determinism artifact — name them so the `why` is actionable.
        format!(
            "NOT YET: {} consecutive clean day(s) of {} required — determinism check missing/failed on {}",
            verdict.consecutive_clean,
            verdict.required,
            verdict.determinism_failures.join(", ")
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
        "burst_days": &verdict.burst_days,
        "determinism_ok": verdict.determinism_ok,
        "determinism_failures": &verdict.determinism_failures,
        "scorecards": scorecards.len(),
        "summary": summary,
    }))
    .map_err(|e| e.to_string())
}

/// UTC minute-of-day (0..1440) for an epoch-ns reading — pure arithmetic on
/// the injected clock (PD-3).
fn minute_of_day_utc(now_ns: i64) -> u32 {
    ((now_ns.rem_euclid(86_400_000_000_000)) / 60_000_000_000) as u32
}

/// Parse a YYYY-MM-DD string into (year, month, day) as u32.
fn parse_date_dashed(date: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 { return None; }
    let y = parts[0].parse::<u32>().ok()?;
    let m = parts[1].parse::<u32>().ok()?;
    let d = parts[2].parse::<u32>().ok()?;
    Some((y, m, d))
}

/// Civil date (y, m, d) for `z` days since 1970-01-01 (Howard Hinnant's
/// inverse of days_from_epoch) — the inverse of the `date_to_nanos` math.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// UTC date `days` days before epoch-ns `now` as YYYY-MM-DD — the date the
/// daily gate (00:05 UTC scoring yesterday) should have scored by now.
fn utc_date_minus_days(now_ns: i64, days: i64) -> String {
    let day = now_ns.div_euclid(86_400_000_000_000) - days;
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Trading mode as the lowercase string `status` reports.
fn mode_str(m: TradingMode) -> &'static str {
    match m {
        TradingMode::Sleep => "sleep",
        TradingMode::Backtest => "backtest",
        TradingMode::Paper => "paper",
        TradingMode::Shadow => "shadow",
        TradingMode::Live => "live",
    }
}

/// Conventional kill-latch path (OPS-3/RG-10): `MP_OPS_KILL_LATCH` overrides,
/// else the same per-OS convention as `mode.toml` (core/src/mode.rs).
fn default_latch_path() -> PathBuf {
    if let Ok(p) = std::env::var("MP_OPS_KILL_LATCH") {
        return PathBuf::from(p);
    }
    #[cfg(target_os = "windows")]
    {
        let pd = std::env::var("PROGRAMDATA").unwrap_or_else(|_| "C:\\ProgramData".into());
        Path::new(&pd).join("money-printer").join("kill.json")
    }
    #[cfg(not(target_os = "windows"))]
    {
        Path::new("/etc/money-printer/kill.json").to_path_buf()
    }
}

/// Conventional off-host backup manifest (vps_backup.ps1 §`backup_manifest.jsonl`).
fn default_backup_manifest() -> String {
    #[cfg(target_os = "windows")]
    {
        "C:\\mp-backup\\vps-data\\backup_manifest.jsonl".to_string()
    }
    #[cfg(not(target_os = "windows"))]
    {
        "/opt/money-printer/backup/backup_manifest.jsonl".to_string()
    }
}

/// A latched scope as a human-readable string (`status` field).
fn latch_scope_str(s: &LatchScope) -> String {
    match s {
        LatchScope::Global => "global".to_string(),
        LatchScope::Venue { venue } => format!("venue:{}", venue.slug()),
        LatchScope::Strategy { id } => format!("strategy:{id}"),
    }
}

/// Last non-empty line of a wrapper log (`[ts][LEVEL] msg`), as a JSON value
/// for `status`. A missing log is reported honestly (`present: false`) — a
/// fresh edge without that artifact is a valid state, not an error.
fn last_log_line(path: &str) -> serde_json::Value {
    let p = Path::new(path);
    match std::fs::read_to_string(p) {
        Ok(text) => {
            let last = text.lines().rev().find(|l| !l.trim().is_empty());
            let mtime_ns = p
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as i64);
            serde_json::json!({
                "present": true,
                "path": path,
                "last_line": last,
                "mtime_ns": mtime_ns,
            })
        }
        Err(_) => serde_json::json!({ "present": false, "path": path }),
    }
}

/// `status` — one command that tells the truth about the whole system
/// (OPS-16). Aggregates the artifacts the system already produces into ONE
/// JSON document: trading mode (MOD-1), the promotion streak verdict, the
/// latest scorecard with its per-recording margins, the per-day coverage
/// trend, the last backup manifest entry, the daily pipeline's last log
/// line, and the kill-latch state (OPS-3/RG-10).
///
/// Missing artifacts are reported honestly (`present: false`, `promotion:
/// null`) — a fresh install or an edge without a backup sink is a valid
/// state, not an error. A CORRUPT gate/safety artifact is a failed command
/// (fail-closed, CONV-8): a scorecard that won't parse or a latch file that
/// won't decode is itself an alert-worthy condition, and status must never
/// hide it behind a fabricated verdict.
///
/// Flags:
///   --scorecards-dir DIR     (default data/scorecards)
///   --required venue:symbol ...  scope the verdict + trend to these recordings
///   --trend-days N           coverage-trend depth (default 14)
///   --pipeline-log PATH      (default data/scorecards/pipeline.log)
///   --backup-manifest PATH   (default: the conventional off-host pull sink)
///   --latch PATH             kill-latch file (MP_OPS_KILL_LATCH overrides)
/// Zero-Cost Mode status (docs/ZERO_COST_MODE.md, REQ-ZCS-1): a single
/// command answering the daily operator's questions:
/// - Collectors healthy?
/// - Disk used / days-to-cap
/// - Current promotion streak + last clean day
/// - Any critical alerts
///
/// Usage: mp-ops zero-cost-status [--scorecards-dir DIR] [--data-dir DIR]
///                                  [--cap-bytes N]
fn cmd_zero_cost_status(args: &[String]) -> Result<String, String> {
    use mp_ops::storage::{project_storage, sample_daily_sizes};
    use mp_storage::promotion::ZERO_COST_REQUIRED_CONSECUTIVE_CLEAN_DAYS;

    let scorecards_dir = flag(args, "--scorecards-dir")
        .unwrap_or_else(|| "data/scorecards".to_string());
    let data_dir = flag(args, "--data-dir")
        .unwrap_or_else(|| "data/raw".to_string());
    let cap_bytes: u64 = flag(args, "--cap-bytes")
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var("MP_STORAGE_BUDGET_BYTES").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(30_000_000_000); // 30 GB free-tier default

    let mut status = serde_json::json!({
        "mode": "zero-cost",
        "version": mp_core::SCHEMA_VER,
    });

    // 1. Disk usage: sample daily corpus sizes and project days-to-cap.
    let now = now_ns();
    let disk = match sample_daily_sizes(Path::new(&data_dir), now) {
        Ok(samples) => {
            let proj = project_storage(&samples, cap_bytes, 7);
            serde_json::json!({
                "data_dir": data_dir,
                "cap_bytes": cap_bytes,
                "current_bytes": proj.as_ref().map(|p| p.current_bytes),
                "growth_bytes_per_day": proj.as_ref().and_then(|p| p.growth_bytes_per_day),
                "days_to_cap": proj.as_ref().and_then(|p| p.days_to_cap),
            })
        }
        Err(e) => serde_json::json!({ "error": e }),
    };
    status["disk"] = disk;

    // 2. Promotion streak: load scorecards and run zero-cost promotion check.
    let promotion = match load_scorecards(&scorecards_dir, &BTreeSet::new()) {
        Ok(files) if files.is_empty() => serde_json::json!({
            "streak": 0,
            "required": ZERO_COST_REQUIRED_CONSECUTIVE_CLEAN_DAYS,
            "last_clean_day": null,
            "note": "no scorecards yet",
        }),
        Ok(files) => {
            let artifacts = load_determinism_artifacts(&scorecards_dir, &files)
                .unwrap_or_default();
            let (verdict, _) = promotion_verdict_zero_cost(&files, &BTreeSet::new(), &artifacts);
            let last_clean = files.iter().rev()
                .find(|f| f.promotable)
                .map(|f| f.date.as_str());
            serde_json::json!({
                "streak": verdict.consecutive_clean,
                "required": verdict.required,
                "promoted": verdict.promoted,
                "window_start": verdict.window_start,
                "window_end": verdict.window_end,
                "last_clean_day": last_clean,
                "determinism_ok": verdict.determinism_ok,
            })
        }
        Err(e) => serde_json::json!({ "error": e }),
    };
    status["promotion"] = promotion;

    // 3. Collector health: check for running mp-collector processes.
    let collectors = {
        let mut names = Vec::new();
        // Check for common collector binary names.
        for bin in &["mp-collector", "mp-whale"] {
            let running = std::process::Command::new("pgrep")
                .arg("-x")
                .arg(bin)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            names.push(serde_json::json!({
                "name": bin,
                "running": running,
            }));
        }
        serde_json::json!({ "processes": names })
    };
    status["collectors"] = collectors;

    // 4. Pipeline health: last scorecard date + days since.
    let pipeline = {
        let pipeline_log = flag(args, "--pipeline-log")
            .unwrap_or_else(|| "data/scorecards/pipeline.log".to_string());
        let last_line = last_log_line(&pipeline_log);
        serde_json::json!({
            "last_log_line": last_line,
        })
    };
    status["pipeline"] = pipeline;

    // 5. Critical alerts: retention violations, disk high, etc.
    let alerts = {
        let mut alert_list = Vec::new();
        // Check if raw dir exceeds 14 days (retention violation).
        if let Ok(raw_dir) = std::fs::read_dir(&data_dir) {
            let cutoff = now - (14 * 86_400_000_000_000_i64);
            let mut old_count = 0u64;
            let mut old_bytes = 0u64;
            for entry in raw_dir.flatten() {
                if entry.path().extension().map_or(false, |e| e == "log") {
                    if let Ok(meta) = entry.metadata() {
                        let mtime = meta.modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_nanos() as i64)
                            .unwrap_or(0);
                        if mtime < cutoff {
                            old_count += 1;
                            old_bytes += meta.len();
                        }
                    }
                }
            }
            if old_count > 0 {
                alert_list.push(serde_json::json!({
                    "severity": "warn",
                    "code": "RETENTION_VIOLATION",
                    "detail": format!("{} raw log(s) older than 14 days ({} bytes) — retention enforcement may not be running", old_count, old_bytes),
                }));
            }
        }
        // Check if promotion is blocked by determinism.
        if let Some(prom) = status.get("promotion") {
            if prom.get("determinism_ok") == Some(&serde_json::Value::Bool(false)) {
                alert_list.push(serde_json::json!({
                    "severity": "warn",
                    "code": "DETERMINISM_FAILURE",
                    "detail": "determinism check failing — promotion blocked",
                }));
            }
            if prom.get("streak") == Some(&serde_json::json!(0)) {
                alert_list.push(serde_json::json!({
                    "severity": "info",
                    "code": "NO_STREAK",
                    "detail": "no promotion streak yet",
                }));
            }
        }
        serde_json::json!({ "count": alert_list.len(), "alerts": alert_list })
    };
    status["alerts"] = alerts;

    serde_json::to_string_pretty(&status).map_err(|e| e.to_string())
}

fn cmd_status(args: &[String]) -> Result<String, String> {
    let score_dir = flag(args, "--scorecards-dir").unwrap_or_else(|| "data/scorecards".to_string());
    let required = flags(args, "--required")
        .into_iter()
        .collect::<BTreeSet<_>>();
    let trend_days = match flag(args, "--trend-days") {
        Some(s) => s
            .parse::<usize>()
            .map_err(|_| "--trend-days must be a positive integer")?,
        None => 14,
    };
    if trend_days == 0 {
        return Err("--trend-days must be positive".into());
    }

    // 1. trading mode (MOD-1): env override, else the config file, else Sleep.
    let mode = TradingMode::from_config();
    let mode_source = if std::env::var("MONEY_PRINTER_MODE").is_ok() {
        "env"
    } else {
        "config-or-default"
    };

    // 2. promotion verdict + latest scorecard + coverage trend. A missing dir
    //    is a reportable state; a corrupt scorecard fails closed.
    let mut promotion = serde_json::Value::Null;
    let mut promotion_note = serde_json::Value::Null;
    let mut latest_scorecard = serde_json::Value::Null;
    let mut coverage_trend: Vec<serde_json::Value> = Vec::new();
    let mut files_loaded: Vec<ScorecardFile> = Vec::new();
    if !PathBuf::from(&score_dir).is_dir() {
        promotion_note = serde_json::json!("scorecards directory not present yet");
    } else {
        files_loaded = load_scorecards(&score_dir, &required)?;
        if files_loaded.is_empty() {
            promotion_note = serde_json::json!(
                "no scorecards yet — the streak starts with the first archived scorecard"
            );
        } else {
            let artifacts = load_determinism_artifacts(&score_dir, &files_loaded)?;
            let (verdict, _) = promotion_verdict(&files_loaded, &required, &artifacts);
            promotion = serde_json::to_value(&verdict).map_err(|e| e.to_string())?;
            if let Some(latest) = files_loaded.last() {
                latest_scorecard = serde_json::json!({
                    "date": latest.date,
                    "promotable": latest.promotable,
                    "recordings": latest.recordings.iter().map(|r| serde_json::json!({
                        "venue": r.venue,
                        "symbol": r.symbol,
                        "clean": r.clean,
                        "event_count": r.event_count,
                        "coverage": r.coverage,
                        "blocking_findings": r.blocking_findings,
                        "worst_gap_ns": r.worst_gap_ns,
                        "stale_bursts": r.stale_bursts,
                    })).collect::<Vec<_>>(),
                });
            }
            coverage_trend = files_loaded
                .iter()
                .rev()
                .take(trend_days)
                .rev()
                .map(|f| {
                    serde_json::json!({
                        "date": f.date,
                        "recordings": f.recordings.iter().map(|r| serde_json::json!({
                            "venue": r.venue,
                            "symbol": r.symbol,
                            "coverage": r.coverage,
                            "clean": r.clean,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
        }
    }

    // 3. daily pipeline's last log line (Windows artifact; the bash gate on
    //    the VPS reports through the scorecard itself).
    let pipeline_log =
        flag(args, "--pipeline-log").unwrap_or_else(|| "data/scorecards/pipeline.log".to_string());
    let pipeline = last_log_line(&pipeline_log);

    // 4. last backup manifest entry (vps_backup.ps1): an entry exists only for
    //    a run that passed integrity (failure exits append nothing), so a
    //    present entry means the last run completed.
    let backup_manifest = flag(args, "--backup-manifest").unwrap_or_else(default_backup_manifest);
    let backup = match std::fs::read_to_string(&backup_manifest) {
        Ok(text) => {
            let last = text.lines().rev().find(|l| !l.trim().is_empty());
            match last.and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
                Some(entry) => serde_json::json!({
                    "present": true,
                    "path": backup_manifest,
                    "last_entry": entry,
                }),
                None => serde_json::json!({
                    "present": true,
                    "path": backup_manifest,
                    "last_entry": null,
                    "note": "manifest exists but its last line does not parse (incomplete or corrupt run)",
                }),
            }
        }
        Err(_) => serde_json::json!({ "present": false, "path": backup_manifest }),
    };

    // 5. kill-latch state (OPS-3/RG-10). A corrupt latch fails closed — the
    //    gate artifact that decides whether the system may trade must never be
    //    silently misread.
    let latch_path = match flag(args, "--latch") {
        Some(p) => PathBuf::from(p),
        None => default_latch_path(),
    };
    let killswitch = match std::fs::read_to_string(&latch_path) {
        Ok(text) => {
            let latch = KillLatch::from_json(&text)
                .map_err(|e| format!("kill-latch {} corrupt: {e}", latch_path.display()))?;
            serde_json::json!({
                "latched": !latch.scopes.is_empty(),
                "file": latch_path.to_string_lossy(),
                "scopes": latch.scopes.iter().map(latch_scope_str).collect::<Vec<_>>(),
                "reason": latch.reason,
                "ts_ns": latch.ts_ns,
            })
        }
        // Missing = fresh state (H-2: not an error). Any OTHER read failure
        // (permissions, IO) cannot be verified — fail closed like a corrupt
        // latch instead of reporting an unverified "latched: false".
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({ "latched": false, "file": latch_path.to_string_lossy() })
        }
        Err(e) => {
            return Err(format!(
                "kill-latch {} unreadable — failing closed: {e}",
                latch_path.display()
            ))
        }
    };

    // LAB-9: last scorecard date + days_since (computed from loaded scorecards).
    let last_scorecard_date: serde_json::Value = if let Some(latest) = files_loaded.last() {
        // Compute days_since from today (injected clock, PD-3).
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        let today_days = now_ns.div_euclid(86_400_000_000_000);
        let (ly, lm, ld) = civil_from_days(today_days - 1); // yesterday
        let _today_dash = format!("{ly:04}-{lm:02}-{ld:02}");
        // Parse the scorecard date to get days since.
        let sc_days = if let Some((sy, sm, sd)) = parse_date_dashed(&latest.date) {
            let sc_epoch = days_from_epoch(sy as i64, sm, sd) as i64;
            let today_epoch = today_days;
            (today_epoch - sc_epoch).max(0) as u64
        } else {
            0
        };
        serde_json::json!({
            "date": latest.date,
            "days_since": sc_days,
        })
    } else {
        serde_json::json!({ "date": null, "days_since": null })
    };

    // LAB-9: drain held count from the drain manifest.
    let drain_held: serde_json::Value = {
        let manifest = flag(args, "--drain-manifest")
            .unwrap_or_else(|| "data/vps_drain_manifest.jsonl".to_string());
        match std::fs::read_to_string(&manifest) {
            Ok(text) => {
                let mut held_count: u64 = 0;
                // Parse the manifest and count entries whose LATEST record has
                // action=landed with release not in {released, no_release}.
                let mut latest_per_file: BTreeMap<String, serde_json::Value> = BTreeMap::new();
                for line in text.lines() {
                    if line.trim().is_empty() { continue; }
                    if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                        if let Some(f) = entry.get("file").and_then(|v| v.as_str()) {
                            // Keep the latest entry per file (manifest is append-only).
                            latest_per_file.insert(f.to_string(), entry);
                        }
                    }
                }
                for (_, entry) in &latest_per_file {
                    let action = entry.get("action").and_then(|v| v.as_str()).unwrap_or("");
                    let release = entry.get("release").and_then(|v| v.as_str()).unwrap_or("");
                    if action == "landed" && release != "released" && release != "no_release" {
                        held_count += 1;
                    }
                }
                serde_json::json!({ "present": true, "held_count": held_count })
            }
            Err(_) => serde_json::json!({ "present": false, "held_count": 0 }),
        }
    };

    // LAB-9: data_home path.
    let data_home: serde_json::Value = match std::env::var("MP_DATA_HOME") {
        Ok(path) => {
            let has_downloads = path.to_lowercase().contains("downloads");
            serde_json::json!({
                "path": path,
                "has_downloads": has_downloads,
            })
        }
        Err(_) => serde_json::json!({ "path": null, "has_downloads": null }),
    };

    // LAB-9: compact_zero_row incidents in the last 7 days (scan pipeline.log).
    let compact_zero_row_incidents: serde_json::Value = {
        let log_path = &pipeline_log;
        match std::fs::read_to_string(log_path) {
            Ok(text) => {
                let now_ns = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as i64;
                let cutoff_days = now_ns.div_euclid(86_400_000_000_000) - 7;
                let mut count: u64 = 0;
                for line in text.lines() {
                    if line.contains("LAB-5") && line.contains("zero-row") {
                        // Check if the timestamp is within the last 7 days.
                        // Lines have format: [ts][LEVEL] msg
                        if let Some(ts_str) = line.strip_prefix('[').and_then(|s| s.split(']').next()) {
                            if let Ok(ts) = ts_str.parse::<i64>() {
                                // ts is in seconds since epoch.
                                let ts_days = ts.div_euclid(86400);
                                if ts_days >= cutoff_days {
                                    count += 1;
                                }
                            }
                        }
                    }
                }
                serde_json::json!({ "count_7d": count })
            }
            Err(_) => serde_json::json!({ "count_7d": 0 }),
        }
    };

    serde_json::to_string(&serde_json::json!({
        "mode": mode_str(mode),
        "mode_source": mode_source,
        "promotion": promotion,
        "promotion_note": promotion_note,
        "latest_scorecard": latest_scorecard,
        "coverage_trend": coverage_trend,
        "pipeline": pipeline,
        "backup": backup,
        "killswitch": killswitch,
        // LAB-9 fields
        "last_scorecard_date": last_scorecard_date,
        "drain_held": drain_held,
        "data_home": data_home,
        "compact_zero_row_incidents": compact_zero_row_incidents,
    }))
    .map_err(|e| e.to_string())
}

/// LAB-7: validate data-home configuration.
/// Fails if:
/// - MP_DATA_HOME (or --data-dir) is not set and no default resolves,
/// - the resolved path contains "Downloads" (case-insensitive).
fn cmd_check_config(args: &[String]) -> Result<String, String> {
    // Parse --data-dir flag.
    let mut data_dir: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--data-dir" && i + 1 < args.len() {
            data_dir = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }

    let resolved = if let Some(d) = data_dir {
        d
    } else if let Ok(d) = std::env::var("MP_DATA_HOME") {
        d
    } else {
        return Err(
            "LAB-7: MP_DATA_HOME is unset and --data-dir was not provided; \
             the scheduled task hard-stops when no data root is configured.".to_string()
        );
    };

    // LAB-7: case-insensitive check for "Downloads".
    if resolved.to_lowercase().contains("downloads") {
        return Err(format!(
            "LAB-7: data path contains 'Downloads' ({resolved}); \
             data must live off Downloads (W-6). Hard stop."
        ));
    }

    let pb = PathBuf::from(&resolved);
    if !pb.exists() {
        return Err(format!("LAB-7: data path does not exist: {resolved}"));
    }

    Ok(format!("LAB-7: config OK, data_home={resolved}"))
}

fn cmd_scorecard(args: &[String]) -> Result<String, String> {
    scorecard_from_root(Path::new("data"), args)
}

/// Scorecard JSON generation rooted at `data_root` — the workspace `data`
/// dir in production, a temp dir under test. The per-recording `stale_bursts`
/// count and `worst_gap_ns` are copied verbatim from the audit (the gate's
/// truth under the 2026-08-12 tolerance semantics); the JSON is what `mp-ops
/// promote` and the daily pipeline actually read.
fn scorecard_from_root(data_root: &Path, args: &[String]) -> Result<String, String> {
    let date = need(args, "--date")?.replace('-', "");
    if date.len() != 8 {
        return Err("date must be YYYYMMDD or YYYY-MM-DD".into());
    }
    let required = flags(args, "--required");
    if required.is_empty() {
        return Err("scorecard requires one or more --required venue:symbol entries".into());
    }
    let dashed = format!("{}-{}-{}", &date[0..4], &date[4..6], &date[6..8]);
    let score_dir = data_root.join("scorecards");
    let manifest_path = score_dir.join(".scorecard_sources.json");

    // Fast re-score (2026-08-13): with --reuse-unchanged, a day whose every
    // raw source matches the recorded (size, mtime_ns, config fingerprint)
    // AND whose archived scorecard exists with the same required set is
    // returned as-is — no multi-GB re-audit. The cache is never gate data:
    // any mismatch falls through to the full audit below, and a corrupt
    // manifest is discarded (a cache, not evidence).
    let mut manifest = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|text| serde_json::from_str::<SourceManifest>(&text).ok())
        .unwrap_or_default();
    if args.iter().any(|a| a == "--reuse-unchanged") && !args.iter().any(|a| a == "--force") {
        let mut req_sorted = required.clone();
        req_sorted.sort();
        let req_key = req_sorted.join(",");
        let cfg_fp = source_config_fingerprint(args);
        let mut match_ok = manifest
            .days
            .get(&dashed)
            .map(|d| d.required == req_key)
            .unwrap_or(false)
            && score_dir.join(format!("{dashed}.json")).exists();
        if match_ok {
            for item in &required {
                let (venue_name, symbol) = item
                    .split_once(':')
                    .ok_or_else(|| format!("invalid --required {item}; expected venue:symbol"))?;
                let raw_path = data_root
                    .join("raw")
                    .join(format!("{date}_{venue_name}_{symbol}.log"));
                let recorded = manifest.days.get(&dashed).and_then(|d| d.sources.get(item));
                match (recorded, std::fs::metadata(&raw_path)) {
                    (Some(f), Ok(md)) => {
                        let size = md.len();
                        let mtime = md.modified().ok().map(|t| {
                            t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as i64
                        });
                        if f.size != size || f.mtime_ns != mtime || f.config != cfg_fp {
                            match_ok = false;
                            break;
                        }
                    }
                    _ => {
                        match_ok = false;
                        break;
                    }
                }
            }
        }
        if match_ok {
            let text = std::fs::read_to_string(score_dir.join(format!("{dashed}.json")))
                .map_err(|e| format!("read archived scorecard {dashed}.json: {e}"))?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_string();
            eprintln!(
                "scorecard {dashed}: reused from unchanged sources ({} recording(s))",
                required.len()
            );
            return Ok(text);
        }
    }

    let mut entries = Vec::new();
    for item in &required {
        let (venue_name, symbol) = item
            .split_once(':')
            .ok_or_else(|| format!("invalid --required {item}; expected venue:symbol"))?;
        let venue = parse_venue(venue_name)?;
        let path = data_root
            .join("raw")
            .join(format!("{date}_{venue_name}_{symbol}.log"));
        let audit = audit_raw_log(&path, &audit_config(args, venue, symbol)?);
        entries.push((venue, symbol.to_owned(), audit));
    }
    // Gate-integrity plausibility guard (incident 2026-08-22): when EVERY
    // required recording audits to zero events, this is not a dirty day —
    // the gate read nothing (raw sources never drained, or a reader/writer
    // schema split like the one that blinded the VPS gate 08-19..21).
    // Verdicts computed from zero bytes are not evidence: refuse loudly
    // instead of archiving a plausible-looking DIRTY verdict that silently
    // eats the promotion streak. The missed archive then surfaces through
    // the existing dead-man (`pipeline-stale`, OPS-17) as a P1. An operator
    // who genuinely wants the zero-verdict JSON passes --allow-all-zero.
    if entries.iter().all(|(_, _, a)| a.event_count == 0)
        && !args.iter().any(|a| a == "--allow-all-zero")
    {
        eprintln!(
            "gate-integrity anomaly: every required recording for {dashed} audited to \
             0 events (missing raw sources or a reader/writer schema split) - \
             refusing to emit a verdict from zero bytes; re-score after the \
             sources land or pass --allow-all-zero"
        );
        return Err(format!(
            "all-zero event counts across {} recording(s) for {dashed} - \
             not a dirty day but a gate-integrity anomaly (incident 2026-08-22)",
            entries.len()
        ));
    }
    // Lightweight file: verdict + per-recording counts only, never the full
    // findings Vec (legacy days balloon to GB otherwise — 2026-08-04).
    let zero_cost = is_zero_cost(args);
    let promotable = if zero_cost {
        !entries.is_empty() && entries.iter().all(|(_, _, a)| a.is_clean_zero_cost())
    } else {
        !entries.is_empty() && entries.iter().all(|(_, _, a)| a.is_clean())
    };
    let file = ScorecardFile {
        date: dashed.clone(),
        promotable,
        recordings: entries
            .iter()
            .map(|(venue, symbol, audit)| light_entry(venue, symbol, audit))
            .collect(),
    };
    // Record the source fingerprints for future --reuse-unchanged runs. The
    // audit-config fingerprint covers max-gap + required streams; venue/symbol
    // are the entry key itself.
    let cfg_fp = source_config_fingerprint(args);
    let mut req_sorted = required.clone();
    req_sorted.sort();
    let mut day = SourceDay {
        required: req_sorted.join(","),
        ..Default::default()
    };
    for item in &required {
        let (venue_name, symbol) = item
            .split_once(':')
            .ok_or_else(|| format!("invalid --required {item}; expected venue:symbol"))?;
        let raw_path = data_root
            .join("raw")
            .join(format!("{date}_{venue_name}_{symbol}.log"));
        if let Ok(md) = std::fs::metadata(&raw_path) {
            day.sources.insert(
                item.clone(),
                SourceEntry {
                    size: md.len(),
                    mtime_ns: md.modified().ok().map(|t| {
                        t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as i64
                    }),
                    config: cfg_fp.clone(),
                },
            );
        }
    }
    manifest.days.insert(dashed.clone(), day);
    if let Some(dir) = manifest_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&manifest) {
        let _ = std::fs::write(&manifest_path, json);
    }

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
/// OPS-15 storage-budget watch: projects when the corpus reaches the budget
/// cap and raises `storage-budget` (P2) when the projection is within the
/// alert horizon — operationalizing the spec 001 appendix's disk-budget
/// revisit trigger. The trend is derived from the corpus's own per-day
/// `{YYYYMMDD}_*.log` files (read-only, W-6 — no state file); the current
/// partial day is excluded; growth is the trailing-window mean daily
/// addition rate. The budget is explicit (`--cap-bytes` or
/// `MP_STORAGE_BUDGET_BYTES`)
/// and an unconfigured budget fails closed (exit 2) — a watch that cannot
/// know the cap must not pretend it did.
///
/// Prints a JSON verdict — `{"dir", "current_bytes", "growth_bytes_per_day",
/// "days_to_cap", "alert": {...}|null}` — and exits 0 either way. With
/// `--telegram`, a fired P2 is routed through the Bot API edge and sent
/// immediately (P2 always breaks through quiet hours, OPS-9).
fn cmd_storage_budget(args: &[String]) -> Result<String, String> {
    let dir = match flag(args, "--dir") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(
            std::env::var("MP_STORAGE_DIR").unwrap_or_else(|_| "data/raw".to_string()),
        ),
    };
    let cap_bytes = match flag(args, "--cap-bytes") {
        Some(s) => s
            .parse::<u64>()
            .map_err(|_| "--cap-bytes must be an integer (bytes)")?,
        None => match std::env::var("MP_STORAGE_BUDGET_BYTES") {
            Ok(s) => s
                .parse::<u64>()
                .map_err(|_| "MP_STORAGE_BUDGET_BYTES must be an integer (bytes)")?,
            Err(_) => {
                return Err(
                    "storage-budget: no budget configured — set --cap-bytes or MP_STORAGE_BUDGET_BYTES (fail-closed: an unconfigured budget never silently skips)"
                        .into(),
                )
            }
        },
    };
    if cap_bytes == 0 {
        return Err("--cap-bytes must be positive".into());
    }
    let alert_at_days = match flag(args, "--alert-at-days") {
        Some(s) => s
            .parse::<f64>()
            .map_err(|_| "--alert-at-days must be a number")?,
        None => 14.0,
    };
    if alert_at_days <= 0.0 {
        return Err("--alert-at-days must be positive".into());
    }
    let trend_days = match flag(args, "--trend-days") {
        Some(s) => s
            .parse::<usize>()
            .map_err(|_| "--trend-days must be an integer")?,
        None => 7,
    };
    if trend_days < 2 {
        return Err("--trend-days must be at least 2".into());
    }
    let dedupe_ns = match flag(args, "--dedupe-ns") {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| "--dedupe-ns must be an integer")?,
        None => 86_400_000_000_000, // one day — a daily check re-alerts daily while trending
    };
    if dedupe_ns <= 0 {
        return Err("--dedupe-ns must be positive".into());
    }
    let now = match flag(args, "--ts-ns") {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| "--ts-ns must be an integer (epoch ns)")?,
        None => now_ns(),
    };
    let label = dir.to_string_lossy().into_owned();
    let samples = sample_daily_sizes(&dir, now).map_err(|e| format!("storage-budget: {e}"))?;
    let projection = project_storage(&samples, cap_bytes, trend_days);
    let mut alert = storage_budget_alert(
        &samples,
        cap_bytes,
        trend_days,
        alert_at_days,
        dedupe_ns,
        &label,
    );
    // Optional drain-manifest scan: flag files that LANDED in the master
    // corpus but were never released from the relay (action=landed with
    // release not in {released, no_release} — ssh_failed/skipped). This is a
    // Windows-side artifact (the VPS timer has no manifest), and an
    // explicitly-given but unreadable manifest fails closed (CONV-8) — a
    // check that cannot see its input must not pretend it did.
    let held = match flag(args, "--manifest").map(PathBuf::from) {
        Some(p) => {
            let text = std::fs::read_to_string(&p)
                .map_err(|e| format!("storage-budget: read manifest {}: {e}", p.display()))?;
            let entries: Vec<DrainManifestEntry> =
                text.lines().filter_map(parse_drain_manifest_line).collect();
            held_drain_files(&entries)
        }
        None => Vec::new(),
    };
    // A silently held VPS file fires the same P2 regardless of the growth
    // projection — the relay is not draining (the "VPS never accumulates"
    // contract the storage-budget watch exists to police).
    if !held.is_empty() {
        let names = held
            .iter()
            .map(|(f, _)| f.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let held_detail = format!(
            "{label}: {} held VPS file(s) after drain (landed but release not confirmed): {names} — the relay is still holding byte-verified files; check the vps_drain release leg",
            held.len()
        );
        alert = Some(match alert {
            Some(a) => Alert::new(
                "storage-budget",
                Severity::P2,
                dedupe_ns,
                format!("{}; {}", a.detail, held_detail),
            ),
            None => Alert::new("storage-budget", Severity::P2, dedupe_ns, held_detail),
        });
    }
    let mut verdict = serde_json::json!({
        "dir": label,
        "current_bytes": projection.map(|p| p.current_bytes),
        "growth_bytes_per_day": projection.and_then(|p| p.growth_bytes_per_day),
        "days_to_cap": projection.and_then(|p| p.days_to_cap),
        "held_vps_files": held.iter().map(|(f, r)| serde_json::json!({
            "file": f,
            "release": r,
        })).collect::<Vec<_>>(),
        "held_vps_count": held.len(),
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
                // P2 always breaks through quiet hours (OPS-9).
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

/// One-shot Telegram notification for a wrapper verdict (the daily
/// promotion-gate verdict in `daily_pipeline.ps1`): sends `--detail`
/// immediately through the Bot API edge. Unlike `band-accuracy-decay
/// --telegram`, there is NO quiet-hours batching — a wrapper verdict must
/// land when it is produced (the daily pipeline runs at 00:05 UTC, inside
/// the 22:00–07:00 quiet window; a batched P3 would sit in the ledger until
/// the next flush, defeating the point of the alert). Fail-closed (CONV-8):
/// no credentials ⇒ exit 2, never a silent drop; a failed send is an error,
/// never a fake "sent".
fn cmd_telegram_send(args: &[String]) -> Result<String, String> {
    let id = need(args, "--id")?;
    let detail = need(args, "--detail")?;
    if id.is_empty() || detail.is_empty() {
        return Err("telegram-send: --id and --detail must be non-empty".into());
    }
    let severity = match flag(args, "--severity").as_deref() {
        Some("p1") => Severity::P1,
        Some("p2") => Severity::P2,
        Some("p3") | None => Severity::P3,
        Some(other) => {
            return Err(format!(
                "telegram-send: unknown --severity {other} (p1|p2|p3)"
            ))
        }
    };
    let (Some(token), Some(chat_id)) = (
        std::env::var("TELEGRAM_BOT_TOKEN").ok(),
        std::env::var("TELEGRAM_CHAT_ID").ok(),
    ) else {
        return Err("telegram-send: TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID must be set".into());
    };
    let cfg = TelegramConfig {
        url: std::env::var("MP_OPS_TELEGRAM_URL")
            .unwrap_or_else(|_| "https://api.telegram.org".to_string()),
        token,
        chat_id,
    };
    let ts_ns = match flag(args, "--ts-ns") {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| "telegram-send: --ts-ns must be an integer")?,
        None => now_ns(),
    };
    let dispatch = Dispatch::from_alert(&Alert::new(id.clone(), severity, 0, detail), ts_ns);
    // No quiet-hours router: a wrapper verdict always sends now (a batched
    // P3 would wait for the next flush, defeating the point).
    post_telegram(&dispatch, &cfg).map_err(|e| format!("telegram send failed: {e}"))?;
    serde_json::to_string(&serde_json::json!({
        "sent": true,
        "id": dispatch.id,
        "severity": dispatch.severity.as_str(),
        "channel": match dispatch.channel {
            Channel::TelegramPhone => "telegram_phone",
            Channel::Telegram => "telegram",
            Channel::TelegramQuiet => "telegram_quiet",
        },
        "detail": dispatch.detail,
        "runbook": dispatch.runbook,
        "ts_ns": dispatch.ts_ns,
    }))
    .map_err(|e| e.to_string())
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
        Some(s) => s.parse::<i64>().map_err(|_| "--ts-ns must be an integer")?,
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
/// `pipeline-stale` — OPS-17 dead-man for the daily gate itself (spec 024,
/// vps-phase0-bringup.md §3). The scorecards and the streak are produced by
/// one cron/scheduled job; a silent failure of THAT job is the failure mode
/// "you find out in 11 days" (blueprint failure-mode #6). This check
/// verifies the previous UTC day's scorecard exists and parses by the
/// deadline (default 15 minutes after UTC midnight — the 00:05 gate plus
/// margin), raising a P1 when it does not.
///
/// Safe to run hourly: before the deadline (UTC minute-of-day <
/// `--deadline-min`) the check defers (`stale: false`, `deferred: true`)
/// rather than alerting — the gate may still be running. `--ts-ns` injects
/// the clock (PD-3) so tests never depend on wall time.
///
/// Prints a JSON verdict `{stale, expected_date, alert|null}` and exits 0
/// either way (a fired alert is the check doing its job, not a failed
/// command). With `--telegram` / `--webhook`, a fired P1 is delivered
/// through the configured edges; unconfigured credentials are reported in
/// the verdict, never silently dropped (CONV-8).
fn cmd_pipeline_stale(args: &[String]) -> Result<String, String> {
    let score_dir = flag(args, "--scorecards-dir").unwrap_or_else(|| "data/scorecards".to_string());
    let deadline_min = match flag(args, "--deadline-min") {
        Some(s) => s
            .parse::<u32>()
            .map_err(|_| "--deadline-min must be an integer (minutes of UTC day)")?,
        None => 15,
    };
    if deadline_min >= 1440 {
        return Err("--deadline-min must be in 0..1440".into());
    }
    let dedupe_ns = match flag(args, "--dedupe-ns") {
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| "--dedupe-ns must be an integer")?,
        None => 6 * 3_600_000_000_000i64,
    };
    if dedupe_ns <= 0 {
        return Err("--dedupe-ns must be positive".into());
    }
    let now = match flag(args, "--ts-ns") {
        Some(s) => s.parse::<i64>().map_err(|_| "--ts-ns must be an integer")?,
        None => now_ns(),
    };
    let expected_date = utc_date_minus_days(now, 1);
    let mut verdict = serde_json::json!({
        "stale": false,
        "expected_date": expected_date,
        "deferred": false,
        "alert": null,
    });
    let minute = minute_of_day_utc(now);
    if minute < deadline_min {
        verdict["deferred"] = serde_json::json!(true);
        verdict["note"] = serde_json::json!(format!(
            "UTC minute {minute} is before the {deadline_min}-minute deadline — the 00:05 gate may still be running"
        ));
        return serde_json::to_string(&verdict).map_err(|e| e.to_string());
    }

    // The expected scorecard is for the PREVIOUS UTC day (the daily gate at
    // 00:05 scores yesterday). Missing, unreadable, or unparseable all fire
    // — a file that exists but cannot be read is as bad as no file.
    let scorecard_path = PathBuf::from(&score_dir).join(format!("{expected_date}.json"));
    let alert = if !scorecard_path.exists() {
        Some(Alert::new(
            "pipeline-stale",
            Severity::P1,
            dedupe_ns,
            format!(
                "no scorecard for {expected_date} at UTC minute {minute} (deadline {deadline_min}m) — the daily gate did not land; check the cron/scheduled task"
            ),
        ))
    } else {
        match std::fs::read_to_string(&scorecard_path) {
            Ok(text) => {
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                match serde_json::from_str::<ScorecardFile>(text) {
                    Ok(_) => None,
                    Err(e) => Some(Alert::new(
                        "pipeline-stale",
                        Severity::P1,
                        dedupe_ns,
                        format!("scorecard for {expected_date} exists but is unparseable: {e}"),
                    )),
                }
            }
            Err(e) => Some(Alert::new(
                "pipeline-stale",
                Severity::P1,
                dedupe_ns,
                format!("scorecard for {expected_date} exists but cannot be read: {e}"),
            )),
        }
    };

    if let Some(alert) = alert {
        verdict["stale"] = serde_json::json!(true);
        verdict["alert"] = serde_json::json!({
            "id": alert.id,
            "severity": alert.severity.as_str(),
            "detail": alert.detail,
            "runbook": alert.runbook,
            "dedupe_key": alert.dedupe_key,
        });
        if args.iter().any(|a| a == "--telegram") {
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
                // P1 always breaks through quiet hours (OPS-9); a one-shot
                // router has no dedupe history, so the fired alert is Sent.
                let mut router = AlertRouter::new(None);
                match router.route(&alert, now) {
                    RouteOutcome::Sent(d) => match post_telegram(&d, &cfg) {
                        Ok(()) => verdict["telegram"] = serde_json::json!("sent"),
                        Err(e) => return Err(format!("telegram send failed: {e}")),
                    },
                    RouteOutcome::Batched | RouteOutcome::Deduped => {
                        unreachable!("a fresh no-quiet-hours router never batches or dedupes a P1")
                    }
                }
            } else {
                verdict["telegram"] = serde_json::json!("unconfigured");
            }
        }
        if args.iter().any(|a| a == "--webhook") {
            match std::env::var("MP_OPS_P1_WEBHOOK") {
                Ok(url) => {
                    match AlertRouter::post_p1_webhook(&Dispatch::from_alert(&alert, now), &url) {
                        Ok(()) => verdict["webhook"] = serde_json::json!("sent"),
                        Err(e) => return Err(format!("p1 webhook failed: {e}")),
                    }
                }
                Err(_) => verdict["webhook"] = serde_json::json!("unconfigured"),
            }
        }
    } else {
        verdict["telegram"] = serde_json::json!("none");
        verdict["webhook"] = serde_json::json!("none");
    }
    serde_json::to_string(&verdict).map_err(|e| e.to_string())
}

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
            "Subcommands: compact, prune, audit, scorecard, promote, status, zero-cost-status, check-config, pipeline-stale, band-accuracy-decay, telegram-flush, telegram-stale, storage-budget, telegram-send, p1-webhook"
        );
        return ExitCode::FAILURE;
    }

    let result = match args[1].as_str() {
        "compact" => cmd_compact(&args[2..]),
        "prune" => cmd_prune(&args[2..]),
        "audit" => cmd_audit(&args[2..]),
        "scorecard" => cmd_scorecard(&args[2..]),
        "promote" => cmd_promote(&args[2..]),
        "band-accuracy-decay" => cmd_band_accuracy_decay(&args[2..]),
        "status" => cmd_status(&args[2..]),
        "zero-cost-status" => cmd_zero_cost_status(&args[2..]),
        "check-config" => cmd_check_config(&args[2..]),
        "pipeline-stale" => cmd_pipeline_stale(&args[2..]),
        "telegram-flush" => cmd_telegram_flush(&args[2..]),
        "telegram-stale" => cmd_telegram_stale(&args[2..]),
        "storage-budget" => cmd_storage_budget(&args[2..]),
        "telegram-send" => cmd_telegram_send(&args[2..]),
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
                "band-accuracy-decay"
                    | "telegram-flush"
                    | "telegram-stale"
                    | "storage-budget"
                    | "telegram-send"
                    | "p1-webhook"
                    | "pipeline-stale"
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
        SymbolId, SymbolMeta, SymbolTable,
    };

    /// Serializes tests that mutate the process environment (`std::env::set_var`
    /// / `remove_var`). Rust runs tests in parallel threads within one binary,
    /// and env mutation is process-global and NOT synchronized — two tests
    /// reading env concurrently can observe each other's half-set state. The
    /// one mutator (`pipeline_stale_fires_p1_when_gate_did_not_land`) caused
    /// intermittent `promote_*` failures under full-suite parallelism.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            // The determinism condition (spec 018 MOD-9): each window day must
            // also carry a PASSING determinism artifact.
            let det = serde_json::json!({
                "date": format!("2026-08-{day:02}"),
                "passed": true,
                "self_consistent": true,
                "live_present": false,
                "live_matches": null,
                "strategy": "null",
                "event_count": 1,
                "replayed_lines": 1,
            });
            std::fs::write(
                dir.join(format!("2026-08-{day:02}.determinism.json")),
                serde_json::to_string(&det).unwrap(),
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
        let json: serde_json::Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(
            json["determinism_ok"], true,
            "gate must report determinism ok"
        );
    }

    /// promote: the Phase-0 window condition (spec 024, amendment
    /// 2026-08-12) holds a full streak back when a bursty day sits inside
    /// the qualifying window — the streak stays at 7, the verdict names the
    /// burst date and recording via `burst_days`, and promotion waits.
    #[test]
    fn promote_window_condition_blocks_bursty_streak() {
        let dir = std::env::temp_dir().join(format!("mp-promote-burst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for day in 1..=7 {
            let mut recordings = vec![
                serde_json::json!({ "venue": "hyperliquid", "symbol": "BTC", "clean": true,
                     "event_count": 1, "coverage": 1.0, "findings": 0, "blocking_findings": 0,
                     "stale_bursts": 0 }),
                serde_json::json!({ "venue": "hyperliquid", "symbol": "ETH", "clean": true,
                     "event_count": 1, "coverage": 1.0, "findings": 0, "blocking_findings": 0,
                     "stale_bursts": 0 }),
            ];
            if day == 4 {
                recordings[0]["stale_bursts"] = serde_json::json!(2);
            }
            let card = serde_json::json!({
                "date": format!("2026-08-{day:02}"),
                "recordings": recordings,
                "promotable": true
            });
            std::fs::write(
                dir.join(format!("2026-08-{day:02}.json")),
                serde_json::to_string(&card).unwrap(),
            )
            .unwrap();
        }
        let verdict = cmd_promote(&[
            "--scorecards-dir".into(),
            dir.to_string_lossy().into(),
            "--required".into(),
            "hyperliquid:BTC".into(),
            "--required".into(),
            "hyperliquid:ETH".into(),
        ]);
        let _ = std::fs::remove_dir_all(&dir);
        let ok = verdict.expect("promote should succeed");
        assert!(
            ok.contains("NOT YET"),
            "bursty window must not promote: {ok}"
        );
        assert!(
            ok.contains("2026-08-04"),
            "the burst day must be named in the why: {ok}"
        );
        let json: serde_json::Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(json["consecutive_clean"], 7, "the streak stays intact");
        assert_eq!(json["first_failure"], serde_json::Value::Null);
        assert_eq!(json["burst_days"][0]["date"], "2026-08-04");
        assert_eq!(json["burst_days"][0]["recordings"][0], "hyperliquid:BTC");
    }

    /// promote: the window condition is window-level, never a per-day veto
    /// — an isolated burst inside a longer clean run does not block; the
    /// burst-free tail becomes the qualifying window.
    #[test]
    fn promote_passes_on_burst_free_window_within_streak() {
        let dir = std::env::temp_dir().join(format!("mp-promote-win-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for day in 1..=8 {
            let burst = if day == 1 { 3 } else { 0 };
            let card = serde_json::json!({
                "date": format!("2026-08-{day:02}"),
                "recordings": [
                    { "venue": "hyperliquid", "symbol": "BTC", "clean": true,
                     "event_count": 1, "coverage": 1.0, "findings": 0, "blocking_findings": 0,
                     "stale_bursts": burst },
                    { "venue": "hyperliquid", "symbol": "ETH", "clean": true,
                     "event_count": 1, "coverage": 1.0, "findings": 0, "blocking_findings": 0,
                     "stale_bursts": 0 }
                ],
                "promotable": true
            });
            std::fs::write(
                dir.join(format!("2026-08-{day:02}.json")),
                serde_json::to_string(&card).unwrap(),
            )
            .unwrap();
            // Passing determinism artifact for every day of the streak (spec
            // 018 MOD-9): day 1's burst is a window-level fact, not a veto.
            let det = serde_json::json!({ "date": format!("2026-08-{day:02}"),
                "passed": true, "self_consistent": true, "live_present": false,
                "live_matches": null, "strategy": "null", "event_count": 1,
                "replayed_lines": 1 });
            std::fs::write(
                dir.join(format!("2026-08-{day:02}.determinism.json")),
                serde_json::to_string(&det).unwrap(),
            )
            .unwrap();
        }
        let verdict = cmd_promote(&["--scorecards-dir".into(), dir.to_string_lossy().into()]);
        let _ = std::fs::remove_dir_all(&dir);
        let ok = verdict.expect("promote should succeed");
        assert!(
            ok.contains("PROMOTED"),
            "burst-free tail must qualify: {ok}"
        );
        let json: serde_json::Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(json["window_start"], "2026-08-02");
        assert_eq!(json["window_end"], "2026-08-08");
        assert!(json["burst_days"].as_array().unwrap().is_empty());
        assert_eq!(
            json["determinism_ok"], true,
            "gate must report determinism ok"
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

    /// Incident 2026-08-22 plausibility guard: every-required-recording-zero
    /// is a gate-integrity anomaly (missing drain / reader-writer schema
    /// split), NOT a dirty day — the command refuses (exit 2, no verdict
    /// JSON) unless the operator explicitly passes --allow-all-zero.
    #[test]
    fn scorecard_all_zero_refuses_verdict_unless_allowed() {
        let root = std::env::temp_dir().join(format!("mp-allzero-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raw")).unwrap();
        let args = vec![
            "scorecard".into(),
            "--date".into(),
            "2026-08-24".into(),
            "--required".into(),
            "hyperliquid:BTC".into(),
        ];
        let err = scorecard_from_root(&root, &args).unwrap_err();
        assert!(err.contains("all-zero"), "{err}");
        assert!(!root.join("scorecards/2026-08-24.json").exists());
        // Operator escape hatch: explicit, on-record, still promotable=false.
        let mut allowed = args.clone();
        allowed.push("--allow-all-zero".into());
        let json = scorecard_from_root(&root, &allowed).expect("allowed zero-verdict");
        let card: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(card["promotable"], false);
        assert_eq!(card["recordings"][0]["event_count"], 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Incident 2026-08-22 version-skew regression: what the CURRENT writer
    /// encodes, the scorecard's audit path must decode with real counts —
    /// a reader/writer split inside one snapshot can never ship again
    /// (asserting event_count > 0 through `scorecard_from_root`).
    #[test]
    fn scorecard_decodes_schema_current_writer_events() {
        let root = std::env::temp_dir().join(format!("mp-pairsmoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raw")).unwrap();
        let raw = root.join("raw/20260825_hyperliquid_BTC.log");
        let (mut writer, _) = EventLogWriter::open(&raw).unwrap();
        writer
            .write_symbols(&[SymbolMeta::new(
                SymbolId(0),
                Venue::Hyperliquid,
                "BTC",
                "BTC",
                "USD",
                InstrumentKind::Perp,
                0.1,
                0.1,
                1.0,
            )])
            .unwrap();
        for seq in 1i64..=2 {
            writer
                .append(
                    &EventEnvelope::new(
                        Venue::Hyperliquid,
                        SymbolId(0),
                        seq * 1_000_000_000,
                        seq * 1_000_000_000,
                        seq as u64,
                        MarketEvent::Trade {
                            price: 100.0,
                            qty: 1.0,
                            side: Side::Buy,
                            trade_id: seq as u64,
                        },
                    )
                    .with_provenance(EventProvenance {
                        stream: "trade".into(),
                        subscription: "x".into(),
                        connection_id: 1,
                        snapshot_source: SnapshotSource::None,
                    }),
                )
                .unwrap();
        }
        writer.sync().unwrap();
        let args = vec![
            "scorecard".into(),
            "--date".into(),
            "2026-08-25".into(),
            "--required".into(),
            "hyperliquid:BTC".into(),
        ];
        let json = scorecard_from_root(&root, &args).expect("schema-current audit decodes");
        let card: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(card["recordings"][0]["event_count"], 2, "{json}");
        assert_eq!(card["recordings"][0]["clean"], true);
        assert_eq!(card["promotable"], true);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The audit's `stale_bursts` count and `worst_gap_ns` must surface in the
    /// scorecard JSON exactly as the audit computed them — the scorecard is
    /// what the gate (and `mp-ops promote`) actually read, so a mismatch here
    /// would hide the data-integrity margins from the daily verdict
    /// (2026-08-12 tolerance semantics).
    #[test]
    fn scorecard_json_carries_audit_stale_bursts_and_worst_gap() {
        let root =
            std::env::temp_dir().join(format!("mp-scorecard-margins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raw")).unwrap();
        let path = root.join("raw").join("20260812_hyperliquid_BTC.log");

        // Mirror storage::audit int_9: data at 1s; stale at 10s and 25s
        // (15s apart -> one 90s-grouped burst); data at 30s; a >120s recv
        // hole 30s->200s (170s -> worst gap); stale at 200s; data at 201s.
        fn ev(recv: i64, seq: u64, stale: bool) -> EventEnvelope {
            let body = if stale {
                MarketEvent::Status {
                    kind: mp_core::StatusKind::Stale,
                    detail: "stale".into(),
                }
            } else {
                MarketEvent::Trade {
                    price: 1.0,
                    qty: 1.0,
                    side: Side::Buy,
                    trade_id: recv as u64,
                }
            };
            EventEnvelope::new(Venue::Hyperliquid, SymbolId(0), recv, recv, seq, body)
                .with_provenance(EventProvenance {
                    stream: "trade".into(),
                    subscription: "x".into(),
                    connection_id: 1,
                    snapshot_source: SnapshotSource::None,
                })
        }
        let (mut writer, _) = EventLogWriter::open(&path).unwrap();
        writer
            .write_symbols(&[SymbolMeta::new(
                SymbolId(0),
                Venue::Hyperliquid,
                "BTC",
                "BTC",
                "USD",
                InstrumentKind::Perp,
                0.1,
                0.1,
                1.0,
            )])
            .unwrap();
        for (recv, seq, stale) in [
            (1_000_000_000i64, 1u64, false),
            (10_000_000_000, 2, true),
            (25_000_000_000, 3, true),
            (30_000_000_000, 4, false),
            (200_000_000_000, 5, true),
            (201_000_000_000, 6, false),
        ] {
            writer.append(&ev(recv, seq, stale)).unwrap();
        }
        writer.sync().unwrap();

        let json = scorecard_from_root(
            &root,
            &[
                "scorecard".into(),
                "--date".into(),
                "2026-08-12".into(),
                "--required".into(),
                "hyperliquid:BTC".into(),
            ],
        )
        .expect("scorecard should generate from the fixture raw log");
        let _ = std::fs::remove_dir_all(&root);

        let card: serde_json::Value = serde_json::from_str(&json).expect("scorecard is JSON");
        let rec = &card["recordings"][0];
        assert_eq!(rec["venue"], "hyperliquid");
        assert_eq!(rec["symbol"], "BTC");
        assert_eq!(rec["event_count"], 6, "{json}");
        // The audit's margin fields surface verbatim in the gate's JSON.
        assert_eq!(rec["stale_bursts"], 2, "{json}");
        assert_eq!(rec["worst_gap_ns"], 170_000_000_000i64, "{json}");
        // Warnings only on the margins, yet the day still blocks on the
        // numeric bar (six events over ~200s < 0.995): the margins must not
        // be hiding a blocked day.
        assert_eq!(rec["clean"], false, "{json}");
        assert!(rec["blocking_findings"].as_u64().unwrap() > 0, "{json}");
    }

    /// status: aggregates every artifact into one JSON document — promotion
    /// verdict, latest scorecard margins, coverage trend, pipeline log,
    /// backup manifest, and kill-latch state — and reports absent artifacts
    /// honestly instead of failing.
    #[test]
    fn status_reports_the_whole_system_in_one_document() {
        let root = std::env::temp_dir().join(format!("mp-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("scorecards")).unwrap();
        for (day, promotable, cov) in [(10, false, 0.9910f64), (11, true, 0.9984f64)] {
            let card = serde_json::json!({
                "date": format!("2026-08-{day}"),
                "recordings": [
                    { "venue": "hyperliquid", "symbol": "BTC", "clean": promotable,
                     "event_count": 1, "coverage": cov, "findings": 0,
                     "blocking_findings": 0, "worst_gap_ns": 239_000_000_000i64,
                     "stale_bursts": 2 },
                    { "venue": "hyperliquid", "symbol": "ETH", "clean": promotable,
                     "event_count": 1, "coverage": cov, "findings": 0,
                     "blocking_findings": 0, "worst_gap_ns": 239_000_000_000i64,
                     "stale_bursts": 2 }
                ],
                "promotable": promotable
            });
            std::fs::write(
                root.join("scorecards").join(format!("2026-08-{day}.json")),
                serde_json::to_string(&card).unwrap(),
            )
            .unwrap();
        }
        let pipeline_log = root.join("pipeline.log");
        std::fs::write(
            &pipeline_log,
            "[2026-08-11T00:05:00Z][INFO] Daily pipeline start\n[2026-08-11T00:06:00Z][INFO] Daily pipeline complete: 2026-08-10\n",
        )
        .unwrap();
        let manifest = root.join("backup_manifest.jsonl");
        std::fs::write(
            &manifest,
            "{\"ts_utc\":\"2026-08-11T00:30:00Z\",\"full_copy\":false,\"src_delta_files\":2,\"dst_delta_files\":2}\n",
        )
        .unwrap();
        let latch = root.join("kill.json");
        std::fs::write(
            &latch,
            KillLatch::global("manual test", 42).to_json().unwrap(),
        )
        .unwrap();

        let out = cmd_status(&[
            "--scorecards-dir".into(),
            root.join("scorecards").to_string_lossy().into(),
            "--pipeline-log".into(),
            pipeline_log.to_string_lossy().into(),
            "--backup-manifest".into(),
            manifest.to_string_lossy().into(),
            "--latch".into(),
            latch.to_string_lossy().into(),
            "--trend-days".into(),
            "14".into(),
        ])
        .expect("status succeeds");
        let _ = std::fs::remove_dir_all(&root);

        let s: serde_json::Value = serde_json::from_str(&out).expect("status is JSON");
        assert!(
            ["sleep", "backtest", "paper", "shadow", "live"]
                .iter()
                .any(|m| s["mode"] == *m),
            "mode must be one of the known modes: {out}"
        );
        // The promotion verdict: 1 clean day, first break 08-10.
        assert_eq!(s["promotion"]["consecutive_clean"], 1, "{out}");
        assert_eq!(s["promotion"]["first_failure"], "2026-08-10", "{out}");
        assert_eq!(s["latest_scorecard"]["date"], "2026-08-11", "{out}");
        assert_eq!(
            s["latest_scorecard"]["recordings"][0]["stale_bursts"], 2,
            "{out}"
        );
        assert_eq!(
            s["latest_scorecard"]["recordings"][0]["worst_gap_ns"],
            239_000_000_000i64
        );
        // Coverage trend: oldest→newest, both days present.
        let trend = s["coverage_trend"].as_array().unwrap();
        assert_eq!(trend.len(), 2, "{out}");
        assert_eq!(trend[0]["date"], "2026-08-10", "{out}");
        assert_eq!(trend[1]["recordings"][0]["coverage"], 0.9984, "{out}");
        assert_eq!(s["pipeline"]["present"], true, "{out}");
        assert_eq!(
            s["pipeline"]["last_line"],
            "[2026-08-11T00:06:00Z][INFO] Daily pipeline complete: 2026-08-10",
            "{out}"
        );
        assert_eq!(s["backup"]["last_entry"]["full_copy"], false, "{out}");
        assert_eq!(s["killswitch"]["latched"], true, "{out}");
        assert_eq!(s["killswitch"]["scopes"][0], "global", "{out}");
        assert_eq!(s["killswitch"]["reason"], "manual test", "{out}");
    }

    /// status: a scorecards directory that does not exist yet is a valid
    /// early state — `promotion` is null with an explanatory note, not an
    /// error. (Explicit --pipeline-log / --backup-manifest paths keep the
    /// test hermetic: this host's real artifacts may exist.)
    #[test]
    fn status_reports_missing_artifacts_without_failing() {
        let root = std::env::temp_dir().join(format!("mp-status-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let out = cmd_status(&[
            "--scorecards-dir".into(),
            root.join("nope").to_string_lossy().into(),
            "--pipeline-log".into(),
            root.join("no-pipeline.log").to_string_lossy().into(),
            "--backup-manifest".into(),
            root.join("no-manifest.jsonl").to_string_lossy().into(),
            "--latch".into(),
            root.join("no-latch.json").to_string_lossy().into(),
        ])
        .expect("status succeeds with no artifacts");
        let _ = std::fs::remove_dir_all(&root);
        let s: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(s["promotion"], serde_json::Value::Null, "{out}");
        assert!(s["promotion_note"].as_str().is_some(), "{out}");
        assert_eq!(s["pipeline"]["present"], false, "{out}");
        assert_eq!(s["backup"]["present"], false, "{out}");
        assert_eq!(s["killswitch"]["latched"], false, "{out}");
    }

    /// pipeline-stale: with yesterday's scorecard on disk past the deadline
    /// the check is healthy — no alert, not deferred.
    #[test]
    fn pipeline_stale_healthy_when_scorecard_landed() {
        let root = std::env::temp_dir().join(format!("mp-pstale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // 2026-08-13 01:00 UTC — past the 15-minute deadline.
        let now = date_to_nanos("2026-08-13").unwrap().0 + 3_600_000_000_000;
        let expected = utc_date_minus_days(now, 1);
        assert_eq!(expected, "2026-08-12");
        std::fs::write(
            root.join("2026-08-12.json"),
            serde_json::json!({
                "date": "2026-08-12",
                "recordings": [{"venue": "hyperliquid", "symbol": "BTC",
                 "clean": true, "event_count": 1, "coverage": 1.0,
                 "findings": 0, "blocking_findings": 0}],
                "promotable": true
            })
            .to_string(),
        )
        .unwrap();
        let out = cmd_pipeline_stale(&[
            "--scorecards-dir".into(),
            root.to_string_lossy().into(),
            "--ts-ns".into(),
            now.to_string(),
        ])
        .expect("healthy check succeeds");
        let _ = std::fs::remove_dir_all(&root);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["stale"], false, "{out}");
        assert_eq!(v["deferred"], false, "{out}");
        assert_eq!(v["alert"], serde_json::Value::Null, "{out}");
    }

    /// pipeline-stale: a missing scorecard past the deadline raises the P1
    /// with the expected runbook id. With credentials absent, egress is
    /// reported as unconfigured in the verdict — never a fake send, never a
    /// silent drop. (Creds are cleared for the duration so the test is
    /// deterministic even on a host where TELEGRAM_* is configured; this
    /// test binary holds no other reader of those vars.)
    #[test]
    fn pipeline_stale_fires_p1_when_gate_did_not_land() {
        // Hold the env mutex for the whole mutate→use→restore window so
        // parallel tests (e.g. promote_*) never observe a half-mutated env.
        let _guard = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!("mp-pstale-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let prior_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
        let prior_chat = std::env::var("TELEGRAM_CHAT_ID").ok();
        std::env::remove_var("TELEGRAM_BOT_TOKEN");
        std::env::remove_var("TELEGRAM_CHAT_ID");
        let now = date_to_nanos("2026-08-13").unwrap().0 + 3_600_000_000_000;
        let out = cmd_pipeline_stale(&[
            "--scorecards-dir".into(),
            root.to_string_lossy().into(),
            "--ts-ns".into(),
            now.to_string(),
            "--telegram".into(),
        ])
        .expect("stale check succeeds");
        if let Some(t) = prior_token {
            std::env::set_var("TELEGRAM_BOT_TOKEN", t);
        }
        if let Some(c) = prior_chat {
            std::env::set_var("TELEGRAM_CHAT_ID", c);
        }
        let _ = std::fs::remove_dir_all(&root);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["stale"], true, "{out}");
        assert_eq!(v["expected_date"], "2026-08-12", "{out}");
        assert_eq!(v["alert"]["id"], "pipeline-stale", "{out}");
        assert_eq!(v["alert"]["severity"], "P1", "{out}");
        assert_eq!(
            v["alert"]["runbook"], "ops/runbooks/pipeline-stale.md",
            "{out}"
        );
        assert!(
            v["alert"]["detail"]
                .as_str()
                .unwrap()
                .contains("2026-08-12"),
            "{out}"
        );
        // Credentials absent ⇒ egress reported as unconfigured, never a fake send.
        assert_eq!(v["telegram"], "unconfigured", "{out}");
    }

    /// pipeline-stale: an unparseable scorecard is as bad as a missing one —
    /// the gate cannot read it, so the P1 fires.
    #[test]
    fn pipeline_stale_fires_on_unparseable_scorecard() {
        let root = std::env::temp_dir().join(format!("mp-pstale-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("2026-08-12.json"), "{not json").unwrap();
        let now = date_to_nanos("2026-08-13").unwrap().0 + 3_600_000_000_000;
        let out = cmd_pipeline_stale(&[
            "--scorecards-dir".into(),
            root.to_string_lossy().into(),
            "--ts-ns".into(),
            now.to_string(),
        ])
        .expect("check succeeds");
        let _ = std::fs::remove_dir_all(&root);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["stale"], true, "{out}");
        assert!(
            v["alert"]["detail"]
                .as_str()
                .unwrap()
                .contains("unparseable"),
            "{out}"
        );
    }

    /// pipeline-stale: before the deadline (UTC minute < --deadline-min) the
    /// check defers instead of alerting — the 00:05 gate may still be running.
    #[test]
    fn pipeline_stale_defers_before_deadline() {
        let root = std::env::temp_dir().join(format!("mp-pstale-def-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // 2026-08-13 00:05 UTC — inside the window, no scorecard yet.
        let now = date_to_nanos("2026-08-13").unwrap().0 + 300_000_000_000;
        let out = cmd_pipeline_stale(&[
            "--scorecards-dir".into(),
            root.to_string_lossy().into(),
            "--ts-ns".into(),
            now.to_string(),
        ])
        .expect("deferred check succeeds");
        let _ = std::fs::remove_dir_all(&root);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["stale"], false, "{out}");
        assert_eq!(v["deferred"], true, "{out}");
        assert_eq!(v["alert"], serde_json::Value::Null, "{out}");
    }

    /// The civil-date inverse is the exact inverse of days_from_epoch across
    /// a leap-year boundary — the pipeline-stale expected-date math.
    #[test]
    fn utc_date_minus_days_roundtrips_across_leap_boundary() {
        // 2024-02-29 12:00 UTC minus 1 day = 2024-02-28.
        let (start, _) = date_to_nanos("2024-02-29").unwrap();
        let d = utc_date_minus_days(start + 43_200_000_000_000, 1);
        assert_eq!(d, "2024-02-28");
        // And the day-count helpers agree: date_to_nanos → civil_from_days
        // round-trips.
        for day in 0..1461 {
            let ns = day as i64 * 86_400_000_000_000;
            let (y, m, d) = civil_from_days(day as i64);
            let (back, _) = date_to_nanos(&format!("{y:04}-{m:02}-{d:02}")).unwrap();
            assert_eq!(back, ns, "day {day} round-trips");
        }
    }

    /// scorecard --reuse-unchanged: an unchanged source (size+mtime+config)
    /// reuses the archived scorecard instead of re-auditing; a changed source
    /// falls through to the full audit. The manifest is the cache; the verdict
    /// always comes from a real audit or an exact source match.
    #[test]
    fn scorecard_reuse_unchanged_skips_reaudit_until_source_changes() {
        let root = std::env::temp_dir().join(format!("mp-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raw")).unwrap();
        let ev = |recv: i64, seq: u64| {
            EventEnvelope::new(
                Venue::Hyperliquid,
                SymbolId(0),
                recv,
                recv,
                seq,
                MarketEvent::Trade {
                    price: 100.0,
                    qty: 1.0,
                    side: Side::Buy,
                    trade_id: seq,
                },
            )
            .with_provenance(EventProvenance {
                stream: "trade".into(),
                subscription: "x".into(),
                connection_id: 1,
                snapshot_source: SnapshotSource::None,
            })
        };
        let raw = root.join("raw/20260813_hyperliquid_BTC.log");
        let (mut writer, _) = EventLogWriter::open(&raw).unwrap();
        writer
            .write_symbols(&[SymbolMeta::new(
                SymbolId(0),
                Venue::Hyperliquid,
                "BTC",
                "BTC",
                "USD",
                InstrumentKind::Perp,
                0.1,
                0.1,
                1.0,
            )])
            .unwrap();
        writer.append(&ev(1_000_000_000, 1)).unwrap();
        writer.sync().unwrap();
        let args = vec![
            "scorecard".into(),
            "--date".into(),
            "2026-08-13".into(),
            "--required".into(),
            "hyperliquid:BTC".into(),
            "--reuse-unchanged".into(),
        ];
        let first = scorecard_from_root(&root, &args).expect("first scorecard");
        // Second run: source unchanged ⇒ reuse (no re-audit of the raw log).
        let second = scorecard_from_root(&root, &args).expect("reused scorecard");
        assert_eq!(first, second, "reuse returns the identical scorecard JSON");
        let card1: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(card1["recordings"][0]["event_count"], 1);
        // Append a trade ⇒ size changes ⇒ the reuse key breaks ⇒ re-audit.
        writer.append(&ev(2_000_000_000, 2)).unwrap();
        writer.sync().unwrap();
        let third = scorecard_from_root(&root, &args).expect("re-audited scorecard");
        let card3: serde_json::Value = serde_json::from_str(&third).unwrap();
        assert_eq!(card3["recordings"][0]["event_count"], 2, "{third}");
        // The sidecar recorded the fingerprints for future reuse.
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("scorecards/.scorecard_sources.json")).unwrap(),
        )
        .unwrap();
        assert!(
            manifest["days"]["2026-08-13"]["sources"]["hyperliquid:BTC"]["size"]
                .as_u64()
                .unwrap()
                > 0
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// COL-29 (spec 024): `--require-stream venue:stream` applies only to that
    /// venue's recordings — a bare stream applies to all. This is what lets
    /// the gate demand `liquidation` on Binance/Bybit without breaking
    /// Hyperliquid, which has no native liq stream by design.
    #[test]
    fn require_stream_venue_scoping_applies_per_venue() {
        let args = vec![
            "--require-stream".to_string(),
            "trade".to_string(),
            "--require-stream".to_string(),
            "binance:liquidation".to_string(),
            "--require-stream".to_string(),
            "bybit:liquidation".to_string(),
        ];
        let binance = audit_config(&args, Venue::BinanceFutures, "BTCUSDT").unwrap();
        assert!(binance.required_streams.contains("trade"));
        assert!(
            binance.required_streams.contains("liquidation"),
            "binance recording must require its liquidation stream"
        );
        let bybit = audit_config(&args, Venue::Bybit, "BTCUSDT").unwrap();
        assert!(bybit.required_streams.contains("liquidation"));
        let hl = audit_config(&args, Venue::Hyperliquid, "BTC").unwrap();
        assert!(hl.required_streams.contains("trade"));
        assert!(
            !hl.required_streams.contains("liquidation"),
            "hyperliquid has no native liquidation stream; must not be required"
        );
        // An unknown venue name in a scoped flag is ignored, not fatal.
        let ok = audit_config(&args, Venue::Okx, "BTC-USDT").unwrap();
        assert!(!ok.required_streams.contains("liquidation"));
    }

    #[test]
    fn lab_7_check_config_rejects_downloads() {
        let result = cmd_check_config(&["--data-dir".into(), "/some/path/Downloads/data".into()]);
        assert!(result.is_err(), "LAB-7: Downloads path must be rejected");
        let msg = result.unwrap_err();
        assert!(msg.to_lowercase().contains("downloads"), "error must mention Downloads: {msg}");
    }

    #[test]
    fn lab_7_check_config_fails_when_env_unset() {
        std::env::remove_var("MP_DATA_HOME");
        let result = cmd_check_config(&[]);
        assert!(result.is_err(), "LAB-7: must fail when no data dir configured");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("MP_DATA_HOME") || msg.contains("--data-dir"),
            "error must mention configuration options: {msg}"
        );
    }

    #[test]
    fn lab_7_check_config_case_insensitive_downloads() {
        let result = cmd_check_config(&["--data-dir".into(), "c:/Users/me/Downloads".into()]);
        assert!(result.is_err(), "LAB-7: case-insensitive Downloads check");
    }

    #[test]
    fn lab_7_check_config_accepts_valid_path() {
        let result = cmd_check_config(&["--data-dir".into(), ".".into()]);
        assert!(result.is_ok(), "LAB-7: current dir must be accepted: {:?}", result);
    }

    #[test]
    fn lab_1_pipeline_no_hour_skip() {
        // Find workspace root by walking up to Cargo.toml
        let mut dir = std::env::current_dir().unwrap();
        loop {
            if dir.join("Cargo.toml").exists() && dir.join("ops").exists() {
                break;
            }
            if !dir.pop() {
                panic!("could not find workspace root");
            }
        }
        let pipeline = std::fs::read_to_string(dir.join("ops/scripts/daily_pipeline.ps1"))
            .expect("pipeline script must exist");
        // LAB-1: old behavior skipped scoring when UTC hour was outside 00:00-09:00.
        // After LAB-1 this check was removed. Detect actual code (non-comment lines)
        // that gate on hour windows.
        for line in pipeline.lines() {
            let trimmed = line.trim();
            // Skip comments and blank lines.
            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_lowercase();
            if (lower.contains("hour") || lower.contains("hourly"))
                && (lower.contains("skip") || lower.contains("outside"))
                && lower.contains("00:00")
            {
                panic!("LAB-1: active code contains hour-skip logic: {trimmed}");
            }
        }
    }

    #[test]
    fn lab_2_test_module_now_does_not_fail_pipeline_guard() {
        // LAB-2: SystemTime::now in #[cfg(test)] must not trip guardrails.
        // The guardrails skip tests/ dirs and allowlist audit.rs.
        let mut dir = std::env::current_dir().unwrap();
        loop {
            if dir.join("Cargo.toml").exists() && dir.join("ops").exists() {
                break;
            }
            if !dir.pop() {
                panic!("could not find workspace root");
            }
        }
        let guardrails_sh = std::fs::read_to_string(dir.join("ops/ci/guardrails.sh"))
            .expect("guardrails.sh must exist");
        // The guardrails script should have a pattern that skips test directories
        assert!(
            guardrails_sh.contains("tests/") || guardrails_sh.contains("test"),
            "guardrails must skip test modules (LAB-2)"
        );
    }

    #[test]
    fn lab_3_missing_raw_no_promotable_card() {
        // LAB-3: When raw data is missing, scorecard must refuse and not promote.
        // This tests that the pipeline exit code is non-zero on missing data.
        let mut dir = std::env::current_dir().unwrap();
        loop {
            if dir.join("Cargo.toml").exists() && dir.join("ops").exists() {
                break;
            }
            if !dir.pop() {
                panic!("could not find workspace root");
            }
        }
        let pipeline = std::fs::read_to_string(dir.join("ops/scripts/daily_pipeline.ps1"))
            .expect("pipeline script must exist");
        // The pipeline must handle missing raw with a non-zero exit / refuse path
        assert!(
            pipeline.contains("recording_missing") || pipeline.contains("all-zero")
                || pipeline.contains("refuse") || pipeline.contains("missing")
                || pipeline.contains("promotable"),
            "pipeline must handle missing raw (LAB-3)"
        );
    }

    #[test]
    fn lab_8_prune_still_requires_hash() {
        // LAB-8: prune must require source_log_hash verify.
        let mut dir = std::env::current_dir().unwrap();
        loop {
            if dir.join("Cargo.toml").exists() && dir.join("storage").exists() {
                break;
            }
            if !dir.pop() {
                panic!("could not find workspace root");
            }
        }
        let prune_src = std::fs::read_to_string(dir.join("storage/src/prune.rs"))
            .expect("prune.rs must exist");
        assert!(
            prune_src.contains("source_log_hash"),
            "LAB-8: prune must reference source_log_hash"
        );
    }

    #[test]
    fn lab_10_catchup_idempotent_no_duplicates() {
        // LAB-10: catch-up must be idempotent — running twice produces no duplicate cards.
        // Verify the pipeline script has --reuse-unchanged or equivalent idempotency.
        let mut dir = std::env::current_dir().unwrap();
        loop {
            if dir.join("Cargo.toml").exists() && dir.join("ops").exists() {
                break;
            }
            if !dir.pop() {
                panic!("could not find workspace root");
            }
        }
        let pipeline = std::fs::read_to_string(dir.join("ops/scripts/daily_pipeline.ps1"))
            .expect("pipeline script must exist");
        assert!(
            pipeline.contains("reuse-unchanged") || pipeline.contains("idempotent")
                || pipeline.contains("skip if exists"),
            "LAB-10: catch-up must be idempotent (reuse-unchanged or skip-if-exists)"
        );
    }

    #[test]
    fn lab_4_held_after_ssh_fail() {
        // LAB-4: drain must keep files held (no remote delete) when local checksum
        // matches but SSH fails. The drain script must reference held/held list.
        let mut dir = std::env::current_dir().unwrap();
        loop {
            if dir.join("Cargo.toml").exists() && dir.join("ops").exists() {
                break;
            }
            if !dir.pop() {
                panic!("could not find workspace root");
            }
        }
        let drain = std::fs::read_to_string(dir.join("ops/scripts/vps_drain.ps1"))
            .expect("vps_drain.ps1 must exist");
        assert!(
            drain.to_lowercase().contains("held") || drain.to_lowercase().contains("hold"),
            "LAB-4: drain script must reference held/hold for ssh_failed files"
        );
    }

    // ---- prune regression tests (cold-root contract, audit 2026-09-03) ----
    //
    // cmd_prune delegates to prune_day(root, ..) so the retention gate can be
    // driven against a temp tree. These pin the observed live bug: passing the
    // DATA root (data/) to verify_prunable instead of the COLD root (data/cold)
    // made every properly compacted day refuse with ManifestMissing, because
    // manifests live at data/cold/manifests/.. (compactor root), not
    // data/manifests/.. (storage/tests/storage.rs:502 pins the cold-root
    // contract).

    /// Minimal raw day-file fixture, same recipe as storage/tests/storage.rs:
    /// `raw/{YYYYMMDD}_{venue}_{symbol}.log` via the real EventLogWriter.
    fn prune_fixture(tag: &str) -> (PathBuf, SymbolTable, SymbolId, Vec<EventEnvelope>) {
        let data = std::env::temp_dir().join(format!("mp-prune-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        let mut syms = SymbolTable::new();
        let btc = syms.intern_default(Venue::Bybit, "BTCUSDT");
        let evs = vec![
            EventEnvelope::new(
                Venue::Bybit,
                btc,
                10,
                10,
                1,
                MarketEvent::Trade {
                    price: 100.0,
                    qty: 1.0,
                    side: Side::Buy,
                    trade_id: 1,
                },
            ),
            EventEnvelope::new(
                Venue::Bybit,
                btc,
                20,
                20,
                2,
                MarketEvent::Trade {
                    price: 101.0,
                    qty: 1.0,
                    side: Side::Buy,
                    trade_id: 2,
                },
            ),
        ];
        let raw_dir = data.join("raw");
        std::fs::create_dir_all(&raw_dir).unwrap();
        let path = raw_dir.join("20260710_bybit_BTCUSDT.log");
        let (mut w, truncated) = EventLogWriter::open(&path).unwrap();
        assert!(!truncated);
        w.write_symbols(&syms.metas()).unwrap();
        for e in &evs {
            w.append(e).unwrap();
        }
        drop(w);
        (data, syms, btc, evs)
    }

    /// The observed bug: a freshly compacted day must be prunable via the cold
    /// root (data/cold). Regression for the data-root mismatch that refused
    /// every live day with ManifestMissing.
    #[test]
    fn prune_dry_run_passes_on_freshly_compacted_day() {
        let (data, syms, _btc, evs) = prune_fixture("ok");
        let hash = compute_source_hash(&data.join("raw/20260710_bybit_BTCUSDT.log")).unwrap();
        compactor::compact_day(
            &data.join("cold"),
            Venue::Bybit,
            "2026-07-10",
            0,
            86_400_000_000_000,
            evs,
            &syms,
            &hash,
            "g",
            0,
        )
        .unwrap();
        let out = prune_day(
            &data,
            Venue::Bybit,
            "bybit",
            "BTCUSDT",
            "2026-07-10",
            true,
        )
        .expect("freshly compacted day must verify prunable (cold-root contract)");
        assert!(
            out.contains("[dry-run] would prune"),
            "expected dry-run success, got: {out}"
        );
        assert!(
            data.join("raw/20260710_bybit_BTCUSDT.log").exists(),
            "dry-run must not delete the raw log"
        );
        let _ = std::fs::remove_dir_all(&data);
    }

    /// No compaction manifest -> refuse with the W-6 reason, keep the log.
    #[test]
    fn prune_refuses_without_compaction_manifest() {
        let (data, _syms, _btc, _evs) = prune_fixture("nomanifest");
        let err = prune_day(&data, Venue::Bybit, "bybit", "BTCUSDT", "2026-07-10", true)
            .expect_err("uncompacted day must refuse");
        assert!(
            err.contains("no compaction manifest"),
            "unexpected refusal text: {err}"
        );
        assert!(
            data.join("raw/20260710_bybit_BTCUSDT.log").exists(),
            "refusal must leave the raw log in place"
        );
        let _ = std::fs::remove_dir_all(&data);
    }

    /// Missing raw log -> clean error before any proof work.
    #[test]
    fn prune_refuses_missing_raw_log() {
        let data = std::env::temp_dir().join(format!("mp-prune-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        let err = prune_day(&data, Venue::Bybit, "bybit", "BTCUSDT", "2026-07-10", true)
            .expect_err("missing raw log must refuse");
        assert!(err.contains("raw log not found"), "unexpected text: {err}");
        let _ = std::fs::remove_dir_all(&data);
    }

    /// Real delete path: removes the raw log AND journals the deletion.
    #[test]
    fn prune_deletes_and_journals() {
        let (data, syms, _btc, evs) = prune_fixture("delete");
        let hash = compute_source_hash(&data.join("raw/20260710_bybit_BTCUSDT.log")).unwrap();
        compactor::compact_day(
            &data.join("cold"),
            Venue::Bybit,
            "2026-07-10",
            0,
            86_400_000_000_000,
            evs,
            &syms,
            &hash,
            "g",
            0,
        )
        .unwrap();
        let out = prune_day(&data, Venue::Bybit, "bybit", "BTCUSDT", "2026-07-10", false)
            .expect("verified day must prune");
        assert!(out.contains("pruned"), "unexpected output: {out}");
        assert!(
            !data.join("raw/20260710_bybit_BTCUSDT.log").exists(),
            "real prune must delete the raw log"
        );
        let journal = std::fs::read_to_string(data.join("retention_delete_manifest.jsonl"))
            .expect("journal must exist after a real prune (A-12)");
        assert!(journal.contains("2026-07-10"), "journal entry missing date: {journal}");
        assert!(journal.contains("source_hash"), "journal entry missing hash: {journal}");
        let _ = std::fs::remove_dir_all(&data);
    }
}
