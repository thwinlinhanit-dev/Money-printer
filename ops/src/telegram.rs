//! Telegram Bot API edge for alerts (spec 009): the thin network side of the
//! alert framework. Decision logic — decay detection, dedupe, quiet-hours
//! batching — stays in the clock-injected, I/O-free core (alert.rs/report.rs);
//! this module is the "MP_OPS_P1_WEBHOOK-style edge": env-configured, off the
//! decision path, testable against a stubbed endpoint.
//!
//! TLS: the Bot API is https-only, and the ops crate deliberately carries no
//! TLS dependency (`post_p1_webhook` refuses https for the same reason), so
//! the send shells out to `curl` — the host's TLS stack. `MP_OPS_TELEGRAM_URL`
//! overrides the endpoint (tests point it at a local stub; opsd-style
//! deployments leave it at `https://api.telegram.org`).
//!
//! P3 semantics: the decay alert is P3, so every send uses
//! `disable_notification=true` (a quiet push — never a night-time buzz). The
//! quiet-hours *batching* itself is `AlertRouter`'s job: when a P3 routes to
//! `Batched`, the CLI persists the dispatch via [`append_batch`] and the
//! weekly wrapper flushes it at quiet-hours end via [`flush_batch`].

use crate::alert::{Alert, Dispatch, Severity};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One day in ns — the queue/delivery age the report's accountability rows
/// render (shared by the batch and delivery-log loaders).
const DAY_NS: u64 = 86_400_000_000_000;

/// One pending (un-flushed) quiet-hours P3 from the batch ledger, as the
/// monthly report's delivery-accountability section renders it (OPS-6): a
/// line in `journal/telegram/batch.jsonl` is an alert that was batched but
/// NOT yet delivered — anything still queued at month-end is a delivery gap
/// the report must surface, never silently drop (W-6).
#[derive(Debug, Clone)]
pub struct TelegramPendingRow {
    pub id: String,
    pub severity: Severity,
    pub detail: String,
    pub runbook: String,
    /// When the dispatch was queued (ns) — kept so the report could show the
    /// raw timestamp; the section renders the derived queue age instead.
    pub ts_ns: i64,
    /// Whole days the dispatch has been queued at the injected `now_ns`.
    pub queued_days: u64,
}

/// One flushed delivery from the Telegram delivery log, as the monthly
/// report's delivery-accountability section renders it (OPS-6): a line in
/// `journal/telegram/delivered.jsonl` is a P3 that WAS delivered — the
/// accountability counterpart to [`TelegramPendingRow`], proving the ledger
/// worked (W-6) instead of only showing what is still queued.
#[derive(Debug, Clone)]
pub struct TelegramDeliveredRow {
    pub id: String,
    /// When the dispatch was delivered (ns) — the flush's injected clock
    /// (PD-3), stamped on the record by [`flush_batch`].
    pub delivered_ts_ns: i64,
    /// Whole days since delivery at the injected `now_ns`.
    pub delivered_days_ago: u64,
}

/// One physical line of `delivered.jsonl` — the exact shape [`flush_batch`]
/// appends per successful send. Kept private: the public surface is
/// [`TelegramDeliveredRow`].
#[derive(Debug, Serialize, Deserialize)]
struct DeliveredRecord {
    id: String,
    delivered_ts_ns: i64,
}

/// Endpoint + credentials for one Telegram delivery. `url` is the Bot API
/// base (override for tests/stubs); token + chat_id come from the host's
/// `ops.env` (`TELEGRAM_BOT_TOKEN` / `TELEGRAM_CHAT_ID`, PD-2: never committed).
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub url: String,
    pub token: String,
    pub chat_id: String,
}

