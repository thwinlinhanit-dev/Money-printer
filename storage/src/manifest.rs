//! Quality manifests (STO-2, STO-5). The honesty layer: a gap you know about
//! is data; a gap you don't is poison. Every research/sim read consults these
//! before trusting a range (SIM-6 depends on it).

use mp_core::{EventEnvelope, MarketEvent, SnapshotReason, StatusKind, SymbolId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Why a span of time is not covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// Connection dropped (Status::Disconnected → Connected).
    Disconnect,
    /// Ring-buffer overrun (consumer fell behind).
    Overrun,
    /// Venue-side sequence gap (GapDetected → next GapResync snapshot).
    Venue,
}

/// A half-open uncovered interval `[from_ns, to_ns)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    pub from_ns: i64,
    pub to_ns: i64,
    pub kind: GapKind,
}

/// Per-stream quality stats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamStats {
    pub events: u64,
    pub first_ts_ns: i64,
    pub last_ts_ns: i64,
    pub gaps: Vec<Gap>,
    /// Fraction of the day not inside a gap.
    pub coverage: f64,
    /// True if the venue samples this stream (e.g. Binance liquidations).
    pub sampled: bool,
}

/// One venue/day manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityManifest {
    pub venue: String,
    pub date: String,
    pub schema_ver: u16,
    pub day_start_ns: i64,
    pub day_end_ns: i64,
    pub streams: BTreeMap<String, StreamStats>,
    pub compactor_version: String,
    pub created_ts_ns: i64,
}

impl QualityManifest {
    /// Coverage of `stream_key` (e.g. `"trades:BTCUSDT"`), or `None` if absent.
    pub fn coverage(&self, stream_key: &str) -> Option<f64> {
        self.streams.get(stream_key).map(|s| s.coverage)
    }

    /// Gaps recorded for `stream_key`.
    pub fn gaps(&self, stream_key: &str) -> &[Gap] {
        self.streams
            .get(stream_key)
            .map(|s| s.gaps.as_slice())
            .unwrap_or(&[])
    }
}

#[derive(Default)]
struct StreamAcc {
    events: u64,
    first: i64,
    last: i64,
}

fn bump(streams: &mut BTreeMap<(String, SymbolId), StreamAcc>, name: &str, sym: SymbolId, ts: i64) {
    let acc = streams.entry((name.to_owned(), sym)).or_insert(StreamAcc {
        events: 0,
        first: i64::MAX,
        last: i64::MIN,
    });
    acc.events += 1;
    acc.first = acc.first.min(ts);
    acc.last = acc.last.max(ts);
}

/// Merge overlapping/adjacent intervals and return total covered duration within
/// `[lo, hi)`, so overlapping gaps aren't double-counted.
fn merged_duration(mut gaps: Vec<(i64, i64)>, lo: i64, hi: i64) -> i64 {
    if gaps.is_empty() || hi <= lo {
        return 0;
    }
    gaps.sort_by_key(|g| g.0);
    let mut total = 0i64;
    let mut cur_from = gaps[0].0.clamp(lo, hi);
    let mut cur_to = gaps[0].1.clamp(lo, hi);
    for &(f, t) in &gaps[1..] {
        let f = f.clamp(lo, hi);
        let t = t.clamp(lo, hi);
        if f > cur_to {
            total += cur_to - cur_from;
            cur_from = f;
            cur_to = t;
        } else if t > cur_to {
            cur_to = t;
        }
    }
    total += cur_to - cur_from;
    total
}

