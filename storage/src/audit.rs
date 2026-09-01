//! Raw recording integrity audit (spec 024).  This is deliberately independent
//! of compaction: a data-quality verdict is produced before cold storage can
//! be written, never reconstructed after the fact.

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, MarketEvent, SnapshotSource, SymbolMeta, Venue};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Minimum receive-clock coverage for a promotable day. This is the ROADMAP
/// Phase-0 validation gate: "7 consecutive days with manifest coverage
/// ≥ 0.995 on core symbols" (spec 024, decision 2026-08-12 — the gate
/// criterion is the numeric coverage bar; a single sub-tolerance gap or a
/// recovered stale blip must not veto an otherwise-complete day).
pub const MIN_COVERAGE: f64 = 0.995;

/// Zero-Cost Mode: lowered coverage threshold (docs/ZERO_COST_MODE.md).
pub const ZERO_COST_MIN_COVERAGE: f64 = 0.95;

/// Two `Status::Stale` events farther apart than this (ns) start a new burst
/// window. Matches `ops/scripts/audit_bursts.py` `BURST_GAP_S = 90` so the
/// burst grouping lives in one place (spec 024, decision 2026-08-12).
pub const STALE_BURST_GAP_NS: i64 = 90_000_000_000;

/// Expected identity and quality thresholds for one raw recording.
#[derive(Debug, Clone)]
pub struct AuditConfig {
    pub venue: Venue,
    pub symbol: String,
    pub max_gap_ns: i64,
    pub required_streams: BTreeSet<String>,
    /// Zero-Cost Mode: use lowered coverage threshold (0.95 instead of 0.995)
    /// and treat `book` stream absence as expected (docs/ZERO_COST_MODE.md).
    pub zero_cost_mode: bool,
}

impl AuditConfig {
    pub fn single(venue: Venue, symbol: impl Into<String>) -> Self {
        Self {
            venue,
            symbol: symbol.into(),
            max_gap_ns: 120_000_000_000,
            required_streams: BTreeSet::new(),
            zero_cost_mode: false,
        }
    }
}

/// A diagnostic that makes a raw log ineligible for promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditFinding {
    pub code: String,
    pub detail: String,
}

/// A contiguous absent period in receive-clock time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimeRange {
    pub start_ns: i64,
    pub end_ns: i64,
}

/// A raw log file discovered by [`discover_raw_logs`] (spec 024).
#[derive(Debug, Clone)]
pub struct RawLog {
    pub path: PathBuf,
    pub date: String,
    pub venue_str: String,
    pub symbol: String,
}

/// Discover raw logs in `raw_dir` matching the `{date}_{venue}_{symbol}.log`
/// naming contract. Non-matching files (stderr/stdout captures, test scripts,
/// old ad-hoc names) are skipped — they are not recordings.
pub fn discover_raw_logs(raw_dir: &Path) -> Vec<RawLog> {
    let mut logs = Vec::new();
    let Ok(entries) = std::fs::read_dir(raw_dir) else {
        return logs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        // Expected: YYYYMMDD_venue_SYMBOL
        let parts: Vec<&str> = stem.splitn(3, '_').collect();
        if parts.len() != 3 {
            continue;
        }
        // Validate date is 8 digits.
        if parts[0].len() != 8 || !parts[0].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        logs.push(RawLog {
            path,
            date: parts[0].to_string(),
            venue_str: parts[1].to_string(),
            symbol: parts[2].to_string(),
        });
    }
    logs.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(a.venue_str.cmp(&b.venue_str))
            .then(a.symbol.cmp(&b.symbol))
    });
    logs
}

