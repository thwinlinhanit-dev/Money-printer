//! Raw recording integrity audit (spec 024).  This is deliberately independent
//! of compaction: a data-quality verdict is produced before cold storage can
//! be written, never reconstructed after the fact.

use mp_core::log::LogReader;
use mp_core::{EventEnvelope, MarketEvent, SnapshotSource, SymbolMeta, Venue};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Expected identity and quality thresholds for one raw recording.
#[derive(Debug, Clone)]
pub struct AuditConfig {
    pub venue: Venue,
    pub symbol: String,
    pub max_gap_ns: i64,
    pub required_streams: BTreeSet<String>,
}

impl AuditConfig {
    pub fn single(venue: Venue, symbol: impl Into<String>) -> Self {
        Self {
            venue,
            symbol: symbol.into(),
            max_gap_ns: 120_000_000_000,
            required_streams: BTreeSet::new(),
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

/// Machine-readable quality verdict for one raw event log.
#[derive(Debug, Clone, Serialize)]
pub struct RawLogAudit {
    pub event_count: u64,
    pub first_recv_ts_ns: Option<i64>,
    pub last_recv_ts_ns: Option<i64>,
    pub coverage: f64,
    pub streams: BTreeMap<String, u64>,
    pub gaps: Vec<TimeRange>,
    pub stale_periods: Vec<TimeRange>,
    pub findings: Vec<AuditFinding>,
}

impl RawLogAudit {
    /// A clean audit is the only state compaction may accept (INT-4).
    pub fn is_clean(&self) -> bool {
        self.event_count > 0 && self.findings.is_empty()
    }
}

/// Audit a single `(venue, symbol)` raw log.  Any malformed or older schema is
/// a quarantine finding; no caller gets a partial "probably fine" verdict.
pub fn audit_raw_log(path: &Path, config: &AuditConfig) -> RawLogAudit {
    let mut audit = RawLogAudit {
        event_count: 0,
        first_recv_ts_ns: None,
        last_recv_ts_ns: None,
        coverage: 0.0,
        streams: BTreeMap::new(),
        gaps: Vec::new(),
        stale_periods: Vec::new(),
        findings: Vec::new(),
    };

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
                audit.gaps.push(TimeRange {
                    start_ns: previous,
                    end_ns: event.recv_ts_ns,
                });
            }
        }
        if let MarketEvent::Status { kind, .. } = &event.body {
            match kind {
                mp_core::StatusKind::Stale => audit.stale_periods.push(TimeRange {
                    start_ns: event.recv_ts_ns,
                    end_ns: event.recv_ts_ns,
                }),
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
}

#[derive(Debug, Clone, Serialize)]
pub struct ScorecardEntry {
    pub venue: String,
    pub symbol: String,
    pub clean: bool,
    pub audit: RawLogAudit,
}

pub fn scorecard(
    date: impl Into<String>,
    entries: Vec<(Venue, String, RawLogAudit)>,
) -> DailyScorecard {
    let recordings = entries
        .into_iter()
        .map(|(venue, symbol, audit)| ScorecardEntry {
            venue: venue.slug().to_owned(),
            symbol,
            clean: audit.is_clean(),
            audit,
        })
        .collect::<Vec<_>>();
    let promotable = !recordings.is_empty() && recordings.iter().all(|entry| entry.clean);
    DailyScorecard {
        date: date.into(),
        recordings,
        promotable,
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
    fn int_5_scorecard_requires_every_recording_clean() {
        let clean = RawLogAudit {
            event_count: 1,
            first_recv_ts_ns: Some(1),
            last_recv_ts_ns: Some(1),
            coverage: 1.0,
            streams: BTreeMap::new(),
            gaps: vec![],
            stale_periods: vec![],
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
}