/// Derive a manifest from one venue/day's events (STO-2). `symbol_name` resolves
/// interned ids to their venue symbol strings.
///
/// Gap rules (v1, documented in spec 003 Decisions):
/// - `Disconnected`→`Connected` on symbol S ⇒ Disconnect gap over *all* of S's
///   streams (a dropped socket loses every stream for that symbol).
/// - `GapDetected`→ next `BookSnapshot{GapResync}` on S ⇒ Venue (or Overrun, per
///   the detail text) gap over S's `book_deltas` stream.
/// - Any interval still open at `day_end_ns` closes there.
#[allow(clippy::too_many_arguments)]
pub fn derive_manifest(
    venue: &str,
    date: &str,
    day_start_ns: i64,
    day_end_ns: i64,
    events: impl Iterator<Item = EventEnvelope>,
    symbol_name: impl Fn(SymbolId) -> String,
    compactor_version: &str,
    created_ts_ns: i64,
) -> QualityManifest {
    let mut disconnect: BTreeMap<SymbolId, Vec<(i64, i64)>> = BTreeMap::new();
    let mut open_disc: BTreeMap<SymbolId, i64> = BTreeMap::new();
    let mut seqgap: BTreeMap<SymbolId, Vec<(i64, i64, GapKind)>> = BTreeMap::new();
    let mut open_seq: BTreeMap<SymbolId, (i64, GapKind)> = BTreeMap::new();
    let mut streams: BTreeMap<(String, SymbolId), StreamAcc> = BTreeMap::new();

    for e in events {
        let sym = e.symbol;
        match &e.body {
            MarketEvent::Status { kind, detail } => match kind {
                StatusKind::Disconnected => {
                    open_disc.entry(sym).or_insert(e.recv_ts_ns);
                }
                StatusKind::Connected => {
                    if let Some(from) = open_disc.remove(&sym) {
                        disconnect
                            .entry(sym)
                            .or_default()
                            .push((from, e.recv_ts_ns));
                    }
                }
                StatusKind::GapDetected => {
                    let kind = if detail.contains("overrun") {
                        GapKind::Overrun
                    } else {
                        GapKind::Venue
                    };
                    open_seq.entry(sym).or_insert((e.recv_ts_ns, kind));
                }
                _ => {}
            },
            MarketEvent::BookSnapshot {
                reason: SnapshotReason::GapResync,
                ..
            } => {
                if let Some((from, kind)) = open_seq.remove(&sym) {
                    seqgap
                        .entry(sym)
                        .or_default()
                        .push((from, e.recv_ts_ns, kind));
                }
                bump(&mut streams, "book_snapshots", sym, e.recv_ts_ns);
            }
            body => bump(
                &mut streams,
                crate::layout::stream_type_name(body),
                sym,
                e.recv_ts_ns,
            ),
        }
    }

    for (sym, from) in open_disc {
        disconnect.entry(sym).or_default().push((from, day_end_ns));
    }
    for (sym, (from, kind)) in open_seq {
        seqgap
            .entry(sym)
            .or_default()
            .push((from, day_end_ns, kind));
    }

    let span = (day_end_ns - day_start_ns).max(1) as f64;
    let mut out: BTreeMap<String, StreamStats> = BTreeMap::new();
    for ((type_name, sym), acc) in streams {
        let name = symbol_name(sym);
        let key = format!("{type_name}:{name}");

        let mut gaps: Vec<Gap> = Vec::new();
        for &(f, t) in disconnect.get(&sym).map(|v| v.as_slice()).unwrap_or(&[]) {
            gaps.push(Gap {
                from_ns: f,
                to_ns: t,
                kind: GapKind::Disconnect,
            });
        }
        if type_name == "book_deltas" {
            for &(f, t, k) in seqgap.get(&sym).map(|v| v.as_slice()).unwrap_or(&[]) {
                gaps.push(Gap {
                    from_ns: f,
                    to_ns: t,
                    kind: k,
                });
            }
        }
        let dur = merged_duration(
            gaps.iter().map(|g| (g.from_ns, g.to_ns)).collect(),
            day_start_ns,
            day_end_ns,
        );
        let coverage = (1.0 - dur as f64 / span).clamp(0.0, 1.0);
        out.insert(
            key,
            StreamStats {
                events: acc.events,
                first_ts_ns: acc.first,
                last_ts_ns: acc.last,
                gaps,
                coverage,
                sampled: false,
            },
        );
    }

    QualityManifest {
        venue: venue.to_owned(),
        date: date.to_owned(),
        schema_ver: mp_core::SCHEMA_VER,
        day_start_ns,
        day_end_ns,
        streams: out,
        compactor_version: compactor_version.to_owned(),
        created_ts_ns,
    }
}

/// Write a manifest as pretty JSON.
pub fn write_manifest(path: &std::path::Path, m: &QualityManifest) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(m).expect("manifest serializes");
    std::fs::write(path, json)
}

/// Read a manifest from JSON.
pub fn read_manifest(path: &std::path::Path) -> std::io::Result<QualityManifest> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}