/// Machine-readable quality verdict for one raw event log.
#[derive(Debug, Clone, Serialize)]
pub struct RawLogAudit {
    pub event_count: u64,
    pub first_recv_ts_ns: Option<i64>,
    pub last_recv_ts_ns: Option<i64>,
    pub coverage: f64,
    pub streams: BTreeMap<String, u64>,
    pub gaps: Vec<TimeRange>,
    /// Raw `Status::Stale` events (0-width ranges at each event's recv time).
    pub stale_periods: Vec<TimeRange>,
    /// Stale events collapsed into burst windows (events within
    /// [`STALE_BURST_GAP_NS`] of each other are one burst; start = first event,
    /// end = last event). Margin field — never a blocker (2026-08-12).
    pub stale_bursts: Vec<TimeRange>,
    /// Per-`Status::Stale`-event ACTUAL observed silence in ms, aligned 1:1
    /// with [`RawLogAudit::stale_periods`] (the same order), parsed from the
    /// collector watchdog's detail (`"no valid event for N ms (threshold ...)"`).
    /// This is the diagnostic that separates a short self-healing reconnect
    /// (a few seconds past the bar -> no recv-clock loss, coverage stays 1.0)
    /// from a real minutes-long feed outage (which also drops coverage). It is
    /// a margin field — never a blocker. Older logs that predate the measured
    /// silence (or a malformed detail) parse to 0.
    #[serde(default)]
    pub stale_silences_ms: Vec<u64>,
    /// Longest recv-clock hole in ns. Margin field — the gate's numeric
    /// criterion is aggregate [`RawLogAudit::coverage`], but the worst single
    /// hole must stay visible in every scorecard.
    pub worst_gap_ns: i64,
    pub findings: Vec<AuditFinding>,
}

impl RawLogAudit {
    /// A clean audit is the only state compaction may accept (INT-4).
    ///
    /// Clean means the ROADMAP Phase-0 criterion is met: `coverage ≥
    /// MIN_COVERAGE` (0.995) with no blocking findings (spec 024, decisions
    /// 2026-08-04 and 2026-08-12). Coverage is the aggregate completeness
    /// number — a single gap's severity is its contribution to coverage, not
    /// a binary veto — so `recv_time_reversal`, `stale_stream` (a COL-2
    /// recovery signal: the collector already reconnected) and `coverage_gap`
    /// are warnings. Codes that mean the data cannot be attributed (parse /
    /// identity / provenance) or that record loss coverage cannot see
    /// (venue-side `sequence_gap`, dropped frames) still block, and
    /// `low_coverage` carries the numeric verdict when the bar is missed.
    pub fn is_clean(&self) -> bool {
        self.event_count > 0
            && self.coverage >= MIN_COVERAGE
            && self.findings.iter().all(|f| !is_blocking_finding(&f.code))
    }

    /// Zero-Cost Mode: clean means coverage >= 0.95 with no blocking findings
    /// (docs/ZERO_COST_MODE.md). Full book absence is expected, not a finding.
    pub fn is_clean_zero_cost(&self) -> bool {
        self.event_count > 0
            && self.coverage >= ZERO_COST_MIN_COVERAGE
            && self.findings.iter().all(|f| !is_blocking_finding(&f.code))
    }
}

/// Findings that make a recording ineligible for promotion. Every code that
/// indicates unattributable data — malformed/unreadable frames, missing
/// provenance, venue/symbol identity issues, an empty log — or real loss the
/// aggregate coverage number cannot see (venue-side `sequence_gap`, dropped
/// frames) blocks. `recv_time_reversal`, `stale_stream` and `coverage_gap` are
/// warnings: their severity is already captured by `coverage` (spec 024,
/// decision 2026-08-12 — on 2026-08-09 a single ~138s gap plus 400 stale
/// events left coverage at 0.9984, above the 0.995 bar; a sub-tolerance gap
/// must not veto such a day), and `low_coverage` is the blocker that carries
/// the numeric verdict.
pub fn is_blocking_finding(code: &str) -> bool {
    !matches!(code, "recv_time_reversal" | "stale_stream" | "coverage_gap")
}

