//! Multi-symbol collector fan-out (spec 032, MSC-1..10).
//!
//! **UNWIRED — DEAD CODE (A-4, audit 2026-09-02).** No binary references this
//! module: `mp-collector` (and every other bin) drives the single-symbol
//! `Collector` path only. Spec 032 (MSC-1..10) is PENDING — nothing here runs
//! in production, and its presence must NOT be read as "multi-symbol
//! collection is implemented". Kept as an explicit, unit-tested scaffold for
//! the future spec-032 slice; wire it through a binary before relying on it.
//!
//! Deployment grouping: ONE `mp-collector` process may own N symbols of one
//! venue while every existing consumer keeps its per-symbol contract — one
//! daily log `{yyyymmdd}_{venue}_{symbol}.log`, one exclusive
//! `.lock_{venue}_{symbol}`, one `mp-collector-{venue}-{symbol}.heartbeat`,
//! one gate/scorecard/drain line per symbol. The fan-out is a deployment
//! grouping, NOT a change to the recorded == required contract (MSC-8).
//!
//! This module owns the testable core mechanics:
//! - [`resolve_symbols`] — `--symbol` vs `--symbols` mutual exclusion (MSC-1);
//! - [`acquire_locks_all_or_nothing`] — per-symbol exclusive locks (MSC-2);
//! - [`WriterTable`] — per-symbol log rotation (MSC-3), recv monotonicity per
//!   log (MSC-4), `write_symbols` into every owned log (MSC-7), venue-level
//!   `Status` fan-out to every owned symbol log (MSC-10);
//! - [`bybit_frames_for_symbols`] — N-topic subscribe frames under the venue's
//!   per-frame topic cap (MSC-5 building block).
//!
//! Pure std + the event-log writer — no network, no feature gates, so the
//! full fan-out mechanics are unit-testable offline (CONV-23).

use crate::binutil::InstanceLock;
use mp_core::log::{EventLogWriter, FsyncPolicy, LogError};
use mp_core::{EventEnvelope, MarketEvent, SymbolMeta, Venue};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Resolve the fan-out symbol list from the mutually-exclusive config forms
/// (MSC-1). `symbol` is the legacy single-symbol form; `symbols` is the list
/// form. Setting both is ambiguous and rejected; setting neither returns the
/// historical default `["BTCUSDT"]` (the pre-fan-out flag-path default), so
/// `--symbols` is purely additive.
pub fn resolve_symbols(
    symbol: Option<String>,
    symbols: Option<Vec<String>>,
) -> Result<Vec<String>, String> {
    match (symbol, symbols) {
        (Some(_), Some(_)) => Err(
            "`symbol` and `symbols` are mutually exclusive (spec 032 MSC-1): use one or the other"
                .into(),
        ),
        (Some(s), None) => {
            if s.is_empty() {
                return Err("symbol must be non-empty (MSC-1)".into());
            }
            Ok(vec![s])
        }
        (None, Some(list)) => {
            let mut out: Vec<String> = Vec::new();
            for s in list {
                let t = s.trim();
                if t.is_empty() {
                    continue;
                }
                if !out.iter().any(|x| x == t) {
                    out.push(t.to_owned());
                }
            }
            if out.is_empty() {
                return Err(
                    "symbols list must contain at least one non-empty symbol (MSC-1)".into(),
                );
            }
            Ok(out)
        }
        (None, None) => Ok(vec!["BTCUSDT".to_owned()]),
    }
}

