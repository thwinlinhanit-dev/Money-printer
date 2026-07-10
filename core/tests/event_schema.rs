//! Acceptance tests for spec 001. Test names embed requirement IDs (CONV-21).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mp_core::codec::{decode_event, encode_event};
use mp_core::event::*;
use mp_core::log::{EventLogWriter, LogReader, MergeReader};
use mp_core::ring::{Overrun, Ring};
use mp_core::symbol::{InstrumentKind, SymbolMeta, SymbolTable};
use mp_core::{BookMirror, EventEnvelope, SCHEMA_VER};

use proptest::prelude::*;
use smallvec::SmallVec;

// Thread-local counting allocator to prove the Trade path is heap-free (EVT-2).
// Only the arming thread counts, so concurrent test threads don't pollute it.
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
        static ALLOCS: Cell<u64> = const { Cell::new(0) };
    }

    pub struct Counting;
    // SAFETY: delegates to System; only adds a thread-local Cell increment.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if ARMED.with(Cell::get) {
                ALLOCS.with(|c| c.set(c.get() + 1));
            }
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
    }

    /// Count allocations made on this thread while `f` runs.
    pub fn count_allocs(f: impl FnOnce()) -> u64 {
        // Warm both TLS keys while disarmed so init doesn't count.
        ARMED.with(|a| a.set(false));
        ALLOCS.with(|c| c.set(0));
        ARMED.with(|a| a.set(true));
        f();
        ARMED.with(|a| a.set(false));
        ALLOCS.with(Cell::get)
    }
}

#[global_allocator]
static GLOBAL: alloc_probe::Counting = alloc_probe::Counting;

// ---- helpers ----------------------------------------------------------------

fn levels_strategy() -> impl Strategy<Value = Levels> {
    prop::collection::vec((-1.0e9f64..1.0e9, 0.0f64..1.0e9), 0..10)
        .prop_map(|v| v.into_iter().collect::<SmallVec<[Level; 8]>>())
}

fn side_strategy() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Buy), Just(Side::Sell)]
}

fn body_strategy() -> impl Strategy<Value = MarketEvent> {
    prop_oneof![
        (
            -1.0e9f64..1.0e9,
            0.0f64..1.0e9,
            side_strategy(),
            any::<u64>()
        )
            .prop_map(|(price, qty, side, trade_id)| MarketEvent::Trade {
                price,
                qty,
                side,
                trade_id
            }),
        (
            levels_strategy(),
            levels_strategy(),
            any::<u64>(),
            any::<u64>()
        )
            .prop_map(|(bids, asks, a, b)| MarketEvent::BookDelta {
                bids,
                asks,
                first_seq: a.min(b),
                last_seq: a.max(b),
            }),
        (
            levels_strategy(),
            levels_strategy(),
            any::<u64>(),
            any::<u16>()
        )
            .prop_map(|(bids, asks, seq, depth)| MarketEvent::BookSnapshot {
                bids,
                asks,
                seq,
                depth,
                reason: SnapshotReason::Init,
            }),
        (-1.0f64..1.0, any::<u32>(), any::<i64>()).prop_map(|(rate, interval_s, n)| {
            MarketEvent::Funding {
                rate,
                interval_s,
                next_funding_ts_ns: n,
            }
        }),
        (0.0f64..1.0e9, 0.0f64..1.0e9)
            .prop_map(|(mark, index)| MarketEvent::MarkPrice { mark, index }),
        (0.0f64..1.0e12, 0.0f64..1.0e12).prop_map(|(oi_contracts, oi_notional)| {
            MarketEvent::OpenInterest {
                oi_contracts,
                oi_notional,
            }
        }),
        (0.0f64..1.0e9, 0.0f64..1.0e9, side_strategy())
            .prop_map(|(price, qty, side)| MarketEvent::Liquidation { price, qty, side }),
        (0.0f64..1.0e9).prop_map(|index| MarketEvent::IndexPrice { index }),
        ".*".prop_map(|detail| MarketEvent::Status {
            kind: StatusKind::GapDetected,
            detail,
        }),
    ]
}

fn envelope_strategy() -> impl Strategy<Value = EventEnvelope> {
    (any::<i64>(), any::<i64>(), any::<u64>(), body_strategy()).prop_map(
        |(exch, recv, seq, body)| {
            EventEnvelope::new(Venue::Bybit, SymbolId(3), exch, recv, seq, body)
        },
    )
}

