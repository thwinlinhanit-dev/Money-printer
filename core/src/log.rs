//! Append-only, crash-safe event log (EVT-4, EVT-5, EVT-8).
//!
//! File = `MAGIC || format_ver:u16` header, then a sequence of frames:
//! `kind:u8 || len:u32 || crc32:u32 || payload[len]`.
//! `kind` is [`FRAME_SYMBOLS`] (a [`SymbolMeta`] snapshot) or [`FRAME_EVENT`]
//! (`schema_ver:u16 || bincode(EventEnvelope)`). A torn final frame (short
//! read or CRC mismatch) is detected and truncated on open (EVT-4).

use crate::codec::{self, CodecError};
use crate::event::EventEnvelope;
use crate::symbol::SymbolMeta;
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
    #[error("unknown frame kind {0}")]
    BadFrameKind(u8),
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

// ---- writer -----------------------------------------------------------------

/// Append-only event-log writer (EVT-4). Recovers a torn tail on open.
pub struct EventLogWriter {
    file: BufWriter<File>,
}

impl EventLogWriter {
    /// Open `path` for appending, creating it with a header if new and
    /// truncating any torn trailing frame if it already exists. Returns the
    /// writer and whether a truncation occurred (WARN-worthy, EVT-4).
    pub fn open(path: &Path) -> Result<(Self, bool), LogError> {
        let exists = path.exists();
        let mut truncated = false;
        if exists {
            let valid = scan_valid_len(path)?;
            let actual = std::fs::metadata(path)?.len();
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
        if !exists {
            file.write_all(MAGIC)?;
            file.write_all(&FORMAT_VER.to_le_bytes())?;
        }
        Ok((
            Self {
                file: BufWriter::new(file),
            },
            truncated,
        ))
    }

    /// Persist a symbol-table snapshot (EVT-8). Write this before the events
    /// that reference newly-interned ids.
    pub fn write_symbols(&mut self, metas: &[SymbolMeta]) -> Result<(), LogError> {
        let payload = codec::encode_symbols(metas)?;
        self.file
            .write_all(&encode_frame(FRAME_SYMBOLS, &payload))?;
        Ok(())
    }

    /// Append one event.
    pub fn append(&mut self, e: &EventEnvelope) -> Result<(), LogError> {
        let mut payload = e.schema_ver.to_le_bytes().to_vec();
        payload.extend_from_slice(&codec::encode_event(e)?);
        self.file.write_all(&encode_frame(FRAME_EVENT, &payload))?;
        Ok(())
    }

    /// Flush userspace buffers to the OS.
    pub fn flush(&mut self) -> Result<(), LogError> {
        self.file.flush()?;
        Ok(())
    }

    /// Flush and fsync to durable storage.
    pub fn sync(&mut self) -> Result<(), LogError> {
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
        })
    }

    /// Symbol metadata seen so far (grows as symbol frames are read).
    pub fn symbols(&self) -> &[SymbolMeta] {
        &self.symbols
    }

    fn next_event(&mut self) -> Result<Option<EventEnvelope>, LogError> {
        loop {
            if self.done {
                return Ok(None);
            }
            match read_frame(&mut self.reader)? {
                FrameRead::Eof | FrameRead::Torn => {
                    self.done = true;
                    return Ok(None);
                }
                FrameRead::Frame { kind, payload } => match kind {
                    FRAME_SYMBOLS => {
                        self.symbols = codec::decode_symbols(&payload)?;
                        continue;
                    }
                    FRAME_EVENT => {
                        // payload = schema_ver:u16 || bincode(envelope)
                        if payload.len() < 2 {
                            self.done = true;
                            return Ok(None);
                        }
                        let e = codec::decode_event(&payload[2..])?;
                        return Ok(Some(e));
                    }
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