/// Acquire one exclusive lock per symbol, all-or-nothing (MSC-2): if ANY
/// lock is held, release the locks already taken and error — a partial
/// fan-out that silently owns half the set is worse than no fan-out. Lock
/// names keep the existing shape `.lock_{venue}_{symbol}` (binutil contract,
/// audit 08-04: delete a stale lock only if sure).
pub fn acquire_locks_all_or_nothing(
    raw_dir: &Path,
    venue: &str,
    symbols: &[String],
) -> std::io::Result<Vec<InstanceLock>> {
    let mut held: Vec<InstanceLock> = Vec::with_capacity(symbols.len());
    for s in symbols {
        match InstanceLock::acquire(raw_dir, &format!("{venue}_{s}")) {
            Ok(lock) => held.push(lock),
            Err(e) => {
                // Dropping the already-held locks releases their exclusive
                // handles and removes the lock files (InstanceLock::drop) —
                // no partial ownership left behind.
                drop(held);
                return Err(e);
            }
        }
    }
    Ok(held)
}

/// Bybit v5 subscribe frames for N symbols (MSC-5 building block). Bybit caps
/// one subscribe request at 10 args; the full topic set is 4 args per symbol
/// (`publicTrade.`, `orderbook.50.`, `tickers.`, `allLiquidation.` — COL-29
/// uses the live `allLiquidation.` topic, never the dead `liquidation.` one),
/// and the swing-only set is 3 (spec 035 SWG-1 drops `orderbook.50.`). Frames
/// are chunked so no frame exceeds the cap. Single-symbol output is
/// byte-identical to the pre-fan-out builder (`subscribe_for`).
pub fn bybit_frames_for_symbols(symbols: &[String], swing_only: bool) -> Vec<String> {
    const TOPIC_CAP_PER_FRAME: usize = 10;
    const TOPICS_PER_SYMBOL_FULL: usize = 4;
    const TOPICS_PER_SYMBOL_SWING: usize = 3;
    let per_symbol = if swing_only {
        TOPICS_PER_SYMBOL_SWING
    } else {
        TOPICS_PER_SYMBOL_FULL
    };
    let syms_per_frame = (TOPIC_CAP_PER_FRAME / per_symbol).max(1);
    let mut frames = Vec::new();
    for chunk in symbols.chunks(syms_per_frame) {
        let mut args: Vec<String> = Vec::with_capacity(chunk.len() * per_symbol);
        for s in chunk {
            if swing_only {
                args.push(format!("publicTrade.{s}"));
                args.push(format!("tickers.{s}"));
                args.push(format!("allLiquidation.{s}"));
            } else {
                args.push(format!("publicTrade.{s}"));
                args.push(format!("orderbook.50.{s}"));
                args.push(format!("tickers.{s}"));
                args.push(format!("allLiquidation.{s}"));
            }
        }
        frames.push(format!(
            r#"{{"op":"subscribe","args":{}}}"#,
            serde_json::json!(args)
        ));
    }
    frames
}
/// One owned symbol's log slot: its own writer, rotation date, symbol-table
/// count and recv clock (MSC-3/MSC-4) — one symbol's date roll or recv
/// regression can never disturb another's.
/// `symbol` is retained as part of the designed per-symbol slot contract but
/// is not read yet — the module itself is an UNWIRED scaffold (A-4, audit
/// 2026-09-02), so the dead-code deny is consciously waived here, scoped to
/// this struct only.
#[allow(dead_code)]
struct SymbolLog {
    symbol: String,
    current_date: String,
    writer: Option<EventLogWriter>,
    last_symbol_count: usize,
    last_written_recv_ns: i64,
}

/// The per-symbol writer table (MSC-1/3/4/7/10). Keyed by symbol in a
/// `BTreeMap` so fan-out routing is deterministic (CONV-10).
pub struct WriterTable {
    raw_dir: PathBuf,
    venue: Venue,
    logs: BTreeMap<String, SymbolLog>,
    fsync_policy: FsyncPolicy,
}

impl WriterTable {
    pub fn new(
        raw_dir: PathBuf,
        venue: Venue,
        symbols: &[String],
        fsync_policy: FsyncPolicy,
    ) -> Self {
        let logs = symbols
            .iter()
            .map(|s| {
                (
                    s.clone(),
                    SymbolLog {
                        symbol: s.clone(),
                        current_date: String::new(),
                        writer: None,
                        last_symbol_count: 0,
                        last_written_recv_ns: 0,
                    },
                )
            })
            .collect();
        Self {
            raw_dir,
            venue,
            logs,
            fsync_policy,
        }
    }