fn ev(recv_ts_ns: i64, stream_seq: u64) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SymbolId(0),
        0,
        recv_ts_ns,
        stream_seq,
        MarketEvent::Trade {
            price: 1.0,
            qty: 1.0,
            side: Side::Buy,
            trade_id: stream_seq,
        },
    )
}

// ---- EVT-1 / EVT-3 ----------------------------------------------------------

#[test]
fn evt_1_envelope_new_stamps_schema_ver() {
    let e = ev(1, 1);
    assert_eq!(e.schema_ver, SCHEMA_VER);
}

#[test]
fn evt_2_trade_envelope_is_alloc_free() {
    // Constructing and cloning a Trade envelope must not touch the heap: the
    // Trade variant carries no SmallVec/String, so it is stack-only (EVT-2).
    let mut sink = None;
    let allocs = alloc_probe::count_allocs(|| {
        let e = EventEnvelope::new(
            Venue::Bybit,
            SymbolId(1),
            1,
            2,
            3,
            MarketEvent::Trade {
                price: 1.0,
                qty: 2.0,
                side: Side::Buy,
                trade_id: 9,
            },
        );
        let e2 = e.clone();
        sink = Some(std::hint::black_box(e2));
    });
    assert!(sink.is_some());
    assert_eq!(allocs, 0, "Trade construct+clone must be heap-free (EVT-2)");
}

proptest! {
    #[test]
    fn evt_3_envelope_roundtrip(e in envelope_strategy()) {
        let bytes = encode_event(&e).unwrap();
        let back = decode_event(&bytes).unwrap();
        prop_assert_eq!(e, back);
    }
}

#[test]
fn evt_3_nan_sentinel_roundtrip() {
    // NaN is the "absent" sentinel for MarkPrice.index / OpenInterest.oi_notional.
    let e = EventEnvelope::new(
        Venue::Okx,
        SymbolId(1),
        0,
        0,
        0,
        MarketEvent::MarkPrice {
            mark: 100.0,
            index: f64::NAN,
        },
    );
    let back = decode_event(&encode_event(&e).unwrap()).unwrap();
    match back.body {
        MarketEvent::MarkPrice { mark, index } => {
            assert_eq!(mark, 100.0);
            assert!(index.is_nan());
        }
        _ => panic!("wrong variant"),
    }
}

// ---- EVT-8 ------------------------------------------------------------------

#[test]
fn evt_8_symbol_interning_stable() {
    let mut t = SymbolTable::new();
    let make = |id| {
        SymbolMeta::new(
            id,
            Venue::Bybit,
            "BTCUSDT",
            "BTC",
            "USDT",
            InstrumentKind::Perp,
            0.1,
            0.001,
            5.0,
        )
    };
    let a = t.intern(Venue::Bybit, "BTCUSDT", make);
    let b = t.intern(Venue::Bybit, "BTCUSDT", make);
    let c = t.intern(Venue::Bybit, "ETHUSDT", |id| {
        SymbolMeta::new(
            id,
            Venue::Bybit,
            "ETHUSDT",
            "ETH",
            "USDT",
            InstrumentKind::Perp,
            0.01,
            0.01,
            5.0,
        )
    });
    assert_eq!(a, b, "same key must intern to same id");
    assert_ne!(a, c);
    assert_eq!(t.get(a).unwrap().venue_symbol, "BTCUSDT");
    // Persist + rebuild preserves ids (EVT-8).
    let rebuilt = SymbolTable::from_metas(t.metas().to_vec());
    assert_eq!(rebuilt.lookup(Venue::Bybit, "BTCUSDT"), Some(a));
    assert_eq!(rebuilt.get(c).unwrap().venue_symbol, "ETHUSDT");
}

// ---- EVT-4 ------------------------------------------------------------------

