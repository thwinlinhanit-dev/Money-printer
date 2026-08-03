//! Alert framework (OPS-4, OPS-9): every alert carries an id, severity, dedupe
//! window, and runbook link. Routing is deterministic — the clock is injected
//! as `now_ns` (PD-3), never read here. Quiet hours batch P3s; P1/P2 always
//! break through.

use std::collections::BTreeMap;

/// Severity drives the channel and the on-call expectation (spec 009 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Money at risk now.
    P1,
    /// Data / edge degrading.
    P2,
    /// FYI.
    P3,
}

/// Where an alert is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// A delivered alert: what actually goes out on a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatch {
    pub id: String,
    pub severity: Severity,
    pub channel: Channel,
    pub detail: String,
    pub runbook: String,
    pub ts_ns: i64,
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

impl QuietHours {
    /// Whether `now_ns` falls in quiet hours. Pure arithmetic on the injected
    /// clock — no wall-clock read (PD-3).
    pub fn contains(&self, now_ns: i64) -> bool {
        let mod_day = now_ns.rem_euclid(86_400_000_000_000);
        let minute = (mod_day / 60_000_000_000) as u32;
        if self.start_min <= self.end_min {
            minute >= self.start_min && minute < self.end_min
        } else {
            minute >= self.start_min || minute < self.end_min
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

        let dispatch = Dispatch {
            id: alert.id.clone(),
            severity: alert.severity,
            channel: alert.severity.channel(),
            detail: alert.detail.clone(),
            runbook: alert.runbook.clone(),
            ts_ns: now_ns,
        };

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

    /// Post a P1 dispatch to the owner-configured webhook sink (O1). This is
    /// the P1 "phone-call webhook" channel made real: when the owner sets
    /// `MP_OPS_P1_WEBHOOK` the binary edge posts the dispatch JSON there;
    /// when unset the caller keeps its warn-only fallback (documented —
    /// functionality-gated, not silently dead). Never called for P2/P3.
    ///
    /// Off the decision path (PD-3): this is alert egress, and it reads no
    /// clock and no secrets — the URL arrives via owner-managed env. Any
    /// network failure is reported to the caller so it can at least log it.
    pub fn post_p1_webhook(dispatch: &Dispatch, url: &str) -> Result<(), String> {
        debug_assert_eq!(dispatch.severity, Severity::P1, "webhook sink is P1-only");
        let body = format!(
            "{{\"id\":{},\"severity\":{},\"detail\":{},\"runbook\":{},\"ts_ns\":{}}}",
            json_str(&dispatch.id),
            json_str(dispatch.severity.as_str()),
            json_str(&dispatch.detail),
            json_str(&dispatch.runbook),
            dispatch.ts_ns,
        );
        let (host, path) = url
            .strip_prefix("http://")
            .filter(|_| !url.starts_with("https")) // https would need TLS; refuse honestly
            .and_then(|rest| rest.split_once('/'))
            .map(|(host, rest)| (host.to_string(), format!("/{rest}")))
            .ok_or_else(|| "MP_OPS_P1_WEBHOOK must be an http:// host[:port]/path URL".to_string())?;
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let mut stream = TcpStream::connect(host.as_str())
            .map_err(|e| format!("p1 webhook connect {host}: {e}"))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        stream.take(8192).read_to_end(&mut buf).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&buf);
        // Fail closed on anything but a 2xx: an alert sink that "accepted" a
        // 500 would be worse than the honest warn-only fallback.
        let status = text
            .split_whitespace()
            .nth(1)
            .unwrap_or_default();
        if status.starts_with('2') {
            Ok(())
        } else {
            Err(format!("p1 webhook non-2xx: {}", status))
        }
    }

    pub fn batch_len(&self) -> usize {
        self.batch.len()
    }
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