    /// Owned symbols, in deterministic (insertion) order (CONV-10).
    pub fn symbols(&self) -> impl Iterator<Item = &str> {
        self.logs.keys().map(|s| s.as_str())
    }

    /// Open (or rotate to) the writer for `symbol` at `date`, applying the
    /// fsync policy (spec 019 `[fsync]` + spec 014 FSP-1). A "new" file keeps
    /// append-only semantics (W-6): existing valid frames are preserved and
    /// only a torn tail is recovered.
    fn open_writer(&mut self, symbol: &str, date: &str) -> Result<(), LogError> {
        let venue_name = self.venue_name().to_owned();
        let log = self.logs.get_mut(symbol).ok_or_else(|| {
            LogError::Io(std::io::Error::other(format!(
                "writer table has no slot for symbol {symbol}"
            )))
        })?;
        if log.current_date == date {
            return Ok(());
        }
        if let Some(ref mut w) = log.writer {
            let _ = w.flush();
        }
        let log_path = self
            .raw_dir
            .join(format!("{date}_{venue_name}_{symbol}.log"));
        tracing::info!(path = %log_path.display(), "rotating log file");
        let (mut w, truncated) = EventLogWriter::open(&log_path)?;
        if truncated {
            tracing::warn!(path = %log_path.display(), "recovered torn tail");
        }
        w.set_fsync_policy(self.fsync_policy);
        log.writer = Some(w);
        log.current_date = date.to_owned();
        log.last_symbol_count = 0;
        log.last_written_recv_ns = 0;
        Ok(())
    }

