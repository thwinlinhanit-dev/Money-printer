//! Append-only, crash-safe event log (EVT-4, EVT-5, EVT-8).
//!
//! File = `MAGIC || format_ver:u16` header, then a sequence of frames:
//! `kind:u8 || len:u32 || crc32:u32 || payload[len]`.
//! `kind` is [`FRAME_SYMBOLS`] (a [`SymbolMeta`] snapshot) or [`FRAME_EVENT`]
//! (`schema_ver:u16 || bincode(EventEnvelope)`). A torn final frame (short
//! read or CRC mismatch) is detected and truncated on open (EVT-4).

use crate::codec::{self, CodecError};
use crate::event::{EventEnvelope, EventProvenance, MarketEvent, SymbolId, Venue};
use crate::symbol::SymbolMeta;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"MPLOG\0\0\0";
const FORMAT_VER: u16 = 1;
const FRAME_SYMBOLS: u8 = 0;
const FRAME_EVENT: u8 = 1;
const FRAME_HEADER_LEN: u64 = 9; // kind(1) + len(4) + crc(4)
/// Reject absurd frame lengths from corrupt data before allocating (EVT-4).
const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

/// Schema-1 envelope layout (pre-provenance, written before SCHEMA_VER was
/// bumped to 2). Field order is the v1 law and MUST stay as-is: bincode maps
/// struct fields positionally, so this is the only shape that decodes the
/// historical recordings. The 2026-08-03 audit fix added this so the
/// 07-18..07-29 capture becomes readable again instead of being quarantined
/// behind a strict `schema_ver != SCHEMA_VER` reject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvelopeV1 {
    pub schema_ver: u16,
    pub venue: Venue,
    pub symbol: SymbolId,
    pub exch_ts_ns: i64,
    pub recv_ts_ns: i64,
    pub stream_seq: u64,
    pub body: MarketEvent,
}

/// Event-log errors.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("codec: {0}")]
    Codec(#[from] CodecError),
    #[error("bad magic: not an mp event log")]
    BadMagic,
    #[error("unsupported format version {0}")]
    BadFormat(u16),
    #[error("unsupported event schema version {0}")]
    UnsupportedSchema(u16),
    #[error("unknown frame kind {0}")]
    BadFrameKind(u8),
    #[error("symbol id {0} has no metadata (corrupt log?)")]
    CorruptSymbol(u32),
}

// ---- frame primitives -------------------------------------------------------

enum FrameRead {
    Frame {
        kind: u8,
        payload: Vec<u8>,
    },
    /// Clean end of file at a frame boundary.
    Eof,
    /// Truncated or corrupt trailing frame — everything from here is discarded.
    Torn,
}

fn read_frame<R: Read>(r: &mut R) -> io::Result<FrameRead> {
    let mut hdr = [0u8; FRAME_HEADER_LEN as usize];
    match read_full_or_eof(r, &mut hdr)? {
        ReadState::Eof => return Ok(FrameRead::Eof),
        ReadState::Partial => return Ok(FrameRead::Torn),
        ReadState::Full => {}
    }
    let kind = hdr[0];
    let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
    let crc = u32::from_le_bytes([hdr[5], hdr[6], hdr[7], hdr[8]]);
    if len > MAX_FRAME_LEN {
        return Ok(FrameRead::Torn);
    }
    let mut payload = vec![0u8; len as usize];
    match read_full_or_eof(r, &mut payload)? {
        ReadState::Full => {}
        _ => return Ok(FrameRead::Torn),
    }
    if crc32fast::hash(&payload) != crc {
        return Ok(FrameRead::Torn);
    }
    Ok(FrameRead::Frame { kind, payload })
}

enum ReadState {
    Full,
    Partial,
    Eof,
}