/// POST one dispatch to the Bot API via curl and require a Telegram `ok:true`.
/// Returns an error on any failure — a sink that silently "accepted" a 400
/// would be worse than the honest error (fail-closed, same posture as
/// `post_p1_webhook`). `disable_notification=true`: this edge only carries P3s.
pub fn post_telegram(dispatch: &Dispatch, cfg: &TelegramConfig) -> Result<(), String> {
    let url = format!(
        "{}/bot{}/sendMessage",
        cfg.url.trim_end_matches('/'),
        cfg.token
    );
    let text = format!(
        "{} ({})\n{}\nRunbook: {}",
        dispatch.id,
        dispatch.severity.as_str(),
        dispatch.detail,
        dispatch.runbook
    );
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sS", "-m", "30", "-X", "POST"]);
    cmd.arg(&url);
    cmd.arg("-d").arg(format!("chat_id={}", cfg.chat_id));
    cmd.arg("--data-urlencode").arg(format!("text={text}"));
    cmd.arg("-d").arg("disable_notification=true");
    let out = cmd
        .output()
        .map_err(|e| format!("curl spawn failed (is curl installed?): {e}"))?;
    if !out.status.success() {
        // Audit 2026-08-17 (PD-2, same posture as `post_p1_webhook`): do NOT
        // echo curl's stderr - on transport errors it can embed the
        // token-bearing Bot API URL. Report only the exit code.
        return Err(format!(
            "curl exited {} (stderr suppressed - may contain the bot URL)",
            out.status
        ));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    if !body.contains("\"ok\":true") {
        // The Bot API never echoes the request URL (the only place the token
        // lives) in its response body, so a truncated preview is
        // credential-safe (PD-2); transport stderr above is suppressed for
        // the same reason.
        let preview: String = body.chars().take(300).collect();
        return Err(format!("telegram api non-ok: {preview}"));
    }
    Ok(())
}

/// Path of the P3 quiet-hours batch ledger for a directory (default
/// `journal/telegram`). Append-only JSONL of `Dispatch` records (W-6).
pub fn batch_path(dir: &Path) -> PathBuf {
    dir.join("batch.jsonl")
}

/// Path of the Telegram delivery log for a directory — `delivered.jsonl`,
/// the counterpart of [`batch_path`]. Append-only JSONL of flushed records
/// (`{id, delivered_ts_ns}`), written by [`flush_batch`] on every successful
/// delivery (W-6): what WAS delivered, as opposed to what is still queued.
pub fn delivered_path(dir: &Path) -> PathBuf {
    dir.join("delivered.jsonl")
}

/// Load the quiet-hours batch ledger (`<dir>/batch.jsonl`, append-only W-6)
/// as pending deliveries for the monthly report's delivery-accountability
/// section. Fail-closed (CONV-8): every non-empty line must parse as a
/// `Dispatch` — the exact shape [`append_batch`] writes — and a corrupt line
/// names the line and fails the whole load: a queued alert is evidence, never
/// silently dropped. A missing file is the healthy "nothing pending" state
/// (every batch was flushed), not an error. Rows are sorted by queue time
/// (oldest first, CONV-10). `now_ns` is injected (PD-3) so queue age is
/// deterministic in tests.
pub fn load_telegram_batch(dir: &Path, now_ns: i64) -> Result<Vec<TelegramPendingRow>, String> {
    let path = batch_path(dir);
    let content = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let mut rows = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let dispatch: Dispatch = serde_json::from_str(line)
            .map_err(|e| format!("{} line {}: {e}", path.display(), idx + 1))?;
        rows.push(TelegramPendingRow {
            queued_days: now_ns.saturating_sub(dispatch.ts_ns) as u64 / DAY_NS,
            id: dispatch.id,
            severity: dispatch.severity,
            detail: dispatch.detail,
            runbook: dispatch.runbook,
            ts_ns: dispatch.ts_ns,
        });
    }
    // Deterministic queue order: oldest queued first (CONV-10).
    rows.sort_by_key(|r| r.ts_ns);
    Ok(rows)
}