    fn venue_name(&self) -> &'static str {
        use mp_core::Venue::*;
        match self.venue {
            Bybit => "bybit",
            BinanceFutures => "binance",
            Okx => "okx",
            Hyperliquid => "hyperliquid",
            Coinbase => "coinbase",
            KrakenFutures => "kraken",
            Deribit => "deribit",
            Fred => "macro",
            Ethereum => "netflow",
            Cboe => "cboe",
            DeFiLlama => "defillama",
            Coinalyze => "coinalyze",
        }
    }
    /// Route one symbol's batch to its own log: rotate if the date changed,
    /// persist the symbol metas when they grew (MSC-7), clamp recv order per
    /// log (MSC-4), append, flush. Returns the new running recv for the
    /// caller's reference (unused by the binary; kept for tests).
    pub fn write_batch(
        &mut self,
        symbol: &str,
        date: &str,
        metas: &[SymbolMeta],
        events: &mut [EventEnvelope],
    ) -> Result<(), LogError> {
        self.open_writer(symbol, date)?;
        let log = self
            .logs
            .get_mut(symbol)
            .expect("slot exists (opened above)");
        let w = log.writer.as_mut().expect("writer opened above");
        if metas.len() != log.last_symbol_count {
            w.write_symbols(metas)?;
            log.last_symbol_count = metas.len();
        }
        log.last_written_recv_ns = crate::monotonicize(events, log.last_written_recv_ns);
        for ev in events.iter() {
            w.append(ev)?;
        }
        let _ = w.flush();
        Ok(())
    }

    /// Write one venue-level `Status` event into EVERY owned symbol log
    /// (MSC-10): connect/disconnect/gap/backpressure are data for each
    /// symbol's integrity pass — exactly as each single-symbol process logs
    /// its own Status stream today. The symbol metas written are the
    /// originating stream's full interned table (MSC-7).
    pub fn fan_out_status(
        &mut self,
        date: &str,
        status: &EventEnvelope,
        metas: &[SymbolMeta],
    ) -> Result<(), LogError> {
        let symbols: Vec<String> = self.logs.keys().cloned().collect();
        for symbol in symbols {
            let mut one = vec![status.clone()];
            self.write_batch(&symbol, date, metas, &mut one)?;
        }
        Ok(())
    }

    /// Flush + fsync every open writer on graceful shutdown (FSP-4 / COL-19);
    /// honours `FsyncPolicy::on_sigterm` per writer.
    pub fn sync_all_shutdown(&mut self) -> Result<(), LogError> {
        for log in self.logs.values_mut() {
            if let Some(w) = log.writer.as_mut() {
                w.sync_on_shutdown()?;
            }
        }
        Ok(())
    }

    /// Venue-level status events inside a symbol batch (used by the binary to
    /// fan them out after routing — MSC-10).
    pub fn status_events(events: &[EventEnvelope]) -> Vec<EventEnvelope> {
        events
            .iter()
            .filter(|e| matches!(e.body, MarketEvent::Status { .. }))
            .cloned()
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::event::Side;
    use mp_core::log::LogReader;
    use mp_core::SymbolTable;
    use mp_core::{StatusKind, SymbolId};
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "mp-multisym-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("create temp dir");
        d
    }

    fn trade(venue: Venue, sym: SymbolId, recv: i64, seq: u64) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            sym,
            recv,
            recv,
            seq,
            MarketEvent::Trade {
                price: 1.0,
                qty: 1.0,
                side: Side::Buy,
                trade_id: recv as u64,
            },
        )
    }

    fn status(venue: Venue, sym: SymbolId, recv: i64, kind: StatusKind) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            sym,
            recv,
            recv,
            0,
            MarketEvent::Status {
                kind,
                detail: String::new(),
            },
        )
    }

    fn read_events(path: &std::path::Path) -> Vec<EventEnvelope> {
        LogReader::open(path)
            .expect("open log")
            .by_ref()
            .filter_map(|r| r.ok())
            .collect()
    }

    #[test]
    fn msc_1_resolve_symbols_mutual_exclusion_and_defaults() {
        // Both forms set => ambiguous, rejected.
        assert!(resolve_symbols(Some("BTCUSDT".into()), Some(vec!["ETHUSDT".into()])).is_err());
        // Legacy single form.
        assert_eq!(
            resolve_symbols(Some("BTCUSDT".into()), None).unwrap(),
            vec!["BTCUSDT"]
        );
        // List form; trims, drops empties, dedups, preserves order.
        assert_eq!(
            resolve_symbols(
                None,
                Some(vec![
                    " ETHUSDT ".into(),
                    "".into(),
                    "SOLUSDT".into(),
                    "ETHUSDT".into()
                ])
            )
            .unwrap(),
            vec!["ETHUSDT", "SOLUSDT"]
        );
        // A single-element list behaves identically to --symbol mode (MSC-1).
        assert_eq!(
            resolve_symbols(Some("BTCUSDT".into()), None).unwrap(),
            resolve_symbols(None, Some(vec!["BTCUSDT".into()])).unwrap()
        );
        // Neither => historical flag-path default.
        assert_eq!(resolve_symbols(None, None).unwrap(), vec!["BTCUSDT"]);
        // Empty single symbol rejected.
        assert!(resolve_symbols(Some(String::new()), None).is_err());
        // All-empty list rejected.
        assert!(resolve_symbols(None, Some(vec!["  ".into()])).is_err());
    }
    #[test]
    fn msc_1_fan_out_writes_per_symbol_logs() {
        let dir = tmp_dir("logs");
        let mut table = WriterTable::new(
            dir.clone(),
            Venue::Bybit,
            &["BTCUSDT".into(), "ETHUSDT".into()],
            FsyncPolicy::default(),
        );
        let mut st = SymbolTable::new();
        let btc = st.intern_default(Venue::Bybit, "BTCUSDT");
        let eth = st.intern_default(Venue::Bybit, "ETHUSDT");
        let metas = st.metas().to_vec();

        let mut b = vec![trade(Venue::Bybit, btc, 100, 1)];
        table
            .write_batch("BTCUSDT", "20260814", &metas, &mut b)
            .unwrap();
        let mut e = vec![trade(Venue::Bybit, eth, 100, 1)];
        table
            .write_batch("ETHUSDT", "20260814", &metas, &mut e)
            .unwrap();

        // Two distinct daily logs, named exactly as the per-symbol contract.
        let btc_path = dir.join("20260814_bybit_BTCUSDT.log");
        let eth_path = dir.join("20260814_bybit_ETHUSDT.log");
        assert!(btc_path.exists() && eth_path.exists());
        let btc_events = read_events(&btc_path);
        let eth_events = read_events(&eth_path);
        assert_eq!(btc_events.len(), 1);
        assert_eq!(eth_events.len(), 1);
        assert_eq!(btc_events[0].symbol, btc);
        assert_eq!(eth_events[0].symbol, eth);
        // No unexpected logs created by the table.
        let created: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(created.len(), 2, "expected exactly two logs: {created:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn msc_2_all_or_nothing_locks_release_on_any_conflict() {
        let dir = tmp_dir("locks");
        // Pre-hold symbol A's lock (a second process owns it).
        let preheld = InstanceLock::acquire(&dir, "bybit_BTCUSDT").unwrap();
        let err =
            acquire_locks_all_or_nothing(&dir, "bybit", &["BTCUSDT".into(), "ETHUSDT".into()])
                .unwrap_err();
        assert!(
            err.to_string().contains("another collector"),
            "expected the held-lock error, got: {err}"
        );
        // All-or-nothing: symbol B's lock must NOT have been left behind.
        let files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !files.iter().any(|f| f.contains("ETHUSDT")),
            "partial lock ownership leaked: {files:?}"
        );
        drop(preheld);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn msc_3_per_symbol_rotation_independent() {
        let dir = tmp_dir("rotate");
        let mut table = WriterTable::new(
            dir.clone(),
            Venue::Bybit,
            &["BTCUSDT".into(), "ETHUSDT".into()],
            FsyncPolicy::default(),
        );
        let mut st = SymbolTable::new();
        let btc = st.intern_default(Venue::Bybit, "BTCUSDT");
        let eth = st.intern_default(Venue::Bybit, "ETHUSDT");
        let metas = st.metas().to_vec();

        let mut b1 = vec![trade(Venue::Bybit, btc, 1, 1)];
        table
            .write_batch("BTCUSDT", "20260814", &metas, &mut b1)
            .unwrap();
        let mut e1 = vec![trade(Venue::Bybit, eth, 2, 2)];
        table
            .write_batch("ETHUSDT", "20260814", &metas, &mut e1)
            .unwrap();

        // Advance the date for symbol BTCUSDT only (injected date). Its log
        // rolls; ETHUSDT stays on the old day (its writer stays open).
        let mut b2 = vec![trade(Venue::Bybit, btc, 3, 3)];
        table
            .write_batch("BTCUSDT", "20260815", &metas, &mut b2)
            .unwrap();

        let day14_btc = dir.join("20260814_bybit_BTCUSDT.log");
        let day15_btc = dir.join("20260815_bybit_BTCUSDT.log");
        let day14_eth = dir.join("20260814_bybit_ETHUSDT.log");
        assert!(day14_btc.exists() && day15_btc.exists());
        assert!(day14_eth.exists());
        // ETHUSDT must NOT have rolled to the 15th.
        assert!(!dir.join("20260815_bybit_ETHUSDT.log").exists());
        // Both days of BTCUSDT hold their own frames.
        assert_eq!(read_events(&day14_btc).len(), 1);
        assert_eq!(read_events(&day15_btc).len(), 1);
        // The untouched ETHUSDT log still appends to the 14th.
        let mut e2 = vec![trade(Venue::Bybit, eth, 4, 4)];
        table
            .write_batch("ETHUSDT", "20260814", &metas, &mut e2)
            .unwrap();
        assert_eq!(read_events(&day14_eth).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn msc_4_recv_monotonicity_per_log() {
        let dir = tmp_dir("mono");
        let mut table = WriterTable::new(
            dir.clone(),
            Venue::Bybit,
            &["BTCUSDT".into(), "ETHUSDT".into()],
            FsyncPolicy::default(),
        );
        let mut st = SymbolTable::new();
        let btc = st.intern_default(Venue::Bybit, "BTCUSDT");
        let eth = st.intern_default(Venue::Bybit, "ETHUSDT");
        let metas = st.metas().to_vec();

        // BTCUSDT batch arrives out of order (REST straggler, spec 024).
        let mut b = vec![
            trade(Venue::Bybit, btc, 100, 1),
            trade(Venue::Bybit, btc, 99, 2),
        ];
        table
            .write_batch("BTCUSDT", "20260814", &metas, &mut b)
            .unwrap();
        let btc_events = read_events(&dir.join("20260814_bybit_BTCUSDT.log"));
        let recvs: Vec<i64> = btc_events.iter().map(|e| e.recv_ts_ns).collect();
        assert!(
            recvs.windows(2).all(|w| w[0] <= w[1]),
            "clamped per log: {recvs:?}"
        );

        // ETHUSDT's first event at recv 1 stays 1 — its own clock is untouched
        // by BTCUSDT's clamp (independent last_written_recv_ns, MSC-4).
        let mut e = vec![trade(Venue::Bybit, eth, 1, 1)];
        table
            .write_batch("ETHUSDT", "20260814", &metas, &mut e)
            .unwrap();
        let eth_events = read_events(&dir.join("20260814_bybit_ETHUSDT.log"));
        assert_eq!(eth_events[0].recv_ts_ns, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn msc_5_bybit_frames_batch_under_topic_cap() {
        // Single symbol: byte-identical to the legacy single-frame builder
        // (MSC-1: `--symbols X` behaves identically to `--symbol X`).
        let single = bybit_frames_for_symbols(&["BTCUSDT".into()], false);
        assert_eq!(single.len(), 1);
        assert_eq!(
            single[0],
            r#"{"op":"subscribe","args":["publicTrade.BTCUSDT","orderbook.50.BTCUSDT","tickers.BTCUSDT","allLiquidation.BTCUSDT"]}"#
        );

        // 5 symbols × 4 topics = 20 args > the 10-arg per-frame cap → 3 frames
        // (2+2+1 symbols), none over the cap.
        let syms: Vec<String> = ["A", "B", "C", "D", "E"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let frames = bybit_frames_for_symbols(&syms, false);
        assert_eq!(frames.len(), 3, "expected 2+2+1 symbol chunks");
        for f in &frames {
            assert!(
                f.starts_with(r#"{"op":"subscribe","args":"#),
                "frame shape: {f}"
            );
            let args = f
                .trim_start_matches(r#"{"op":"subscribe","args":"#)
                .trim_end_matches('}');
            // Count topics by counting topic separators + 1.
            let n_topics = args.matches(",\"").count() + 1;
            assert!(
                n_topics <= 10,
                "frame exceeds bybit per-frame topic cap: {f}"
            );
        }
        // All 20 topics present across the frames, in order.
        let joined = frames.join("\n");
        for s in &syms {
            for t in ["publicTrade", "orderbook.50", "tickers", "allLiquidation"] {
                assert!(
                    joined.contains(&format!("\"{t}.{s}\"")),
                    "missing {t}.{s}: {joined}"
                );
            }
        }

        // Swing-only: 3 topics/symbol → 3 symbols per frame; 4 symbols → 2 frames.
        let swing =
            bybit_frames_for_symbols(&["A".into(), "B".into(), "C".into(), "D".into()], true);
        assert_eq!(swing.len(), 2);
        assert!(
            !swing.iter().any(|f| f.contains("orderbook.50")),
            "swing-only frames must never carry orderbook.50 (spec 035 SWG-1)"
        );
    }

    #[test]
    fn msc_7_status_fanout_reaches_every_owned_log_and_symbols() {
        let dir = tmp_dir("fanout");
        let mut table = WriterTable::new(
            dir.clone(),
            Venue::Bybit,
            &["BTCUSDT".into(), "ETHUSDT".into()],
            FsyncPolicy::default(),
        );
        let mut st = SymbolTable::new();
        let btc = st.intern_default(Venue::Bybit, "BTCUSDT");
        let _eth = st.intern_default(Venue::Bybit, "ETHUSDT");
        let metas = st.metas().to_vec();

        let disconnected = status(Venue::Bybit, btc, 50, StatusKind::Disconnected);
        table
            .fan_out_status("20260814", &disconnected, &metas)
            .unwrap();

        let btc_events = read_events(&dir.join("20260814_bybit_BTCUSDT.log"));
        let eth_events = read_events(&dir.join("20260814_bybit_ETHUSDT.log"));
        assert_eq!(btc_events.len(), 1);
        assert_eq!(eth_events.len(), 1);
        assert!(
            matches!(
                btc_events[0].body,
                MarketEvent::Status {
                    kind: StatusKind::Disconnected,
                    ..
                }
            ),
            "status must land in every owned log (MSC-10)"
        );
        assert!(
            matches!(
                eth_events[0].body,
                MarketEvent::Status {
                    kind: StatusKind::Disconnected,
                    ..
                }
            ),
            "status must land in every owned log (MSC-10)"
        );
        // MSC-7: both logs carry the full interned symbol table (both metas).
        let mut reader = LogReader::open(&dir.join("20260814_bybit_ETHUSDT.log")).unwrap();
        reader.load_symbols().unwrap();
        let symbols_seen = reader.symbols().len();
        assert!(
            symbols_seen >= 2,
            "fan-out must write full metas per log, saw {symbols_seen}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn msc_9_migration_is_append_only_non_destructive() {
        let dir = tmp_dir("migrate");
        // Day-1 recording written by the legacy single-symbol process.
        let legacy_path = dir.join("20260814_bybit_BTCUSDT.log");
        {
            let mut w = EventLogWriter::open(&legacy_path).unwrap().0;
            let mut st = SymbolTable::new();
            let btc = st.intern_default(Venue::Bybit, "BTCUSDT");
            w.write_symbols(st.metas()).unwrap();
            w.append(&trade(Venue::Bybit, btc, 10, 1)).unwrap();
            let _ = w.flush();
        }
        let before = read_events(&legacy_path);
        assert_eq!(before.len(), 1);

        // The fan-out process opens the SAME file name (same day) — W-6
        // append-only: old frames stay, new frames land after, nothing is
        // overwritten and no torn-tail is forced.
        let mut table = WriterTable::new(
            dir.clone(),
            Venue::Bybit,
            &["BTCUSDT".into()],
            FsyncPolicy::default(),
        );
        let mut st = SymbolTable::new();
        let btc = st.intern_default(Venue::Bybit, "BTCUSDT");
        let metas = st.metas().to_vec();
        let mut b = vec![trade(Venue::Bybit, btc, 20, 2)];
        table
            .write_batch("BTCUSDT", "20260814", &metas, &mut b)
            .unwrap();

        let after = read_events(&legacy_path);
        assert_eq!(
            after.len(),
            2,
            "legacy frame preserved + new frame appended"
        );
        assert_eq!(after[0].recv_ts_ns, 10);
        assert_eq!(after[1].recv_ts_ns, 20);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn msc_10_status_events_in_batch_are_detected_for_fanout() {
        let mut st = SymbolTable::new();
        let btc = st.intern_default(Venue::Bybit, "BTCUSDT");
        let events = vec![
            trade(Venue::Bybit, btc, 1, 1),
            status(Venue::Bybit, btc, 2, StatusKind::GapDetected),
        ];
        let statuses = WriterTable::status_events(&events);
        assert_eq!(statuses.len(), 1);
        assert!(matches!(statuses[0].body, MarketEvent::Status { .. }));
    }
}