/// Audit a single `(venue, symbol)` raw log.  Any malformed or older schema is
/// a quarantine finding; no caller gets a partial "probably fine" verdict.
///
/// A missing day-file is its own named finding (`recording_missing`), distinct
/// from a corrupt one (`unreadable_log`) — scoring a day whose sources never
/// landed (failed VPS drain) must be greppable as "no data here", not look
/// like decode trouble (incident 2026-08-22 post-mortem).
pub fn audit_raw_log(path: &Path, config: &AuditConfig) -> RawLogAudit {
    let mut audit = RawLogAudit {
        event_count: 0,
        first_recv_ts_ns: None,
        last_recv_ts_ns: None,
        coverage: 0.0,
        streams: BTreeMap::new(),
        gaps: Vec::new(),
        stale_periods: Vec::new(),
        stale_bursts: Vec::new(),
        stale_silences_ms: Vec::new(),
        worst_gap_ns: 0,
        findings: Vec::new(),
    };

    if !path.exists() {
        audit
            .findings
            .push(finding("recording_missing", path.display().to_string()));
        return audit;
    }
    let mut reader = match LogReader::open(path) {
        Ok(reader) => reader,
        Err(error) => {
            audit
                .findings
                .push(finding("unreadable_log", error.to_string()));
            return audit;
        }
    };
    let mut previous_recv = None;
    loop {
        let event = match reader.next() {
            Some(Ok(event)) => event,
            Some(Err(error)) => {
                audit
                    .findings
                    .push(finding("legacy_or_malformed", error.to_string()));
                break;
            }
            None => break,
        };
        validate_event(&event, reader.symbols(), config, &mut audit.findings);
        *audit
            .streams
            .entry(event.provenance.stream.clone())
            .or_default() += 1;
        audit.event_count += 1;
        audit.first_recv_ts_ns.get_or_insert(event.recv_ts_ns);
        if let Some(previous) = previous_recv {
            if event.recv_ts_ns < previous {
                audit.findings.push(finding(
                    "recv_time_reversal",
                    format!("{previous} then {}", event.recv_ts_ns),
                ));
            } else if event.recv_ts_ns - previous > config.max_gap_ns {
                let duration = event.recv_ts_ns - previous;
                audit.worst_gap_ns = audit.worst_gap_ns.max(duration);
                audit.gaps.push(TimeRange {
                    start_ns: previous,
                    end_ns: event.recv_ts_ns,
                });
            }
        }
        if let MarketEvent::Status { kind, detail } = &event.body {
            match kind {
                mp_core::StatusKind::Stale => {
                    audit.stale_periods.push(TimeRange {
                        start_ns: event.recv_ts_ns,
                        end_ns: event.recv_ts_ns,
                    });
                    audit.stale_silences_ms.push(parse_stale_silence_ms(detail));
                }
                mp_core::StatusKind::GapDetected => audit.findings.push(finding(
                    "sequence_gap",
                    format!("gap status at {}", event.recv_ts_ns),
                )),
                mp_core::StatusKind::BackpressureDrop { dropped } => audit.findings.push(finding(
                    "backpressure_loss",
                    format!("{dropped} dropped frame(s) at {}", event.recv_ts_ns),
                )),
                _ => {}
            }
        }
        previous_recv = Some(event.recv_ts_ns);
        audit.last_recv_ts_ns = Some(event.recv_ts_ns);
    }

    if audit.event_count == 0 {
        audit
            .findings
            .push(finding("empty_log", "no readable events"));
    }
    for stream in &config.required_streams {
        if !audit.streams.contains_key(stream) {
            audit
                .findings
                .push(finding("missing_stream", stream.clone()));
        }
    }
    // Group stale status events into burst windows (2026-08-12). Sorted
    // defensively: recv order can jitter (`recv_time_reversal` is a warning,
    // not a blocker, so a stale event may land slightly out of order).
    let mut stale_ts: Vec<i64> = audit.stale_periods.iter().map(|p| p.start_ns).collect();
    stale_ts.sort_unstable();
    for ts in stale_ts {
        match audit.stale_bursts.last_mut() {
            Some(burst) if ts - burst.end_ns <= STALE_BURST_GAP_NS => {
                burst.end_ns = burst.end_ns.max(ts);
            }
            _ => audit.stale_bursts.push(TimeRange {
                start_ns: ts,
                end_ns: ts,
            }),
        }
    }

    if !audit.gaps.is_empty() {
        audit.findings.push(finding(
            "coverage_gap",
            format!("{} gap(s)", audit.gaps.len()),
        ));
    }
    if !audit.stale_periods.is_empty() {
        audit.findings.push(finding(
            "stale_stream",
            format!("{} stale status event(s)", audit.stale_periods.len()),
        ));
    }
    audit.coverage = coverage(&audit);
    let min_cov = if config.zero_cost_mode {
        ZERO_COST_MIN_COVERAGE
    } else {
        MIN_COVERAGE
    };
    if audit.event_count > 0 && audit.coverage < min_cov {
        audit.findings.push(finding(
            "low_coverage",
            format!("coverage {:.4} < required {min_cov}", audit.coverage),
        ));
    }
    audit
}