/// Persist one batched dispatch (a P3 held during quiet hours). Append-only;
/// fsynced before returning so a crash cannot lose a queued alert (W-6/OPS-12
/// spirit — the batch is the delivery ledger until flushed).
pub fn append_batch(dir: &Path, dispatch: &Dispatch) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = batch_path(dir);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    // JSONL: exactly one record per physical line (append-only W-6) — without
    // the trailing newline a second dispatch would concatenate onto the first
    // line and corrupt the ledger for every later reader.
    let mut line = serde_json::to_string(dispatch).map_err(|e| e.to_string())?;
    line.push('\n');
    f.write_all(line.as_bytes())
        .map_err(|e| format!("append {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("sync {}: {e}", path.display()))?;
    Ok(())
}

/// Near-real-time staleness check on the quiet-hours batch ledger (OPS-9/
/// OPS-14): raises `telegram-stale` (P2) when a dispatch has been queued
/// longer than one full quiet window (`threshold_ns`, default 24h). The
/// monthly report only flags a stuck queue at month-end — a MISSED
/// `telegram-flush` must alert in near-real-time, which is what the hourly
/// `telegram-stale` check runs. Pure and clock-injected (PD-3): the rows
/// carry their queue time, `now_ns` decides the age.
///
/// The OLDEST stale dispatch wins (the worst offender) and the alert's dedupe
/// key is that dispatch's id (regression_audit3 pattern) so one stuck alert
/// never suppresses another; a stateful consumer re-alerts each entity at
/// most once per `dedupe_ns`.
pub fn stale_batch_alert(
    rows: &[TelegramPendingRow],
    now_ns: i64,
    threshold_ns: i64,
    dedupe_ns: i64,
) -> Option<Alert> {
    let stale = rows
        .iter()
        .filter(|r| now_ns.saturating_sub(r.ts_ns) >= threshold_ns)
        .collect::<Vec<_>>();
    let worst = stale.iter().min_by_key(|r| r.ts_ns)?;
    // Rounded UP so the named age never understates the "≥ {threshold}h"
    // trigger (a 24h59m-old dispatch reads "25h", not "24h").
    let age_ns = now_ns.saturating_sub(worst.ts_ns);
    let queued_h = (age_ns + 3_600_000_000_000 - 1) / 3_600_000_000_000;
    let threshold_h = threshold_ns / 3_600_000_000_000;
    Some(
        Alert::new(
            "telegram-stale",
            Severity::P2,
            dedupe_ns,
            format!(
                "{} dispatch(es) queued ≥ {threshold_h}h (missed flush); oldest '{}' queued {queued_h}h — run `mp-ops telegram-flush --wait`",
                stale.len(),
                worst.id
            ),
        )
        .with_dedupe_key(worst.id.clone()),
    )
}

/// Persist one flushed delivery (a P3 that reached Telegram) into the
/// delivery log: alert id + when it was delivered. Append-only, fsynced
/// before returning (W-6 — a delivery is evidence, never lost). The exact
/// shape [`load_telegram_delivered`] reads for the monthly report.
pub fn append_delivered(dir: &Path, id: &str, delivered_ts_ns: i64) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = delivered_path(dir);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut line = serde_json::to_string(&DeliveredRecord {
        id: id.to_string(),
        delivered_ts_ns,
    })
    .map_err(|e| e.to_string())?;
    line.push('\n');
    f.write_all(line.as_bytes())
        .map_err(|e| format!("append {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("sync {}: {e}", path.display()))?;
    Ok(())
}

/// Load the Telegram delivery log (`<dir>/delivered.jsonl`, append-only W-6)
/// — the flushed records: what WAS delivered (alert id + when), the
/// accountability counterpart to the pending batch ledger. Fail-closed
/// (CONV-8): every non-empty line must parse as a flushed record with a
/// non-empty `id`, and a corrupt line names the line and fails the whole
/// load — a delivery is evidence, never silently dropped. A missing log is
/// the healthy "no deliveries recorded" state (the flush has not run or had
/// nothing to send), not an error. Rows are sorted by delivery time (oldest
/// first, CONV-10). `now_ns` is injected (PD-3) so delivery age is
/// deterministic in tests.
pub fn load_telegram_delivered(
    dir: &Path,
    now_ns: i64,
) -> Result<Vec<TelegramDeliveredRow>, String> {
    let path = delivered_path(dir);
    let content = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let mut rows = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let lineno = idx + 1;
        let rec: DeliveredRecord = serde_json::from_str(line)
            .map_err(|e| format!("{} line {lineno}: {e}", path.display()))?;
        if rec.id.is_empty() {
            return Err(format!("{} line {lineno}: empty 'id'", path.display()));
        }
        rows.push(TelegramDeliveredRow {
            id: rec.id,
            delivered_ts_ns: rec.delivered_ts_ns,
            delivered_days_ago: now_ns.saturating_sub(rec.delivered_ts_ns) as u64 / DAY_NS,
        });
    }
    // Deterministic log order: earliest delivery first (CONV-10).
    rows.sort_by_key(|r| r.delivered_ts_ns);
    Ok(rows)
}
/// Send every batched dispatch, log each successful delivery to
/// `delivered.jsonl`, and remove the batch ledger on full success. Fail-closed
/// (CONV-8): a send failure or corrupt line aborts with the FAILED line and
/// everything after it still in the ledger — lines already delivered are
/// dropped, so a retry never duplicates them (at-most-once per attempt; the
/// operator sees a non-zero exit, nothing is silently lost). Every successful
/// send is recorded in the delivery log FIRST (append-only, fsynced, W-6): a
/// delivery that reached Telegram is evidence — the report's delivery log
/// — and is never lost with the batch ledger; a FAILED dispatch is never
/// logged as delivered (it stays pending in the batch). Returns the number of
/// dispatches flushed. `now_ns` is the injected delivery timestamp (PD-3)
/// stamped on the flushed records. The log append and the batch rewrite are
/// not atomic: a crash between them leaves the delivered line in BOTH — a
/// retry re-sends it (the same duplicate-FYI posture as a corrupt-line
/// retry), never silently drops it.
pub fn flush_batch(dir: &Path, cfg: &TelegramConfig, now_ns: i64) -> Result<usize, String> {
    let path = batch_path(dir);
    if !path.exists() {
        return Ok(0);
    }
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut remaining: Vec<&str> = content.lines().collect();
    let mut flushed = 0usize;
    while let Some(line) = remaining.first() {
        // A corrupt line aborts WITHOUT rewriting (unlike a send failure):
        // the ledger is evidence needing human inspection, and the lines
        // already flushed stay in it — a retry after the human fixes the
        // corrupt line would re-send them, which is fine for a P3 FYI batch
        // (better a duplicate FYI than a silently dropped ledger line).
        let dispatch: Dispatch = serde_json::from_str(line)
            .map_err(|e| format!("batch line {} corrupt: {e}", flushed + 1))?;
        if let Err(e) = post_telegram(&dispatch, cfg) {
            // Preserve the failed line and everything after it; the lines
            // already flushed are gone (no duplicates on the next attempt).
            let rest = remaining.join("\n") + "\n";
            // Audit 2026-08-17: this ledger rewrite must be as durable as the
            // delivered.jsonl append that preceded it (W-6 discipline) - a
            // crash right after an unsynced write could resurrect delivered
            // lines for a duplicate re-send. Open + write + sync_all, matching
            // append_batch/append_delivered.
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&path)
                .map_err(|e| format!("rewrite open {}: {e}", path.display()))?;
            f.write_all(rest.as_bytes())
                .map_err(|e| format!("rewrite {}: {e}", path.display()))?;
            f.sync_all()
                .map_err(|e| format!("rewrite sync {}: {e}", path.display()))?;
            return Err(format!("batch line {}: {e}", flushed + 1));
        }
        // Delivery log first: a delivered P3 is evidence (W-6), written
        // before it leaves the batch. An unwritable log fails the flush with
        // the line still in the batch — a retry re-sends (better a duplicate
        // FYI than a silently lost delivery record).
        append_delivered(dir, &dispatch.id, now_ns)?;
        flushed += 1;
        remaining.remove(0);
    }
    std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    Ok(flushed)
}
