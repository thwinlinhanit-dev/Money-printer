//! Alert framework (OPS-4, OPS-9): every alert carries an id, severity, dedupe
//! window, and runbook link. Routing is deterministic — the clock is injected
//! as `now_ns` (PD-3), never read here. Quiet hours batch P3s; P1/P2 always
//! break through.

use std::collections::BTreeMap;

/// Severity drives the channel and the on-call expectation (spec 009 table).
/// Serializes as a stable snake_case string so a persisted dispatch (the P3
/// quiet-hours batch ledger) round-trips.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Money at risk now.
    P1,
    /// Data / edge degrading.
    P2,
    /// FYI.
    P3,
}

/// Where an alert is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Telegram + phone-call webhook (P1).
    TelegramPhone,
    /// Telegram immediately (P2).
    Telegram,
    /// Telegram, batched during quiet hours (P3).
    TelegramQuiet,
}

impl Severity {
    pub fn channel(self) -> Channel {
        match self {
            Severity::P1 => Channel::TelegramPhone,
            Severity::P2 => Channel::Telegram,
            Severity::P3 => Channel::TelegramQuiet,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::P1 => "P1",
            Severity::P2 => "P2",
            Severity::P3 => "P3",
        }
    }
}

/// A raised alert. `runbook` is the `ops/runbooks/{id}.md` link every P1/P2
/// MUST have (OPS-4, enforced by the guardrails lint). `dedupe_key` defaults
/// to the id; alerts that fan out per entity (one dead-man id across many
/// processes) MUST set a per-entity key so one entity's alert never suppresses
/// another's (regression_audit3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub id: String,
    pub severity: Severity,
    pub dedupe_window_ns: i64,
    pub runbook: String,
    pub detail: String,
    pub dedupe_key: String,
}

impl Alert {
    pub fn new(
        id: impl Into<String>,
        severity: Severity,
        dedupe_window_ns: i64,
        detail: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let runbook = format!("ops/runbooks/{id}.md");
        Alert {
            dedupe_key: id.clone(),
            id,
            severity,
            dedupe_window_ns,
            runbook,
            detail: detail.into(),
        }
    }

    /// Scope dedupe to an entity within this alert id (e.g. one process).
    pub fn with_dedupe_key(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = key.into();
        self
    }
}

/// A delivered alert: what actually goes out on a channel. Serializes so
/// edges can persist a batched dispatch (e.g. the P3 quiet-hours batch file)
/// and flush it later.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Dispatch {
    pub id: String,
    pub severity: Severity,
    pub channel: Channel,
    pub detail: String,
    pub runbook: String,
    pub ts_ns: i64,
}

impl Dispatch {
    /// Build the deliverable for a raised alert at `now_ns` — the exact shape
    /// routing would send. The router uses this internally; edges that must
    /// persist a Batched P3 (the Telegram batch file) build the same dispatch
    /// from the same inputs, so what gets batched is what would have been sent.
    pub fn from_alert(alert: &Alert, now_ns: i64) -> Dispatch {
        Dispatch {
            id: alert.id.clone(),
            severity: alert.severity,
            channel: alert.severity.channel(),
            detail: alert.detail.clone(),
            runbook: alert.runbook.clone(),
            ts_ns: now_ns,
        }
    }
}

/// The routing decision for one raised alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteOutcome {
    /// Sent immediately on its channel.
    Sent(Dispatch),
    /// Held for the quiet-hours digest (P3 only).
    Batched,
    /// Suppressed: fired again inside its dedupe window.
    Deduped,
}

/// UTC quiet-hours window as minutes-of-day `[start, end)`. Wraps midnight when
/// `start > end` (e.g. 22:00–07:00).
#[derive(Debug, Clone, Copy)]
pub struct QuietHours {
    pub start_min: u32,
    pub end_min: u32,
}

/// UTC minute-of-day (0..1440) for an epoch-ns clock reading. Pure arithmetic
/// on the injected clock — no wall-clock read (PD-3).
fn minute_of_day(now_ns: i64) -> u32 {
    let mod_day = now_ns.rem_euclid(86_400_000_000_000);
    (mod_day / 60_000_000_000) as u32
}

impl QuietHours {
    /// Whether `now_ns` falls in quiet hours. Pure arithmetic on the injected
    /// clock — no wall-clock read (PD-3).
    pub fn contains(&self, now_ns: i64) -> bool {
        let minute = minute_of_day(now_ns);
        if self.start_min <= self.end_min {
            minute >= self.start_min && minute < self.end_min
        } else {
            minute >= self.start_min || minute < self.end_min
        }
    }

    /// Minutes until quiet hours end for `now_ns` — the amount `telegram-flush
    /// --wait` sleeps before draining — or `0` when `now_ns` is OUTSIDE the
    /// window (a flush that starts after quiet hours end drains immediately,
    /// never sleeping ~24h for a wrap-around window). Pure arithmetic on the
    /// injected clock (PD-3).
    pub fn minutes_until_end(&self, now_ns: i64) -> u32 {
        if !self.contains(now_ns) {
            return 0;
        }
        let minute = minute_of_day(now_ns);
        if self.start_min <= self.end_min {
            self.end_min - minute
        } else {
            // Wraps midnight: `end_min` is next day's, so the wait spans the
            // remainder of today plus `end_min` minutes of tomorrow.
            (self.end_min + 1440 - minute) % 1440
        }
    }
}

/// Routes alerts with per-id dedupe and quiet-hours batching (OPS-4/OPS-9).
#[derive(Debug, Default)]
pub struct AlertRouter {
    last_fired: BTreeMap<String, i64>,
    quiet: Option<QuietHours>,
    batch: Vec<Dispatch>,
}