fn read_full_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<ReadState> {
    let mut read = 0;
    while read < buf.len() {
        match r.read(&mut buf[read..]) {
            Ok(0) => {
                return Ok(if read == 0 {
                    ReadState::Eof
                } else {
                    ReadState::Partial
                });
            }
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(ReadState::Full)
}

fn encode_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN as usize + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Scan a log file and return the byte length up to and including the last
/// fully valid frame (EVT-4). A fresh/short file returns just the header len;
/// an invalid header returns an error.
pub fn scan_valid_len(path: &Path) -> Result<u64, LogError> {
    let mut f = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 8];
    match read_full_or_eof(&mut f, &mut magic)? {
        ReadState::Full => {}
        _ => return Ok(0), // empty/short: no valid header yet
    }
    if &magic != MAGIC {
        return Err(LogError::BadMagic);
    }
    let mut fver = [0u8; 2];
    if !matches!(read_full_or_eof(&mut f, &mut fver)?, ReadState::Full) {
        return Ok(0);
    }
    let fver = u16::from_le_bytes(fver);
    if fver != FORMAT_VER {
        return Err(LogError::BadFormat(fver));
    }
    let mut valid = MAGIC.len() as u64 + 2;
    while let FrameRead::Frame { payload, .. } = read_frame(&mut f)? {
        valid += FRAME_HEADER_LEN + payload.len() as u64;
    }
    Ok(valid)
}

// ---- fsync policy (spec 014) -------------------------------------------------

/// Configurable fsync policy for the event log (FSP-1).
#[derive(Debug, Clone, Copy)]
pub struct FsyncPolicy {
    /// Fsync every N events (0 = disabled).
    pub every_n_events: u64,
    /// Fsync every N nanoseconds (0 = disabled).
    pub every_ns: i64,
    /// Fsync on graceful shutdown (SIGTERM).
    pub on_sigterm: bool,
}

impl Default for FsyncPolicy {
    fn default() -> Self {
        Self {
            every_n_events: 1000,
            every_ns: 10_000_000_000, // 10s
            on_sigterm: true,
        }
    }
}

// ---- writer -----------------------------------------------------------------

/// Append-only event-log writer (EVT-4). Recovers a torn tail on open.
pub struct EventLogWriter {
    file: BufWriter<File>,
    fsync_policy: FsyncPolicy,
    events_since_fsync: u64,
    last_fsync_ns: i64,
}

impl EventLogWriter {
    /// Open `path` for appending. A "new" file means *no valid header*, not
    /// merely "inode exists" (audit M2: a crash between creation and header
    /// flush previously left a permanently unreadable log because `exists()`
    /// was true but MAGIC was never written).
    pub fn open(path: &Path) -> Result<(Self, bool), LogError> {
        let exists = path.exists();
        let mut truncated = false;
        let mut has_header = false;
        if exists {
            let valid = scan_valid_len(path)?;
            let actual = std::fs::metadata(path)?.len();
            // A valid log begins with MAGIC(8) + FORMAT_VER:u16(2) = 10 bytes.
            // scan_valid_len returns exactly MAGIC.len() + 2 for a header-only
            // file (audit C-1: this used to compare against MAGIC.len() + 4, so
            // a header-only file was judged headerless and a second header was
            // appended; the next session's scan then misparsed that second
            // header as a frame and truncated the whole log).
            let header_len = (MAGIC.len() + 2) as u64;
            has_header = valid >= header_len;
            if valid < actual {
                let f = OpenOptions::new().write(true).open(path)?;
                f.set_len(valid)?;
                f.sync_all()?;
                truncated = true;
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        if !has_header {
            // Write (or re-write after truncation-to-zero) the header so any
            // subsequent appends produce a readable log.
            file.write_all(MAGIC)?;
            file.write_all(&FORMAT_VER.to_le_bytes())?;
            // Belt-and-braces (EVT-4): a crash right after creation could
            // otherwise leave an empty file. Readers self-heal that case via
            // scan_valid_len, but fsync makes the header durable as soon as
            // it exists.
            file.sync_data()?;
        }
        Ok((
            Self {
                file: BufWriter::new(file),
                fsync_policy: FsyncPolicy::default(),
                events_since_fsync: 0,
                last_fsync_ns: 0,
            },
            truncated,
        ))
    }

    /// Set the fsync policy (spec 014). Must be called before appending events.
    pub fn set_fsync_policy(&mut self, policy: FsyncPolicy) {
        self.fsync_policy = policy;
    }

    /// Persist a symbol-table snapshot (EVT-8). Write this before the events
    /// that reference newly-interned ids.
    pub fn write_symbols(&mut self, metas: &[SymbolMeta]) -> Result<(), LogError> {
        let payload = codec::encode_symbols(metas)?;
        self.file
            .write_all(&encode_frame(FRAME_SYMBOLS, &payload))?;
        Ok(())
    }

    /// Append one event. Auto-fsyncs if policy thresholds are crossed (FSP-3).
    pub fn append(&mut self, e: &EventEnvelope) -> Result<(), LogError> {
        let mut payload = e.schema_ver.to_le_bytes().to_vec();
        payload.extend_from_slice(&codec::encode_event(e)?);
        self.file.write_all(&encode_frame(FRAME_EVENT, &payload))?;

        self.events_since_fsync += 1;
        let p = self.fsync_policy;
        let should_fsync = p.every_n_events > 0 && self.events_since_fsync >= p.every_n_events;
        // FSYNC cadence is paced on *event* time (`recv_ts_ns`), not the OS
        // wall clock — PD-3 / CONV-5: no wall-clock reads on the decision path.
        let now = e.recv_ts_ns;
        let should_fsync_time = p.every_ns > 0
            && self.last_fsync_ns > 0
            && now.saturating_sub(self.last_fsync_ns) >= p.every_ns;
        if should_fsync || should_fsync_time {
            self.file.flush()?;
            // Use sync_data (faster) — metadata sync not needed for append-only (FSP-5).
            self.file.get_ref().sync_data()?;
            self.events_since_fsync = 0;
            self.last_fsync_ns = now;
        }
        Ok(())
    }

    /// Flush userspace buffers to the OS.
    pub fn flush(&mut self) -> Result<(), LogError> {
        self.file.flush()?;
        Ok(())
    }

    /// Flush + fsync the final buffered events on graceful shutdown (FSP-4 /
    /// COL-19). Honours `FsyncPolicy::on_sigterm`: when false, only the
    /// userspace flush runs — the operator opted out of shutdown durability.
    pub fn sync_on_shutdown(&mut self) -> Result<(), LogError> {
        self.file.flush()?;
        if self.fsync_policy.on_sigterm {
            self.file.get_ref().sync_data()?;
            self.events_since_fsync = 0;
        }
        Ok(())
    }

    /// Flush and fsync with sync_data (fast path, FSP-5).
    pub fn sync(&mut self) -> Result<(), LogError> {
        self.file.flush()?;
        self.file.get_ref().sync_data()?;
        Ok(())
    }

    /// Flush and fsync fully (sync_all — slower, includes metadata).
    pub fn sync_all(&mut self) -> Result<(), LogError> {
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(())
    }
}

// ---- reader -----------------------------------------------------------------

/// Streams events from a single log file in write (recv) order, reconstructing
/// the symbol table from [`FRAME_SYMBOLS`] frames as it goes (EVT-8).
pub struct LogReader {
    reader: BufReader<File>,
    symbols: Vec<SymbolMeta>,
    done: bool,
    /// A raw event payload buffered by [`LogReader::load_symbols`] (the first
    /// event behind the header) so peeking the symbol table never loses events.
    peeked: Option<Vec<u8>>,
}

impl LogReader {
    pub fn open(path: &Path) -> Result<Self, LogError> {
        let mut reader = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(LogError::BadMagic);
        }
        let mut fver = [0u8; 2];
        reader.read_exact(&mut fver)?;
        let fver = u16::from_le_bytes(fver);
        if fver != FORMAT_VER {
            return Err(LogError::BadFormat(fver));
        }
        Ok(Self {
            reader,
            symbols: Vec::new(),
            done: false,
            peeked: None,
        })
    }

    /// Symbol metadata seen so far (grows as symbol frames are read).
    pub fn symbols(&self) -> &[SymbolMeta] {
        &self.symbols
    }

    /// Read frames until the symbol table is loaded — or until the first
    /// event / end-of-log, whichever comes first — buffering any event frame
    /// so it is not lost. The writer emits the symbol frame before any event
    /// frame, so this costs one header read and lets a caller resolve symbols
    /// BEFORE streaming events (the streaming merge's EVT-8 remap needs the
    /// shared table up front). Symbol-less logs (whale census, GapDetected
    /// statuses) stop at the first event with an empty table — the caller's
    /// symbol-less rule applies.
    pub fn load_symbols(&mut self) -> Result<(), LogError> {
        while !self.done && self.peeked.is_none() && self.symbols.is_empty() {
            match read_frame(&mut self.reader)? {
                FrameRead::Eof | FrameRead::Torn => {
                    self.done = true;
                    return Ok(());
                }
                FrameRead::Frame { kind, payload } => match kind {
                    FRAME_SYMBOLS => {
                        self.symbols = codec::decode_symbols(&payload)?;
                        return Ok(());
                    }
                    FRAME_EVENT => {
                        self.peeked = Some(payload);
                        return Ok(());
                    }
                    _ => continue,
                },
            }
        }
        Ok(())
    }

    /// Decode one event frame payload (`schema_ver:u16 || bincode(envelope)`)
    /// into the current envelope. `None` = malformed frame (payload < 2
    /// bytes) — the caller treats it as the log's end.
    fn decode_event_payload(payload: Vec<u8>) -> Result<Option<EventEnvelope>, LogError> {
        if payload.len() < 2 {
            return Ok(None);
        }
        let schema_ver = u16::from_le_bytes([payload[0], payload[1]]);
        let e = match schema_ver {
            crate::SCHEMA_VER => codec::decode_event(&payload[2..])?,
            // Schema-2: byte-identical envelope layout to the
            // current one (provenance included). The 2→3 bump
            // (specs 028/030/031) only *appended* enum variants
            // to `MarketEvent`/`Venue`/`InstrumentKind`, which
            // bincode maps by index, so old frames decode with
            // the current types — no shape change, no legacy
            // struct needed. Historical schema-2 recordings
            // stay readable and promotable (W-6 / CONV-20).
            2 => codec::decode_event(&payload[2..])?,
            // Schema-3: byte-identical envelope layout to the
            // current one. The 3→4 bump (specs 033/034) only
            // *appended* enum variants to `MarketEvent`/`Venue`,
            // which bincode maps by index, so old frames decode
            // with the current types — no shape change, no
            // legacy struct needed. Historical schema-3
            // recordings (2026-08-05..08-18) stay readable and
            // promotable (W-6 / CONV-20).
            3 => codec::decode_event(&payload[2..])?,
            // Schema-4: byte-identical envelope layout to the
            // current one. The 4→5 bump (spec 040, 2026-08-22)
            // appended `Venue::Cboe`/`InstrumentKind::Option` —
            // no `MarketEvent` change — so old frames decode
            // with the current types. Historical schema-4
            // recordings (2026-08-18..08-22) stay readable and
            // promotable (W-6 / CONV-20). (INCIDENT-2026-08-22
            // lesson: this arm was missing after the 5 bump,
            // blinding the gate to every VPS recording.)
            4 => codec::decode_event(&payload[2..])?,
            // Schema-5: byte-identical envelope layout to the
            // current one. The 5→6 bump (spec 040) appended
            // `Venue::DeFiLlama`/`Venue::Coinalyze` — no
            // `MarketEvent` change — so old frames decode with
            // the current types. Historical schema-5
            // recordings stay readable and promotable (W-6 /
            // CONV-20).
            5 => codec::decode_event(&payload[2..])?,
            // Schema-1 (pre-provenance): decode the historical
            // layout and normalize to the current envelope with
            // synthetic provenance. All market data is
            // preserved; the audit layer flags the synthetic
            // provenance as `missing_provenance` (INT-1), so
            // legacy recordings are readable for research but
            // never promoted to cold as live-attributable.
            1 => {
                let legacy: EnvelopeV1 = codec::decode_envelope_v1(&payload[2..])?;
                EventEnvelope {
                    schema_ver: legacy.schema_ver,
                    venue: legacy.venue,
                    symbol: legacy.symbol,
                    exch_ts_ns: legacy.exch_ts_ns,
                    recv_ts_ns: legacy.recv_ts_ns,
                    stream_seq: legacy.stream_seq,
                    provenance: EventProvenance::synthetic(),
                    body: legacy.body,
                }
            }
            other => return Err(LogError::UnsupportedSchema(other)),
        };
        Ok(Some(e))
    }

    fn next_event(&mut self) -> Result<Option<EventEnvelope>, LogError> {
        if let Some(payload) = self.peeked.take() {
            return match Self::decode_event_payload(payload)? {
                Some(ev) => Ok(Some(ev)),
                None => {
                    self.done = true;
                    Ok(None)
                }
            };
        }
        loop {
            if self.done {
                return Ok(None);
            }
            match read_frame(&mut self.reader)? {
                FrameRead::Eof => {
                    self.done = true;
                    return Ok(None);
                }
                FrameRead::Torn => {
                    // EVT-4: a torn tail means the writer crashed mid-frame.
                    // Distinguish it from a clean EOF so replay isn't fooled
                    // into thinking the capture simply ended. WARN per spec.
                    tracing::warn!(
                        "event log ended with a torn tail; trailing partial frame discarded"
                    );
                    self.done = true;
                    return Ok(None);
                }
                FrameRead::Frame { kind, payload } => match kind {
                    FRAME_SYMBOLS => {
                        self.symbols = codec::decode_symbols(&payload)?;
                        continue;
                    }
                    FRAME_EVENT => match Self::decode_event_payload(payload)? {
                        Some(ev) => return Ok(Some(ev)),
                        None => {
                            self.done = true;
                            return Ok(None);
                        }
                    },
                    other => return Err(LogError::BadFrameKind(other)),
                },
            }
        }
    }
}

impl Iterator for LogReader {
    type Item = Result<EventEnvelope, LogError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_event().transpose()
    }
}

// ---- k-way merge (EVT-5) ----------------------------------------------------

struct HeapItem {
    key: (i64, u64),
    src: usize,
    ev: EventEnvelope,
}
impl PartialEq for HeapItem {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key && self.src == o.src
    }
}
impl Eq for HeapItem {}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for HeapItem {
    fn cmp(&self, o: &Self) -> Ordering {
        // Reverse for a min-heap on (key, src); src breaks ties deterministically.
        o.key.cmp(&self.key).then(o.src.cmp(&self.src))
    }
}