fn validate_event(
    event: &EventEnvelope,
    symbols: &[SymbolMeta],
    config: &AuditConfig,
    findings: &mut Vec<AuditFinding>,
) {
    if event.venue != config.venue {
        findings.push(finding(
            "venue_mismatch",
            format!("event has {:?}", event.venue),
        ));
    }
    match symbols.get(event.symbol.0 as usize) {
        Some(meta) if meta.symbol_id == event.symbol => {
            if meta.venue != config.venue || meta.venue_symbol != config.symbol {
                findings.push(finding(
                    "symbol_mismatch",
                    format!(
                        "symbol id {} resolves to {:?}/{}",
                        event.symbol.0, meta.venue, meta.venue_symbol
                    ),
                ));
            }
        }
        _ => findings.push(finding(
            "invalid_symbol_table",
            format!("symbol id {} is not present", event.symbol.0),
        )),
    }
    if event.provenance.stream.is_empty() || event.provenance.subscription.is_empty() {
        findings.push(finding(
            "missing_provenance",
            "stream/subscription is not live-attributable",
        ));
    }
    if matches!(event.body, MarketEvent::BookSnapshot { .. })
        && matches!(event.provenance.snapshot_source, SnapshotSource::None)
    {
        findings.push(finding(
            "missing_snapshot_source",
            "book snapshot has no source",
        ));
    }
}

/// Parse the ACTUAL measured silence (ms) the collector observed before it
/// declared a stream stale, from the `Status::Stale` detail string. Older
/// logs use `"no valid event for {ns} ns"`; current ones encode
/// `"no valid event for {ms} ms (threshold {ms} ms; topics)". A malformed or
/// unknown detail parses to 0 (no loss attribution claimed).
fn parse_stale_silence_ms(detail: &str) -> u64 {
    if let Some(rest) = detail.strip_prefix("no valid event for ") {
        let mut it = rest.split(' ');
        if let (Some(num), Some(unit)) = (it.next(), it.next()) {
            if let Ok(n) = num.parse::<u64>() {
                if unit.starts_with("ms") {
                    return n;
                }
                if unit.starts_with("ns") {
                    return n / 1_000_000;
                }
            }
        }
    }
    0
}

fn coverage(audit: &RawLogAudit) -> f64 {
    let (Some(first), Some(last)) = (audit.first_recv_ts_ns, audit.last_recv_ts_ns) else {
        return 0.0;
    };
    let span = last.saturating_sub(first);
    if span == 0 {
        return 1.0;
    }
    let gaps: i64 = audit
        .gaps
        .iter()
        .map(|gap| gap.end_ns.saturating_sub(gap.start_ns))
        .sum();
    (1.0 - gaps as f64 / span as f64).clamp(0.0, 1.0)
}

fn finding(code: impl Into<String>, detail: impl Into<String>) -> AuditFinding {
    AuditFinding {
        code: code.into(),
        detail: detail.into(),
    }
}