impl AlertRouter {
    pub fn new(quiet: Option<QuietHours>) -> Self {
        AlertRouter {
            last_fired: BTreeMap::new(),
            quiet,
            batch: Vec::new(),
        }
    }

    /// Route one raised alert at `now_ns`. Dedupe wins first; then P3 during
    /// quiet hours batches; everything else sends immediately. Dedupe is per
    /// `dedupe_key` (defaults to the id) so per-entity alerts sharing an id
    /// never suppress each other (regression_audit3).
    pub fn route(&mut self, alert: &Alert, now_ns: i64) -> RouteOutcome {
        if let Some(&last) = self.last_fired.get(&alert.dedupe_key) {
            if now_ns.saturating_sub(last) < alert.dedupe_window_ns {
                return RouteOutcome::Deduped;
            }
        }
        self.last_fired.insert(alert.dedupe_key.clone(), now_ns);

        let dispatch = Dispatch::from_alert(alert, now_ns);

        let quiet_now = self.quiet.map(|q| q.contains(now_ns)).unwrap_or(false);
        if alert.severity == Severity::P3 && quiet_now {
            self.batch.push(dispatch);
            RouteOutcome::Batched
        } else {
            RouteOutcome::Sent(dispatch)
        }
    }

    /// Drain the quiet-hours P3 digest (call when quiet hours end).
    pub fn drain_batch(&mut self) -> Vec<Dispatch> {
        std::mem::take(&mut self.batch)
    }

    /// Post a P1 dispatch to the owner-configured webhook sink (O1) — the
    /// P1 "phone-call webhook" channel made real and REACHABLE: the
    /// `mp-ops p1-webhook` subcommand is the shipped call site (owner decision
    /// 2026-08-06). When the owner sets `MP_OPS_P1_WEBHOOK` the dispatch JSON
    /// is POSTed there; when unset the subcommand fails loudly — "dead until
    /// creds", never a silent drop. Never called for P2/P3.
    ///
    /// TLS is the host's: like the Telegram edge (`post_telegram`), the send
    /// shells out to `curl` so https endpoints work without a Rust TLS
    /// dependency — a P1 (money-at-risk) channel is never forced to be
    /// cleartext-only (audit 08-04 #9). http is accepted too (local stubs /
    /// internal sinks). Fail-closed: any non-2xx or transport failure is an
    /// error to the caller — a sink that silently "accepted" a 500 would be
    /// worse than the honest error.
    ///
    /// Off the decision path (PD-3): this is alert egress, and it reads no
    /// clock and no secrets — the URL arrives via owner-managed env.
    pub fn post_p1_webhook(dispatch: &Dispatch, url: &str) -> Result<(), String> {
        debug_assert_eq!(dispatch.severity, Severity::P1, "webhook sink is P1-only");
        if !webhook_url_ok(url) {
            return Err("MP_OPS_P1_WEBHOOK must be an http:// or https:// URL".to_string());
        }
        let body = format!(
            "{{\"id\":{},\"severity\":{},\"detail\":{},\"runbook\":{},\"ts_ns\":{}}}",
            json_str(&dispatch.id),
            json_str(dispatch.severity.as_str()),
            json_str(&dispatch.detail),
            json_str(&dispatch.runbook),
            dispatch.ts_ns,
        );
        let mut cmd = std::process::Command::new("curl");
        cmd.args(["-sS", "-m", "30", "-X", "POST"]);
        cmd.arg("-H").arg("Content-Type: application/json");
        cmd.arg("--data-binary").arg(body);
        // Trailing status line (curl -w) after the response body; parsing the
        // last line keeps the code portable across Windows/Unix output sinks.
        cmd.arg("-w").arg("\n%{http_code}");
        cmd.arg(url);
        let out = cmd
            .output()
            .map_err(|e| format!("curl spawn failed (is curl installed?): {e}"))?;
        if !out.status.success() {
            // Do NOT echo curl's stderr verbatim (PD-2, audit 2026-08-08): on
            // transport errors stderr can embed the URL — which may carry a
            // sink token — while the success path deliberately never prints
            // it. Report only the exit code; the URL is owner-known in env.
            return Err(format!(
                "curl exited {} (stderr suppressed — may contain the webhook URL)",
                out.status
            ));
        }
        // Fail closed on anything but a 2xx: an alert sink that "accepted" a
        // 500 would be worse than the honest error.
        let text = String::from_utf8_lossy(&out.stdout);
        let code = text.rsplit('\n').next().unwrap_or_default().trim();
        if code.starts_with('2') {
            Ok(())
        } else {
            Err(format!("p1 webhook non-2xx: {code}"))
        }
    }

    pub fn batch_len(&self) -> usize {
        self.batch.len()
    }
}

/// Scheme gate for `MP_OPS_P1_WEBHOOK`: only http(s) is acceptable. Pure —
/// no network, no clock — so the decision "this URL is well-formed" is unit-
/// testable without a live endpoint.
fn webhook_url_ok(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Minimal JSON-string escaper for the P1 webhook payload (the ops crate does
/// not carry serde-derive for this one-shot body; ids/details are our own).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The webhook sink accepts http(s) and refuses anything else — the
    /// scheme gate is pure so the "this URL is well-formed" decision needs
    /// no network (audit 08-04 #9: https must be accepted, never forced
    /// cleartext).
    #[test]
    fn p1_webhook_url_gate_accepts_http_and_https_only() {
        assert!(webhook_url_ok("http://127.0.0.1:8080/hook"));
        assert!(webhook_url_ok("https://hooks.example.com/alert"));
        assert!(!webhook_url_ok("ftp://hooks.example.com/x"));
        assert!(!webhook_url_ok("hooks.example.com/x"));
        assert!(!webhook_url_ok(""));
    }
}