/// Pure k-way merge of already-sorted, infallible event streams by
/// `(recv_ts_ns, stream_seq)` with source-index tiebreak (EVT-5 / STO-4).
/// Prefer this over reimplementing a heap in higher crates.
pub fn merge_sorted_events(
    mut sources: Vec<std::vec::IntoIter<EventEnvelope>>,
) -> Vec<EventEnvelope> {
    let mut heap = BinaryHeap::new();
    for (src, it) in sources.iter_mut().enumerate() {
        if let Some(ev) = it.next() {
            let key = ev.merge_key();
            heap.push(HeapItem { key, src, ev });
        }
    }
    let mut out = Vec::new();
    while let Some(item) = heap.pop() {
        if let Some(ev) = sources[item.src].next() {
            let key = ev.merge_key();
            heap.push(HeapItem {
                key,
                src: item.src,
                ev,
            });
        }
        out.push(item.ev);
    }
    out
}

/// Merges several event iterators into one globally ordered stream by
/// `(recv_ts_ns, stream_seq)`, ties broken by source index (EVT-5). Each input
/// must already be sorted (a single venue/day log is, by construction).
pub struct MergeReader<I: Iterator<Item = Result<EventEnvelope, LogError>>> {
    sources: Vec<I>,
    heap: BinaryHeap<HeapItem>,
    primed: bool,
}