#[test]
fn evt_4_torn_tail_recovered_on_open() {
    let dir = std::env::temp_dir().join(format!("mplog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("evt4a.log");
    let _ = std::fs::remove_file(&path);

    // Write 5 events cleanly.
    let (mut w, truncated) = EventLogWriter::open(&path).unwrap();
    assert!(!truncated);
    for i in 0..5 {
        w.append(&ev(i, i as u64)).unwrap();
    }
    w.sync().unwrap();
    drop(w);
    let good_len = std::fs::metadata(&path).unwrap().len();

    // Simulate a torn write: append a partial/garbage frame.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(&[1u8, 0xff, 0xff, 0x00, 0x00, 0x42, 0x42])
            .unwrap(); // header claims huge len, no payload
        f.sync_all().unwrap();
    }
    assert!(std::fs::metadata(&path).unwrap().len() > good_len);

    // Reopen: torn tail is detected and truncated (EVT-4 → WARN via `truncated`).
    let (w2, truncated2) = EventLogWriter::open(&path).unwrap();
    assert!(truncated2, "torn tail must be reported");
    drop(w2);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), good_len);

    // Reader yields exactly the 5 whole records.
    let got: Vec<_> = LogReader::open(&path)
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(got.len(), 5);
    for (i, e) in got.iter().enumerate() {
        assert_eq!(e.recv_ts_ns, i as i64);
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn evt_4_symbols_and_events_reload() {
    let dir = std::env::temp_dir().join(format!("mplog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("evt4b.log");
    let _ = std::fs::remove_file(&path);

    let mut table = SymbolTable::new();
    table.intern_default(Venue::Bybit, "BTCUSDT");
    let (mut w, _) = EventLogWriter::open(&path).unwrap();
    w.write_symbols(table.metas()).unwrap();
    w.append(&ev(1, 1)).unwrap();
    w.sync().unwrap();
    drop(w);

    let mut r = LogReader::open(&path).unwrap();
    let e = r.next().unwrap().unwrap();
    assert_eq!(e.recv_ts_ns, 1);
    assert_eq!(r.symbols().len(), 1);
    assert_eq!(r.symbols()[0].venue_symbol, "BTCUSDT");
    let _ = std::fs::remove_file(&path);
}

// ---- EVT-5 ------------------------------------------------------------------

#[test]
fn evt_5_kway_merge_two_venues_two_days() {
    let dir = std::env::temp_dir().join(format!("mplog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p1 = dir.join("venueA.log");
    let p2 = dir.join("venueB.log");
    let _ = std::fs::remove_file(&p1);
    let _ = std::fs::remove_file(&p2);

    // Each file is individually sorted by recv_ts_ns (2 "days" concatenated).
    let a_times = [1i64, 3, 5, 100, 102];
    let b_times = [2i64, 4, 6, 101, 103];
    let (mut wa, _) = EventLogWriter::open(&p1).unwrap();
    for (i, &t) in a_times.iter().enumerate() {
        wa.append(&ev(t, i as u64)).unwrap();
    }
    wa.sync().unwrap();
    let (mut wb, _) = EventLogWriter::open(&p2).unwrap();
    for (i, &t) in b_times.iter().enumerate() {
        wb.append(&ev(t, i as u64)).unwrap();
    }
    wb.sync().unwrap();

    let merged: Vec<_> = MergeReader::new(vec![
        LogReader::open(&p1).unwrap(),
        LogReader::open(&p2).unwrap(),
    ])
    .map(|r| r.unwrap())
    .collect();

    assert_eq!(merged.len(), a_times.len() + b_times.len());
    let keys: Vec<i64> = merged.iter().map(|e| e.recv_ts_ns).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "merged stream must be globally ordered");
    let _ = std::fs::remove_file(&p1);
    let _ = std::fs::remove_file(&p2);
}

// ---- EVT-6 / EVT-7 ----------------------------------------------------------

#[test]
fn evt_6_ring_fifo_within_capacity() {
    let ring = Ring::<u64>::with_capacity(8);
    let mut p = ring.producer();
    let mut c = ring.consumer();
    for i in 0..8 {
        p.push(i);
    }
    for i in 0..8 {
        assert_eq!(c.try_recv(), Ok(Some(i)));
    }
    assert_eq!(c.try_recv(), Ok(None));
}

#[test]
fn evt_7_ring_overrun_deterministic() {
    let ring = Ring::<u64>::with_capacity(8);
    let mut p = ring.producer();
    let mut c = ring.consumer(); // cursor at 0 before any push
    for i in 0..100 {
        p.push(i);
    }
    // First read is overrun: fell 92 behind, resynced to oldest live (index 92).
    assert_eq!(c.try_recv(), Err(Overrun::Overrun { skipped: 92 }));
    for i in 92..100 {
        assert_eq!(c.try_recv(), Ok(Some(i)));
    }
    assert_eq!(c.try_recv(), Ok(None));
}

#[test]
fn evt_7_ring_no_torn_reads_under_contention() {
    const N: u64 = 200_000;
    let ring = Ring::<u64>::with_capacity(64);
    let mut p = ring.producer();
    let mut c = ring.consumer();
    let done = Arc::new(AtomicBool::new(false));

    let done_w = done.clone();
    let producer = std::thread::spawn(move || {
        for i in 0..N {
            p.push(i);
        }
        done_w.store(true, Ordering::Release);
    });

    let mut received: u64 = 0;
    let mut skipped: u64 = 0;
    let mut last: Option<u64> = None;
    loop {
        match c.try_recv() {
            Ok(Some(v)) => {
                // Strictly increasing ⇒ no duplicate, no reorder, no torn value.
                if let Some(l) = last {
                    assert!(v > l, "non-monotonic read: {v} after {l}");
                }
                last = Some(v);
                received += 1;
            }
            Ok(None) => {
                if done.load(Ordering::Acquire) && c.cursor() >= N {
                    break;
                }
                std::hint::spin_loop();
            }
            Err(Overrun::Overrun { skipped: s }) => skipped += s,
        }
    }
    producer.join().unwrap();
    assert_eq!(received + skipped, N, "every index accounted once");
    assert!(received > 0);
}

// ---- EVT-9 ------------------------------------------------------------------

#[test]
fn evt_9_book_gap_marks_stale_until_snapshot() {
    let mut book = BookMirror::new();
    assert!(book.is_stale(), "uninitialized book is stale");

    // Snapshot at seq 10.
    book.apply(&MarketEvent::BookSnapshot {
        bids: [(100.0, 5.0), (99.0, 3.0)].into_iter().collect(),
        asks: [(101.0, 4.0), (102.0, 2.0)].into_iter().collect(),
        seq: 10,
        depth: 2,
        reason: SnapshotReason::Init,
    });
    assert!(!book.is_stale());
    assert_eq!(book.best_bid(), Some((100.0, 5.0)));
    assert_eq!(book.best_ask(), Some((101.0, 4.0)));
    assert_eq!(book.mid(), Some(100.5));

    // Contiguous delta 11..=12 applies.
    assert!(book.apply(&MarketEvent::BookDelta {
        bids: [(100.0, 0.0)].into_iter().collect(), // remove top bid
        asks: SmallVec::new(),
        first_seq: 11,
        last_seq: 12,
    }));
    assert_eq!(book.best_bid(), Some((99.0, 3.0)));

    // Gap: next delta starts at 15 (13,14 missing) ⇒ stale, reads refused.
    assert!(!book.apply(&MarketEvent::BookDelta {
        bids: [(98.0, 9.0)].into_iter().collect(),
        asks: SmallVec::new(),
        first_seq: 15,
        last_seq: 15,
    }));
    assert!(book.is_stale());
    assert_eq!(book.best_bid(), None);
    assert_eq!(book.mid(), None);

    // Fresh snapshot clears staleness (EVT-9 "refuse reads until next snapshot").
    book.apply(&MarketEvent::BookSnapshot {
        bids: [(97.0, 1.0)].into_iter().collect(),
        asks: [(98.0, 1.0)].into_iter().collect(),
        seq: 20,
        depth: 1,
        reason: SnapshotReason::GapResync,
    });
    assert!(!book.is_stale());
    assert_eq!(book.best_bid(), Some((97.0, 1.0)));
}

#[test]
fn evt_9_book_ignores_old_delta() {
    let mut book = BookMirror::new();
    book.apply_snapshot(10, &[(100.0, 1.0)], &[(101.0, 1.0)]);
    // Fully-old delta (last_seq <= current) is ignored, not a gap.
    assert!(!book.apply_delta(5, 9, &[(100.0, 999.0)], &[]));
    assert!(!book.is_stale());
    assert_eq!(book.best_bid(), Some((100.0, 1.0)));
}