/// A daily matrix verdict.  All required recordings must pass; a healthy
/// process for one symbol can never mask another symbol's missing recording.
#[derive(Debug, Clone, Serialize)]
pub struct DailyScorecard {
    pub date: String,
    pub recordings: Vec<ScorecardEntry>,
    pub promotable: bool,
    /// Per-recording stale-burst counts for the Phase-0 promotion window
    /// condition (spec 024, amendment 2026-08-12). Zero `stale_bursts` on
    /// every required recording for every day in the qualifying window is
    /// required for `PROMOTED`; a burst alone never makes a day
    /// non-promotable (the 2026-08-12 tolerance semantics hold — the streak
    /// keeps counting bursty days, the window condition is what holds
    /// promotion back).
    pub recording_bursts: Vec<RecordingBursts>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScorecardEntry {
    pub venue: String,
    pub symbol: String,
    pub clean: bool,
    pub audit: RawLogAudit,
}

/// One recording's stale-burst count, carried into the promotion gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingBursts {
    pub venue: String,
    pub symbol: String,
    /// Number of stale-event burst windows on this recording that day
    /// (margin, never a per-day veto — see [`DailyScorecard::recording_bursts`]).
    pub stale_bursts: usize,
}

pub fn scorecard(
    date: impl Into<String>,
    entries: Vec<(Venue, String, RawLogAudit)>,
) -> DailyScorecard {
    let mut recordings = Vec::with_capacity(entries.len());
    let mut recording_bursts = Vec::with_capacity(entries.len());
    for (venue, symbol, audit) in entries {
        recording_bursts.push(RecordingBursts {
            venue: venue.slug().to_owned(),
            symbol: symbol.clone(),
            stale_bursts: audit.stale_bursts.len(),
        });
        recordings.push(ScorecardEntry {
            venue: venue.slug().to_owned(),
            symbol,
            clean: audit.is_clean(),
            audit,
        });
    }
    let promotable = !recordings.is_empty() && recordings.iter().all(|entry| entry.clean);
    DailyScorecard {
        date: date.into(),
        recordings,
        promotable,
        recording_bursts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::log::EventLogWriter;
    use mp_core::{EventProvenance, MarketEvent, Side, SymbolId};

    fn event(recv: i64) -> EventEnvelope {
        EventEnvelope::new(
            Venue::BinanceFutures,
            SymbolId(0),
            recv,
            recv,
            recv as u64,
            MarketEvent::Trade {
                price: 1.0,
                qty: 1.0,
                side: Side::Buy,
                trade_id: recv as u64,
            },
        )
        .with_provenance(EventProvenance {
            stream: "trade".into(),
            subscription: "btcusdt@aggTrade".into(),
            connection_id: 1,
            snapshot_source: SnapshotSource::None,
        })
    }

    fn fixture(events: Vec<EventEnvelope>) -> std::path::PathBuf {
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mp-int-{}-{nonce}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (mut writer, _) = EventLogWriter::open(&path).unwrap();
        writer
            .write_symbols(&[mp_core::SymbolMeta::new(
                SymbolId(0),
                Venue::BinanceFutures,
                "BTCUSDT",
                "BTC",
                "USDT",
                mp_core::InstrumentKind::Perp,
                0.1,
                0.001,
                1.0,
            )])
            .unwrap();
        for event in events {
            writer.append(&event).unwrap();
        }
        writer.sync().unwrap();
        path
    }

    fn clean_audit() -> RawLogAudit {
        RawLogAudit {
            event_count: 100,
            first_recv_ts_ns: Some(1),
            last_recv_ts_ns: Some(1_000_000_000),
            coverage: 1.0,
            streams: BTreeMap::new(),
            gaps: vec![],
            stale_periods: vec![],
            stale_bursts: vec![],
            stale_silences_ms: vec![],
            worst_gap_ns: 0,
            findings: vec![],
        }
    }

    #[test]
    fn int_1_event_provenance_roundtrips() {
        let path = fixture(vec![event(1)]);
        let audit = audit_raw_log(
            &path,
            &AuditConfig::single(Venue::BinanceFutures, "BTCUSDT"),
        );
        assert!(audit.is_clean(), "{:?}", audit.findings);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn int_2_mixed_log_is_quarantined() {
        let mut bad = event(1);
        bad.venue = Venue::Hyperliquid;
        let path = fixture(vec![bad]);
        let audit = audit_raw_log(
            &path,
            &AuditConfig::single(Venue::BinanceFutures, "BTCUSDT"),
        );
        assert!(audit
            .findings
            .iter()
            .any(|finding| finding.code == "venue_mismatch"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn int_recording_missing_is_named_and_blocking() {
        // Incident 2026-08-22 post-mortem: a day whose sources never landed
        // must audit as "no data here" (recording_missing), not decode
        // trouble — and it blocks like every other unattributable state.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let missing =
            std::env::temp_dir().join(format!("mp-missing-{}-{seq}.log", std::process::id(),));
        let _ = std::fs::remove_file(&missing);
        let config = AuditConfig::single(Venue::BinanceFutures, "BTCUSDT");
        let audit = audit_raw_log(&missing, &config);
        assert_eq!(audit.event_count, 0);
        let codes: Vec<_> = audit.findings.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"recording_missing"), "{codes:?}");
        assert!(!codes.contains(&"unreadable_log"), "{codes:?}");
        assert!(!audit.is_clean());
    }

    #[test]
    fn int_3_audit_reports_gap_and_staleness() {
        let mut stale = event(300);
        stale.body = MarketEvent::Status {
            kind: mp_core::StatusKind::Stale,
            detail: "stale".into(),
        };
        let path = fixture(vec![event(1), stale]);
        let mut config = AuditConfig::single(Venue::BinanceFutures, "BTCUSDT");
        config.max_gap_ns = 10;
        let audit = audit_raw_log(&path, &config);
        assert_eq!(audit.gaps.len(), 1);
        assert_eq!(audit.stale_periods.len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn int_4_compaction_refuses_quarantined_log() {
        use crate::compactor::compact_day_verified;
        use crate::layout;
        use mp_core::{InstrumentKind, SymbolMeta, SymbolTable};

        let root = std::env::temp_dir().join(format!("mp-int4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut syms = SymbolTable::new();
        let id = syms.intern(Venue::BinanceFutures, "BTCUSDT", |id| {
            SymbolMeta::new(
                id,
                Venue::BinanceFutures,
                "BTCUSDT",
                "BTC",
                "USDT",
                InstrumentKind::Perp,
                0.1,
                0.001,
                1.0,
            )
        });

        // Contaminated log: a foreign-venue event mixed into a Binance log.
        let mut bad = event(2);
        bad.venue = Venue::Hyperliquid;
        let bad_log = fixture(vec![event(1), bad.clone()]);
        let audit = audit_raw_log(
            &bad_log,
            &AuditConfig::single(Venue::BinanceFutures, "BTCUSDT"),
        );
        assert!(!audit.is_clean(), "mixed log must be quarantined");

        let refused = compact_day_verified(
            &root,
            Venue::BinanceFutures,
            "2026-07-29",
            0,
            i64::MAX,
            vec![event(1), bad],
            &syms,
            "hashA",
            "gitsha",
            0,
            &audit,
        );
        let msg = format!("{refused:?}");
        assert!(
            refused.is_err(),
            "compaction of a quarantined log must refuse: {msg}"
        );
        assert!(
            msg.contains("Refused"),
            "refusal must be a Refused error: {msg}"
        );
        assert!(
            !layout::partition_file(
                &root,
                "trades",
                Venue::BinanceFutures,
                "BTCUSDT",
                "2026-07-29"
            )
            .exists(),
            "contaminated trades must never reach cold storage"
        );
        let _ = std::fs::remove_file(&bad_log);

        // Positive control: a clean log compacts normally.
        let clean_log = fixture(vec![event(3), event(4)]);
        let clean = audit_raw_log(
            &clean_log,
            &AuditConfig::single(Venue::BinanceFutures, "BTCUSDT"),
        );
        assert!(clean.is_clean());
        let stats = compact_day_verified(
            &root,
            Venue::BinanceFutures,
            "2026-07-29",
            0,
            i64::MAX,
            vec![event(3), event(4)],
            &syms,
            "hashB",
            "gitsha",
            0,
            &clean,
        )
        .unwrap();
        assert_eq!(stats.trade_rows, 2);
        assert!(
            layout::partition_file(
                &root,
                "trades",
                Venue::BinanceFutures,
                "BTCUSDT",
                "2026-07-29"
            )
            .exists(),
            "clean compaction must write cold storage"
        );
        let _ = std::fs::remove_file(&clean_log);
        let _ = std::fs::remove_dir_all(&root);
        let _ = syms;
        let _ = id;
    }

    #[test]
    fn int_6_recv_time_reversal_is_a_warning_not_a_blocker() {
        let mut audit = RawLogAudit {
            event_count: 2,
            first_recv_ts_ns: Some(1),
            last_recv_ts_ns: Some(2),
            coverage: 1.0,
            streams: BTreeMap::new(),
            gaps: vec![],
            stale_periods: vec![],
            stale_bursts: vec![],
            stale_silences_ms: vec![],
            worst_gap_ns: 0,
            findings: vec![finding(
                "recv_time_reversal",
                "arrival-order jitter, all frames present",
            )],
        };
        assert!(audit.is_clean(), "reversal-only log must be promotable");

        // Loss the coverage number cannot see still blocks.
        audit
            .findings
            .push(finding("sequence_gap", "venue-side gap"));
        assert!(!audit.is_clean(), "sequence_gap must block promotion");
        audit.findings.clear();
        audit.findings.push(finding("backpressure_loss", "dropped"));
        assert!(!audit.is_clean(), "backpressure_loss must block promotion");

        // COL-2 recovery signals are warnings, not vetoes (2026-08-12).
        audit.findings.clear();
        audit.findings.push(finding("stale_stream", "stale status"));
        assert!(
            audit.is_clean(),
            "stale_stream is a warning (collector already recovered), not a blocker"
        );
        audit.findings.clear();
        audit.findings.push(finding("coverage_gap", "one 138s gap"));
        assert!(
            audit.is_clean(),
            "a single sub-tolerance gap must not veto a 0.9984-coverage day"
        );
    }

    #[test]
    fn int_7_blocking_code_classification() {
        assert!(!is_blocking_finding("recv_time_reversal"));
        assert!(!is_blocking_finding("stale_stream"));
        assert!(!is_blocking_finding("coverage_gap"));
        for code in [
            "legacy_or_malformed",
            "unreadable_log",
            "empty_log",
            "missing_stream",
            "missing_provenance",
            "venue_mismatch",
            "symbol_mismatch",
            "invalid_symbol_table",
            "missing_snapshot_source",
            "sequence_gap",
            "backpressure_loss",
            "low_coverage",
        ] {
            assert!(is_blocking_finding(code), "{code} must block");
        }
    }

    #[test]
    fn int_8_coverage_below_threshold_blocks_without_findings() {
        // The numeric bar is the gate: coverage 0.99 is DIRTY even with no
        // other findings (spec 024, decision 2026-08-12).
        let audit = RawLogAudit {
            event_count: 10,
            first_recv_ts_ns: Some(1),
            last_recv_ts_ns: Some(86_400_000_000_000),
            coverage: 0.99,
            streams: BTreeMap::new(),
            gaps: vec![],
            stale_periods: vec![],
            stale_bursts: vec![],
            stale_silences_ms: vec![],
            worst_gap_ns: 0,
            findings: vec![],
        };
        assert!(!audit.is_clean(), "coverage 0.99 < 0.995 must block");

        let mut at_bar = audit.clone();
        at_bar.coverage = 0.995;
        assert!(at_bar.is_clean(), "coverage at the 0.995 bar is clean");
    }

    #[test]
    fn int_9_stale_bursts_group_nearby_stale_events_and_worst_gap() {
        fn stale_event(recv: i64) -> EventEnvelope {
            let mut e = event(recv);
            e.body = MarketEvent::Status {
                kind: mp_core::StatusKind::Stale,
                detail: "stale".into(),
            };
            e
        }
        // 1s event; stale at 10s and 25s (15s apart -> one burst); data at
        // 30s; then a >120s recv hole (170s -> worst gap), stale at 200s,
        // data at 201s.
        let path = fixture(vec![
            event(1_000_000_000),
            stale_event(10_000_000_000),
            stale_event(25_000_000_000),
            event(30_000_000_000),
            stale_event(200_000_000_000),
            event(201_000_000_000),
        ]);
        let audit = audit_raw_log(
            &path,
            &AuditConfig::single(Venue::BinanceFutures, "BTCUSDT"),
        );
        assert_eq!(audit.stale_bursts.len(), 2, "{:?}", audit.stale_bursts);
        assert_eq!(audit.stale_bursts[0].start_ns, 10_000_000_000);
        assert_eq!(audit.stale_bursts[0].end_ns, 25_000_000_000);
        assert_eq!(audit.stale_bursts[1].start_ns, 200_000_000_000);
        assert_eq!(audit.worst_gap_ns, 170_000_000_000);
        // The gap and the stale events are reported, but they are warnings:
        // the day fails on the numeric bar (coverage ~0.15 < 0.995).
        assert!(
            audit.findings.iter().any(|f| f.code == "low_coverage"),
            "{:?}",
            audit.findings
        );
        assert!(!audit.is_clean());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn int_5_scorecard_requires_every_recording_clean() {
        let clean = RawLogAudit {
            event_count: 1,
            first_recv_ts_ns: Some(1),
            last_recv_ts_ns: Some(1),
            coverage: 1.0,
            streams: BTreeMap::new(),
            gaps: vec![],
            stale_periods: vec![],
            stale_bursts: vec![],
            stale_silences_ms: vec![],
            worst_gap_ns: 0,
            findings: vec![],
        };
        let bad = RawLogAudit {
            findings: vec![finding("missing_stream", "book")],
            ..clean.clone()
        };
        let card = scorecard(
            "2026-07-29",
            vec![
                (Venue::BinanceFutures, "BTCUSDT".into(), clean),
                (Venue::BinanceFutures, "ETHUSDT".into(), bad),
            ],
        );
        assert!(!card.promotable);
    }

    // ---- Zero-Cost Mode audit tests (docs/ZERO_COST_MODE.md) --------------------

    #[test]
    fn zero_cost_clean_at_095_coverage() {
        let audit = RawLogAudit {
            event_count: 100,
            coverage: 0.95,
            ..clean_audit()
        };
        assert!(audit.is_clean_zero_cost(), "0.95 coverage must pass Zero-Cost");
    }

    #[test]
    fn zero_cost_clean_above_095_coverage() {
        let audit = RawLogAudit {
            event_count: 100,
            coverage: 0.999,
            ..clean_audit()
        };
        assert!(audit.is_clean_zero_cost(), ">0.95 coverage must pass Zero-Cost");
    }

    #[test]
    fn zero_cost_dirty_below_095_coverage() {
        let audit = RawLogAudit {
            event_count: 100,
            coverage: 0.94,
            ..clean_audit()
        };
        assert!(!audit.is_clean_zero_cost(), "<0.95 coverage must fail Zero-Cost");
    }

    #[test]
    fn zero_cost_dirty_with_blocking_finding() {
        let audit = RawLogAudit {
            event_count: 100,
            coverage: 0.98,
            findings: vec![finding("sequence_gap", "3 gap(s)")],
            ..clean_audit()
        };
        assert!(!audit.is_clean_zero_cost(), "blocking finding must fail Zero-Cost");
    }

    #[test]
    fn zero_cost_clean_with_warning_finding() {
        let audit = RawLogAudit {
            event_count: 100,
            coverage: 0.96,
            findings: vec![finding("stale_stream", "2 stale status event(s)")],
            ..clean_audit()
        };
        assert!(audit.is_clean_zero_cost(), "warning finding must pass Zero-Cost");
    }

    #[test]
    fn zero_cost_dirty_with_zero_events() {
        let audit = RawLogAudit {
            event_count: 0,
            coverage: 0.0,
            ..clean_audit()
        };
        assert!(!audit.is_clean_zero_cost(), "zero events must fail Zero-Cost");
    }

    /// Full-mode 0.95 coverage must FAIL (threshold is 0.995).
    #[test]
    fn full_mode_rejects_095_coverage() {
        let audit = RawLogAudit {
            event_count: 100,
            coverage: 0.95,
            ..clean_audit()
        };
        assert!(!audit.is_clean(), "0.95 coverage must fail full-mode (needs 0.995)");
        assert!(audit.is_clean_zero_cost(), "0.95 coverage must pass Zero-Cost");
    }
}
