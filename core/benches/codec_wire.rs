//! Criterion bench: replay-CPU cost of the current `legacy()` wire config vs
//! the `config::standard()` (varint) alternative sketched in spec 001's
//! appendix ("future wire format"). Quantifies appendix point 5 — the CPU
//! cost of a varint flip on the replay path.
//!
//! Run: `cargo bench -p mp-core --bench codec_wire`
//! Quick pass: add `-- --measurement-time 1`.
//!
//! The payloads mirror the two dominant recorded event shapes (spec 001):
//! a `Trade` envelope and a `BookDelta` envelope with a few levels per side,
//! timestamps at 2026 scale (~1.78e18 ns — where varint grows to 9 bytes).
//! Encode and decode are measured with both `legacy()` (current wire format,
//! BDC-3) and `standard()` (varint). Throughput is reported as bytes moved
//! per second using each config's own encoded size.

use bincode_next::config::{legacy, standard};
use bincode_next::serde::{decode_from_slice, encode_to_vec};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use mp_core::{
    EventEnvelope, EventProvenance, Levels, MarketEvent, Side, SnapshotSource, SymbolId, Venue,
};
use std::hint::black_box;

/// Trade envelope with realistic 2026-scale timestamps and a short
/// provenance (stream/subscription strings), matching what the collectors
/// append today.
fn trade_envelope() -> EventEnvelope {
    EventEnvelope::new(
        Venue::Hyperliquid,
        SymbolId(1),
        1_784_163_765_998_000_000, // exch_ts_ns
        1_784_163_766_599_806_900, // recv_ts_ns
        42_000_007,                // stream_seq
        MarketEvent::Trade {
            price: 43_251.5,
            qty: 0.37,
            side: Side::Buy,
            trade_id: 6_721_234_567_890,
        },
    )
    .with_provenance(EventProvenance {
        stream: "trade".into(),
        subscription: "BTC".into(),
        connection_id: 7,
        snapshot_source: SnapshotSource::None,
    })
}

/// Book-delta envelope with 4 bid + 3 ask levels, one removal (qty 0.0),
/// and contiguous seq bounds.
fn book_delta_envelope() -> EventEnvelope {
    let bids: Levels = vec![
        (43_251.5, 12.3),
        (43_250.0, 5.1),
        (43_249.5, 0.0),
        (43_249.0, 8.8),
    ]
    .into_iter()
    .collect();
    let asks: Levels = vec![(43_252.0, 9.9), (43_253.0, 3.3), (43_254.0, 17.7)]
        .into_iter()
        .collect();
    EventEnvelope::new(
        Venue::Hyperliquid,
        SymbolId(1),
        1_784_163_765_998_000_000,
        1_784_163_766_599_806_900,
        100_000,
        MarketEvent::BookDelta {
            bids,
            asks,
            first_seq: 100_000,
            last_seq: 100_003,
        },
    )
    .with_provenance(EventProvenance {
        stream: "depth".into(),
        subscription: "BTC".into(),
        connection_id: 7,
        snapshot_source: SnapshotSource::None,
    })
}

/// Encode + decode for one payload under both configs. Each benchmark's
/// throughput is the byte count of that config's own encoding, so the
/// reported bytes/sec are comparable (bytes moved differs between configs).
fn bench_payload(c: &mut Criterion, name: &str, env: &EventEnvelope) {
    let legacy_bytes = encode_to_vec(env, legacy()).expect("legacy encode");
    let standard_bytes = encode_to_vec(env, standard()).expect("standard encode");
    eprintln!(
        "[{name}] payload: legacy={} B, standard={} B ({:.1}%)",
        legacy_bytes.len(),
        standard_bytes.len(),
        100.0 * (1.0 - standard_bytes.len() as f64 / legacy_bytes.len() as f64)
    );

    let mut group = c.benchmark_group(format!("codec/{name}"));

    group.throughput(Throughput::Bytes(legacy_bytes.len() as u64));
    group.bench_function("encode/legacy", |b| {
        b.iter(|| black_box(encode_to_vec(black_box(env), legacy()).expect("encode")))
    });
    group.throughput(Throughput::Bytes(standard_bytes.len() as u64));
    group.bench_function("encode/standard", |b| {
        b.iter(|| black_box(encode_to_vec(black_box(env), standard()).expect("encode")))
    });

    group.throughput(Throughput::Bytes(legacy_bytes.len() as u64));
    group.bench_function("decode/legacy", |b| {
        b.iter(|| {
            black_box(
                decode_from_slice::<EventEnvelope, _>(black_box(&legacy_bytes), legacy())
                    .map(|(v, _)| v)
                    .expect("decode"),
            )
        })
    });
    group.throughput(Throughput::Bytes(standard_bytes.len() as u64));
    group.bench_function("decode/standard", |b| {
        b.iter(|| {
            black_box(
                decode_from_slice::<EventEnvelope, _>(black_box(&standard_bytes), standard())
                    .map(|(v, _)| v)
                    .expect("decode"),
            )
        })
    });

    group.finish();
}

fn codec_wire(c: &mut Criterion) {
    bench_payload(c, "trade", &trade_envelope());
    bench_payload(c, "book_delta", &book_delta_envelope());
}

criterion_group!(benches, codec_wire);
criterion_main!(benches);
