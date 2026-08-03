//! Raw-log migration: schema-1 → current schema (2026-08-03 audit fix).
//!
//! W-6 discipline: never rewrite recorded data. This module writes NEW
//! current-schema copies to a separate directory and verifies them; the
//! originals are left untouched for the human to delete only after
//! verification. The read side (`mp_core::log::LogReader`) already decodes
//! schema-1 transparently (with synthetic provenance), so this module exists
//! for consumers that need current-schema files on disk (audit, compaction,
//! replay) without the legacy provenance caveat at read time.

use mp_core::log::{EventLogWriter, LogError, LogReader};
use std::path::Path;

/// Outcome of migrating one raw log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateOutcome {
    /// File was already current schema; nothing written.
    AlreadyCurrent,
    /// File was migrated: `legacy_events` of `events` total were re-encoded
    /// and verified as current schema at `dst`.
    Migrated { events: u64, legacy_events: u64 },
}

/// Migration error.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("log: {0}")]
    Log(#[from] LogError),
    #[error("src and dst resolve to the same path: {0}")]
    SamePath(String),
    #[error("src has {src} events but dst has {dst} after write")]
    VerifyMismatch { src: u64, dst: u64 },
}

/// Migrate `src` (a raw event log) to a current-schema file at `dst`.
///
/// - Reads the whole source once, decoding both schema-1 and schema-2 frames
///   (schema-1 via `LogReader`'s legacy path, provenance synthetic).
/// - Writes a fresh file at `dst` with a symbols frame and all events
///   re-encoded at the current schema version.
/// - Re-reads `dst` and refuses success if the event count diverges.
///
/// If `src` contains no schema-1 events, nothing is written and
/// [`MigrateOutcome::AlreadyCurrent`] is returned.
pub fn migrate_log(src: &Path, dst: &Path) -> Result<MigrateOutcome, MigrateError> {
    // Safety: never allow writing over the source itself (W-6).
    if src.canonicalize().ok() == dst.canonicalize().ok() && src.exists() {
        return Err(MigrateError::SamePath(src.display().to_string()));
    }

    // Pass 1: scan for legacy events; collect the symbol table snapshot.
    let mut legacy_events: u64 = 0;
    let mut events: u64 = 0;
    let symbols = {
        let mut reader = LogReader::open(src)?;
        for ev in reader.by_ref() {
            let ev = ev?;
            if ev.schema_ver != mp_core::SCHEMA_VER {
                legacy_events += 1;
            }
            events += 1;
        }
        reader.symbols().to_vec()
    };

    if legacy_events == 0 {
        return Ok(MigrateOutcome::AlreadyCurrent);
    }

    // Pass 2: write the fresh current-schema file. Remove any stale dst from
    // an earlier partial run first — dst is a generated artifact, never
    // recorded data (W-6 protects the source, not the output copy).
    {
        let _ = std::fs::remove_file(dst);
        let (mut writer, _) = EventLogWriter::open(dst)?;
        if !symbols.is_empty() {
            writer.write_symbols(&symbols)?;
        }
        let mut reader = LogReader::open(src)?;
        for ev in reader.by_ref() {
            let mut ev = ev?;
            // Re-stamp: the writer encodes the envelope's schema_ver field,
            // so force it to the current version (the legacy value would
            // otherwise be persisted into the new file).
            ev.schema_ver = mp_core::SCHEMA_VER;
            writer.append(&ev)?;
        }
        writer.sync()?;
    }

    // Verify: reopen dst and count.
    let dst_events: u64 = LogReader::open(dst)?
        .map(|r| r.map(|_| 1u64).unwrap_or(0))
        .sum();
    if dst_events != events {
        return Err(MigrateError::VerifyMismatch {
            src: events,
            dst: dst_events,
        });
    }

    Ok(MigrateOutcome::Migrated { events, legacy_events })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::event::{MarketEvent, Side};
    use mp_core::log::EnvelopeV1;
    use mp_core::{SymbolId, Venue};

    fn write_v1_log(path: &Path, n: u64) {
        // The current writer stamps SCHEMA_VER=2, so a schema-1 fixture has to
        // be hand-framed: header + FRAME_EVENT (kind=1) frames whose payload
        // is `schema_ver:u16 || bincode(EnvelopeV1)` — the exact pre-2026-08
        // collector byte layout.
        let mut f = std::fs::File::create(path).unwrap();
        use std::io::Write;
        f.write_all(b"MPLOG\0\0\0").unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        for i in 0..n {
            let v1 = EnvelopeV1 {
                schema_ver: 1,
                venue: Venue::Bybit,
                symbol: SymbolId(i as u32),
                exch_ts_ns: i as i64,
                recv_ts_ns: i as i64,
                stream_seq: i,
                body: MarketEvent::Trade {
                    price: 1.0,
                    qty: 2.0,
                    side: Side::Buy,
                    trade_id: i,
                },
            };
            let mut payload = 1u16.to_le_bytes().to_vec();
            payload.extend_from_slice(&bincode::serialize(&v1).unwrap());
            let mut frame = vec![1u8]; // FRAME_EVENT kind
            frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            frame.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
            frame.extend_from_slice(&payload);
            f.write_all(&frame).unwrap();
        }
        f.sync_all().unwrap();
    }

    fn write_v2_log(path: &Path, n: u64) {
        let (mut w, _) = EventLogWriter::open(path).unwrap();
        for i in 0..n {
            w.append(&mp_core::EventEnvelope::new(
                Venue::Bybit,
                SymbolId(1),
                i as i64,
                i as i64,
                i,
                MarketEvent::Trade {
                    price: 1.0,
                    qty: 2.0,
                    side: Side::Buy,
                    trade_id: i,
                },
            ))
            .unwrap();
        }
        w.sync().unwrap();
    }

    /// Unique temp dir per test: the storage suite runs tests in parallel and
    /// a shared `mpmigrate-{pid}` dir with identical filenames would let one
    /// test delete another's fixture mid-flight.
    fn tmp(tag: &str) -> std::path::PathBuf {
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mpmigrate-{}-{tag}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn int_1_migrate_rewrites_v1_log_to_current_schema() {
        let dir = tmp("rewrite");
        let src = dir.join("v1.log");
        let dst = dir.join("v1.migrated.log");
        write_v1_log(&src, 5);

        let outcome = migrate_log(&src, &dst).unwrap();
        assert_eq!(
            outcome,
            MigrateOutcome::Migrated { events: 5, legacy_events: 5 }
        );

        // dst must be current schema and carry all 5 events.
        let got: Vec<_> = LogReader::open(&dst)
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(got.len(), 5);
        for e in &got {
            assert_eq!(e.schema_ver, mp_core::SCHEMA_VER);
        }
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }

    #[test]
    fn int_1_migrate_skips_current_schema() {
        let dir = tmp("skip");
        let src = dir.join("v2.log");
        let dst = dir.join("v2.migrated.log");
        write_v2_log(&src, 3);

        let outcome = migrate_log(&src, &dst).unwrap();
        assert_eq!(outcome, MigrateOutcome::AlreadyCurrent);
        assert!(!dst.exists(), "no file should be written for current schema");
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }

    #[test]
    fn int_1_migrate_refuses_same_path() {
        let dir = tmp("samepath");
        let src = dir.join("v1.log");
        write_v1_log(&src, 2);
        let err = migrate_log(&src, &src).unwrap_err();
        assert!(matches!(err, MigrateError::SamePath(_)), "{err:?}");
        let _ = std::fs::remove_file(&src);
    }
}