impl<I: Iterator<Item = Result<EventEnvelope, LogError>>> MergeReader<I> {
    pub fn new(sources: Vec<I>) -> Self {
        Self {
            sources,
            heap: BinaryHeap::new(),
            primed: false,
        }
    }

    fn pull(&mut self, src: usize) -> Result<(), LogError> {
        if let Some(next) = self.sources[src].next() {
            let ev = next?;
            let key = ev.merge_key();
            self.heap.push(HeapItem { key, src, ev });
        }
        Ok(())
    }

    fn prime(&mut self) -> Result<(), LogError> {
        for src in 0..self.sources.len() {
            self.pull(src)?;
        }
        self.primed = true;
        Ok(())
    }

    fn next_merged(&mut self) -> Result<Option<EventEnvelope>, LogError> {
        if !self.primed {
            self.prime()?;
        }
        let Some(item) = self.heap.pop() else {
            return Ok(None);
        };
        self.pull(item.src)?;
        Ok(Some(item.ev))
    }
}

impl<I: Iterator<Item = Result<EventEnvelope, LogError>>> Iterator for MergeReader<I> {
    type Item = Result<EventEnvelope, LogError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_merged().transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{MarketEvent, Side, SnapshotSource};
    use std::io::Write;

    /// Write a schema-1 event log by hand: MAGIC + FORMAT_VER header, then
    /// one event frame whose payload is `schema_ver:u16 || bincode(EnvelopeV1)`
    /// — exactly the byte layout the pre-provenance collector produced.
    fn write_v1_log(path: &std::path::Path, events: &[EnvelopeV1]) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(MAGIC).unwrap();
        f.write_all(&FORMAT_VER.to_le_bytes()).unwrap();
        for e in events {
            let mut payload = e.schema_ver.to_le_bytes().to_vec();
            payload.extend_from_slice(&codec::encode_envelope_v1(e).unwrap());
            f.write_all(&encode_frame(FRAME_EVENT, &payload)).unwrap();
        }
        f.sync_all().unwrap();
    }

    fn v1_trade(recv_ts_ns: i64, stream_seq: u64) -> EnvelopeV1 {
        EnvelopeV1 {
            schema_ver: 1,
            venue: Venue::BinanceFutures,
            symbol: SymbolId(7),
            exch_ts_ns: recv_ts_ns - 1,
            recv_ts_ns,
            stream_seq,
            body: MarketEvent::Trade {
                price: 61_000.5,
                qty: 0.25,
                side: Side::Buy,
                trade_id: stream_seq,
            },
        }
    }

    #[test]
    fn conv_20_schema_1_log_decodes_with_synthetic_provenance() {
        // Audit 2026-08-03: schema-1 recordings must become readable again
        // instead of being rejected by the strict schema_ver gate.
        let dir = std::env::temp_dir().join(format!("mplog-v1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("schema1.log");
        let _ = std::fs::remove_file(&path);

        write_v1_log(&path, &[v1_trade(100, 1), v1_trade(200, 2)]);

        let got: Vec<_> = LogReader::open(&path)
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(got.len(), 2, "both v1 events must decode");
        let e = &got[0];
        assert_eq!(e.recv_ts_ns, 100);
        assert_eq!(e.stream_seq, 1);
        assert_eq!(e.symbol, SymbolId(7));
        assert_eq!(e.venue, Venue::BinanceFutures);
        // Provenance is synthetic: empty stream/subscription, no snapshot
        // source — the audit layer flags this as missing_provenance (INT-1).
        assert_eq!(e.provenance, EventProvenance::synthetic());
        assert_eq!(e.provenance.snapshot_source, SnapshotSource::None);
        match &e.body {
            MarketEvent::Trade {
                price, qty, side, ..
            } => {
                assert_eq!(*price, 61_000.5);
                assert_eq!(*qty, 0.25);
                assert_eq!(*side, Side::Buy);
            }
            other => panic!("wrong variant: {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn conv_20_schema_2_log_still_reads_normally() {
        // Guard: the compat path must not disturb current-schema reads.
        let dir = std::env::temp_dir().join(format!("mplog-v2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("schema2.log");
        let _ = std::fs::remove_file(&path);

        let (mut w, _) = EventLogWriter::open(&path).unwrap();
        w.append(&EventEnvelope::new(
            Venue::Bybit,
            SymbolId(1),
            10,
            20,
            3,
            MarketEvent::Trade {
                price: 1.0,
                qty: 2.0,
                side: Side::Sell,
                trade_id: 9,
            },
        ))
        .unwrap();
        w.sync().unwrap();
        drop(w);

        let got: Vec<_> = LogReader::open(&path)
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].schema_ver, crate::SCHEMA_VER);
        assert_eq!(got[0].recv_ts_ns, 20);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn conv_20_schema_2_log_still_reads_with_appended_variants() {
        // Spec 001 amendment (specs 028/030/031): the 2→3 bump appended enum
        // variants only (MarketEvent, Venue, InstrumentKind), which bincode
        // maps by index — so a schema-2 frame (current envelope layout with
        // provenance) must decode cleanly with the current types. Historical
        // schema-2 recordings stay readable and promotable (W-6).
        let dir = std::env::temp_dir().join(format!("mplog-v2b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("schema2b.log");
        let _ = std::fs::remove_file(&path);

        let (mut w, _) = EventLogWriter::open(&path).unwrap();
        let mut legacy = EventEnvelope::new(
            Venue::Bybit,
            SymbolId(1),
            10,
            20,
            3,
            MarketEvent::Trade {
                price: 1.0,
                qty: 2.0,
                side: Side::Sell,
                trade_id: 9,
            },
        );
        // Simulate a frame written before the 028/030/031 amendment.
        legacy.schema_ver = 2;
        w.append(&legacy).unwrap();
        w.sync().unwrap();
        drop(w);

        let got: Vec<_> = LogReader::open(&path)
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].schema_ver, 2);
        assert_eq!(got[0].recv_ts_ns, 20);
        assert_eq!(got[0].stream_seq, 3);
        // Schema-2 envelopes already carried provenance — it must survive.
        assert_eq!(got[0].provenance, EventProvenance::synthetic());
        match &got[0].body {
            MarketEvent::Trade { price, side, .. } => {
                assert_eq!(*price, 1.0);
                assert_eq!(*side, Side::Sell);
            }
            other => panic!("wrong variant: {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    // ---- audit C-1 regression: header_len off-by-two -------------------------

    fn demo_event(seq: u64) -> EventEnvelope {
        EventEnvelope::new(
            Venue::Bybit,
            SymbolId(1),
            10,
            20 + seq as i64,
            seq,
            MarketEvent::Trade {
                price: 1.0,
                qty: 2.0,
                side: Side::Buy,
                trade_id: seq,
            },
        )
    }

    /// C-1: a session that opened the log (header written) but crashed before
    /// writing any frame must be recognized as *having* a header on reopen.
    /// Regression: `header_len = MAGIC.len() + 4` judged a 10-byte header-only
    /// file headerless, so the next open appended a second header; the scan in
    /// the session after that misparsed it as a frame (len ≈ 1.19 GB > 
    /// MAX_FRAME_LEN → Torn → valid = 10) and truncated the entire log.
    #[test]
    fn regression_audit28_c1_header_only_log_not_reheadered_or_truncated() {
        let dir = std::env::temp_dir().join(format!("mplog-c1a-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c1a.log");
        let _ = std::fs::remove_file(&path);

        // Session 1: open (writes the 10-byte header), write nothing, drop.
        let (w, truncated) = EventLogWriter::open(&path).unwrap();
        assert!(!truncated);
        drop(w);
        let header_len = (MAGIC.len() + 2) as u64;
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            header_len,
            "session 1 must leave exactly the header"
        );

        // Session 2: reopen — no duplicate header may be appended.
        let (mut w, truncated) = EventLogWriter::open(&path).unwrap();
        assert!(!truncated, "header-only log must not be truncated on reopen");
        w.append(&demo_event(0)).unwrap();
        w.append(&demo_event(1)).unwrap();
        w.sync().unwrap();
        drop(w);

        // Session 3: reopen again — all frames readable, nothing truncated,
        // exactly one header (scan_valid_len == actual file length).
        let (w, truncated) = EventLogWriter::open(&path).unwrap();
        assert!(!truncated, "valid log must never be truncated on reopen");
        drop(w);
        let valid = scan_valid_len(&path).unwrap();
        let actual = std::fs::metadata(&path).unwrap().len();
        assert_eq!(valid, actual, "scan must validate the whole file");
        assert!(
            actual > header_len,
            "second header must not have been appended"
        );
        let got: Vec<_> = LogReader::open(&path)
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(got.len(), 2, "both events must survive all three sessions");
        assert_eq!(got[0].stream_seq, 0);
        assert_eq!(got[1].stream_seq, 1);
        let _ = std::fs::remove_file(&path);
    }

    /// C-1 companion: a normal header + frames log reopened must not be
    /// truncated and must keep every frame (valid == actual length).
    #[test]
    fn regression_audit28_c1_log_with_frames_reopen_not_truncated() {
        let dir = std::env::temp_dir().join(format!("mplog-c1b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c1b.log");
        let _ = std::fs::remove_file(&path);

        const N: u64 = 5;
        let (mut w, truncated) = EventLogWriter::open(&path).unwrap();
        assert!(!truncated);
        for i in 0..N {
            w.append(&demo_event(i)).unwrap();
        }
        w.sync().unwrap();
        drop(w);

        let (w, truncated) = EventLogWriter::open(&path).unwrap();
        assert!(!truncated, "well-formed log must not be truncated on reopen");
        drop(w);
        let valid = scan_valid_len(&path).unwrap();
        let actual = std::fs::metadata(&path).unwrap().len();
        assert_eq!(valid, actual, "valid must equal actual length");
        let got: Vec<_> = LogReader::open(&path)
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(got.len() as u64, N);
        for (i, e) in got.iter().enumerate() {
            assert_eq!(e.stream_seq, i as u64);
        }
        let _ = std::fs::remove_file(&path);
    }
}
