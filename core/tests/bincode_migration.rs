//! Spec 001 codec-amendment golden-bytes acceptance tests (BDC-1/BDC-2/BDC-3).
//!
//! `GOLDEN`, `GOLDEN_SYMBOLS`, `GOLDEN_V1` are hex of bincode **1.3.3**
//! output, captured 2026-08-14 by `core/tests/golden_capture.rs` before the
//! dependency swap. They are the wire-format law: never regenerate them. The
//! bincode-2 line (`bincode-next`) must reproduce them with
//! `config::legacy()` and decode them back to the exact source values — that
//! is what makes every recorded log since 2026-07 (schema 1/2/3 frames,
//! symbol frames, arena chunks) readable after the migration.

mod golden_values;

use mp_core::codec::{
    decode_envelope_v1, decode_event, decode_symbols, encode_envelope_v1, encode_event,
    encode_symbols,
};
use mp_core::event::{EventProvenance, MarketEvent, Side, SymbolId};
use mp_core::log::LogReader;
use mp_core::SCHEMA_VER;
use std::io::Write;
use std::path::{Path, PathBuf};

fn unhex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0, "odd-length hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Hex of bincode 1.3.3 `serialize(&[SymbolMeta])` output.
const GOLDEN_SYMBOLS: &str = "020000000000000000000000000000000700000000000000425443555344540300000000000000425443040000000000000055534454010000009a9999999999b93ffca9f1d24d62503f0000000000001440000000000000f03f0000000000000000ffffffffffffff7f010000000300000005000000000000004352554445050000000000000043525544450300000000000000555344030000007b14ae47e17a843f9a9999999999b93f00000000000024400000000000408f400000000000000000ffffffffffffff7f";

/// Hex of bincode 1.3.3 `serialize(&EnvelopeV1)` output — the
/// pre-provenance schema-1 payload the legacy reader path decodes.
const GOLDEN_V1: &str = "01000100000007000000630000000000000064000000000000000100000000000000000000000000000010c9ed40000000000000d03f000000000100000000000000";

/// `(name, hex)` in the same order as `golden_values::envelopes()`.
/// Schema-4 restamp 2026-08-18 (specs 033/034 — appended variants only): the
/// first two bytes of every entry (`schema_ver`) moved 03→04; the remaining
/// bytes are byte-identical to the schema-3 vectors captured with pinned
/// bincode 1.3.3 (BDC-1 still proves the bincode-2 line reproduces them).
const GOLDEN: &[(&str, &str)] = &[
    ("trade", "0400000000000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010c9ed40000000000000d03f000000000700000000000000"),
    ("book_delta", "040001000000020000000000000000000000000000000000000000000000000000000500000000000000646570746814000000000000006f72646572626f6f6b2e35302e425443555344542b0000000000000002000000010000000a000000000000000000000000005940000000000000f03f0000000000e0584000000000000000400000000000c0584000000000000008400000000000a058400000000000001040000000000080584000000000000014400000000000605840000000000000184000000000004058400000000000001c4000000000002058400000000000002040000000000000584000000000000022400000000000e0574000000000000024400a000000000000000000000000005940000000000000f03f0000000000e0584000000000000000400000000000c0584000000000000008400000000000a058400000000000001040000000000080584000000000000014400000000000605840000000000000184000000000004058400000000000001c4000000000002058400000000000002040000000000000584000000000000022400000000000e05740000000000000244064000000000000006d00000000000000"),
    ("book_snapshot_init", "040002000000030000000000000000000000000000000000000000000000000000000500000000000000646570746814000000000000006f72646572626f6f6b2e35302e425443555344542a00000000000000010000000200000002000000000000000000000000005940000000000000f03f0000000000e05840000000000000004002000000000000000000000000005940000000000000f03f0000000000e058400000000000000040c800000000000000320000000000"),
    ("book_snapshot_gap", "0400020000000300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000020000000a000000000000000000000000005940000000000000f03f0000000000e0584000000000000000400000000000c0584000000000000008400000000000a058400000000000001040000000000080584000000000000014400000000000605840000000000000184000000000004058400000000000001c4000000000002058400000000000002040000000000000584000000000000022400000000000e05740000000000000244002000000000000000000000000005940000000000000f03f0000000000e058400000000000000040d200000000000000320001000000"),
    ("book_snapshot_periodic", "04000200000003000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000200000002000000000000000000000000005940000000000000f03f0000000000e05840000000000000004002000000000000000000000000005940000000000000f03f0000000000e058400000000000000040dc00000000000000190002000000"),
    ("funding", "0400030000000400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000030000002d431cebe2361a3f8070000000002a36fe9c9717"),
    ("mark_price", "0400040000000500000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000040000000000000000c9ed40000000000000f87f"),
    ("open_interest", "04000500000006000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000500000000000000c01cc840000000000000f87f"),
    ("liquidation", "0400000000000700000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000060000000000000000cfec40000000000000294001000000"),
    ("index_price", "040001000000080000000000000000000000000000000000000000000000000000000500000000000000696e64657814000000000000006f72646572626f6f6b2e35302e4254435553445409000000000000000300000007000000cdccccccfcc8ed40"),
    ("status_backpressure", "0400020000000900000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000080000000600000007000000000000000a0000000000000071756575652066756c6c"),
    ("status_census", "040003000000090000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000000070000000000000000000000"),
    ("whale_position", "0400030000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000900000008000000000000003078616263313233cdccccccccdc5ec000000000006ae84000000000000024400000000000f9e540"),
    ("macro_point", "0400070000000b000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a00000005000000000000004447533130000000000000114000007caeb75a5018"),
    ("option_trade", "0400060000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000b000000030000000000000042544300000000006af8400000b49376e2fa18000000000000000000407f409a9999999999b93f010000006300000000000000"),
    ("option_book", "0400060000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c000000030000000000000042544300000000006af8400000b49376e2fa18010000000a000000000000000000000000005940000000000000f03f0000000000e0584000000000000000400000000000c0584000000000000008400000000000a058400000000000001040000000000080584000000000000014400000000000605840000000000000184000000000004058400000000000001c4000000000002058400000000000002040000000000000584000000000000022400000000000e0574000000000000024400a000000000000000000000000005940000000000000f03f0000000000e0584000000000000000400000000000c0584000000000000008400000000000a058400000000000001040000000000080584000000000000014400000000000605840000000000000184000000000004058400000000000001c4000000000002058400000000000002040000000000000584000000000000022400000000000e0574000000000000024402b0200000000000001"),
    ("option_ticker_greeks_some", "0400060000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000d000000030000000000000042544300000000006af8400000b49376e2fa18000000009a9999999999e13f0000000000007e400000000000c9ed400000000000448f4001000000000000e03ffca9f1d24d62503f00000000000028c09a9999999999d93f"),
    ("option_ticker_greeks_none", "0400060000000c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000d000000030000000000000042544300000000006af8400000b49376e2fa1801000000333333333333e33f0000000000607d400000000000c9ed400000000000228c4000"),
    ("trade_with_addr", "0400030000000d000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e0000000000000010c9ed40000000000000d03f00000000070000000000000008000000000000003078616263313233"),
    ("netflow_snapshot", "0400080000000e000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000f0000000a00000000000000307864656164626565663d0ad7e387d63241"),
];

#[test]
fn bdc_1_golden_bincode1_bytes_unchanged() {
    let envs = golden_values::envelopes();
    assert_eq!(GOLDEN.len(), envs.len(), "GOLDEN table out of sync");
    for ((name, hex), (n2, ev)) in GOLDEN.iter().zip(&envs) {
        assert_eq!(name, n2, "GOLDEN order out of sync");
        let expected = unhex(hex);
        assert_eq!(
            encode_event(ev).unwrap(),
            expected,
            "encode drift vs bincode 1.3.3 for {name}"
        );
    }
    let metas = golden_values::symbol_metas();
    assert_eq!(encode_symbols(&metas).unwrap(), unhex(GOLDEN_SYMBOLS));
    let v1 = golden_values::envelope_v1();
    assert_eq!(encode_envelope_v1(&v1).unwrap(), unhex(GOLDEN_V1));
}

#[test]
fn bdc_2_golden_bytes_decode_with_legacy() {
    let envs = golden_values::envelopes();
    for ((name, hex), (n2, ev)) in GOLDEN.iter().zip(&envs) {
        assert_eq!(name, n2);
        let bytes = unhex(hex);
        let decoded = decode_event(&bytes).unwrap();
        // Structural check — Debug normalizes NaN (the f64 sentinel used by
        // MarkPrice/OpenInterest), so this compares semantically.
        assert_eq!(
            format!("{decoded:?}"),
            format!("{ev:?}"),
            "decode drift vs bincode 1.3.3 for {name}"
        );
        // Rigorous identity: decode(hex) must re-encode to hex. bincode
        // copies f64 payload bits verbatim (incl. NaN bit patterns), so
        // this proves the decoded value is bit-exactly the source value.
        assert_eq!(
            encode_event(&decoded).unwrap(),
            bytes,
            "decode→re-encode drift for {name}"
        );
    }
    let metas = golden_values::symbol_metas();
    assert_eq!(decode_symbols(&unhex(GOLDEN_SYMBOLS)).unwrap(), metas);
    let v1 = golden_values::envelope_v1();
    assert_eq!(decode_envelope_v1(&unhex(GOLDEN_V1)).unwrap(), v1);
}

/// BDC-3: the full legacy read path on *committed historical bytes* — a
/// hand-framed schema-1 log whose payload is the golden `EnvelopeV1` hex
/// (not freshly serialized) must decode through the real `LogReader` with
/// synthetic provenance, exactly like the pre-migration recordings.
#[test]
fn bdc_3_existing_compat_paths_still_decode() {
    let dir = std::env::temp_dir().join(format!("mplog-bdc3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schema1.log");
    let _ = std::fs::remove_file(&path);

    // payload = schema_ver:u16 || bincode(EnvelopeV1); frame = kind(1) ||
    // len:u32 || crc:u32 || payload; file = MAGIC || format_ver:u16 || frames.
    let mut payload = 1u16.to_le_bytes().to_vec();
    payload.extend_from_slice(&unhex(GOLDEN_V1));
    let mut frame = vec![1u8]; // FRAME_EVENT
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
    frame.extend_from_slice(&payload);

    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"MPLOG\0\0\0").unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&frame).unwrap();
    f.sync_all().unwrap();

    let got: Vec<_> = LogReader::open(&path)
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(got.len(), 1, "golden schema-1 frame must decode");
    let e = &got[0];
    assert_eq!(e.schema_ver, 1);
    assert_eq!(e.recv_ts_ns, 100);
    assert_eq!(e.stream_seq, 1);
    assert_eq!(e.symbol, SymbolId(7));
    // Synthetic provenance: empty stream/subscription, no snapshot source
    // (INT-1 flags this as missing_provenance; readable, never promotable).
    assert_eq!(e.provenance, EventProvenance::synthetic());
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

// ---- BDC-4..9: repository-level acceptance (CONV-21 requires an ID-bearing
// test fn per requirement; these are fast, hermetic file checks) ------------

/// Workspace root: this test lives in `core/tests`, so the workspace root is
/// the parent of the crate directory (`CARGO_MANIFEST_DIR` is `core/`).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core/ sits inside the workspace")
        .to_path_buf()
}

fn rs_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
    {
        let p = entry.unwrap().path();
        if p.is_dir() {
            rs_files_under(&p, out);
        } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn lockfile() -> String {
    std::fs::read_to_string(workspace_root().join("Cargo.lock"))
        .expect("workspace Cargo.lock readable")
}

/// BDC-4: no v1 bincode API remains. The bare v1 free functions (the
/// `serialize` / `deserialize` entry points on the bincode 1 crate path)
/// may not appear in any `core/` or `storage/` source, and the lockfile
/// must not pin the unmaintained `bincode` crate (crate name exactly, so
/// `bincode-next` does not false-positive).
#[test]
fn bdc_4_no_v1_bincode_api_remains() {
    // Needles built at runtime so this file does not itself contain the
    // tokens it scans for.
    let v1_call = |f: &str| format!("bincode::{f}");
    let needle_enc = v1_call("serialize");
    let needle_dec = v1_call("deserialize");
    let root = workspace_root();
    let mut files = Vec::new();
    for dir in ["core", "storage"] {
        rs_files_under(&root.join(dir), &mut files);
    }
    assert!(
        !files.is_empty(),
        "no .rs sources found under core/ + storage/"
    );
    for p in &files {
        let src =
            std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        assert!(
            !src.contains(&needle_enc) && !src.contains(&needle_dec),
            "v1 bincode API call in {}",
            p.display()
        );
    }
    assert!(
        !lockfile().contains("name = \"bincode\""),
        "Cargo.lock still pins the unmaintained bincode v1 crate"
    );
}

/// BDC-5: RUSTSEC-2025-0141 targets the `bincode` crate itself, which has
/// **no patched version** — with no `bincode` package in the lockfile the
/// advisory cannot be reported. (The full `cargo audit` report is the
/// deps-audit CI job; verified locally 2026-08-14: only the known `paste`
/// RUSTSEC-2024-0436 warning remains.)
#[test]
fn bdc_5_audit_no_longer_flags_bincode() {
    let lock = lockfile();
    assert!(
        !lock.contains("name = \"bincode\""),
        "RUSTSEC-2025-0141 would still be reported"
    );
    assert!(
        lock.contains("name = \"bincode-next\""),
        "adopted line (bincode-next) missing from Cargo.lock"
    );
}

/// BDC-6 (operator step, not CI — real data is never committed, W-6): the
/// new codec must fully decode a pre-swap recording. `#[ignore]` so CI skips
/// it; run over the real corpus with:
///
/// ```text
/// cargo test -p mp-core --test bincode_migration -- --ignored bdc_6
/// ```
///
/// Defaults to `data/collected.eventlog`; override with `BDC6_LOG=/path`.
#[test]
#[ignore = "operator step over real data/ files (spec 001 bdc_6)"]
fn bdc_6_real_corpus_readback_matches() {
    let path = std::env::var("BDC6_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("data/collected.eventlog"));
    assert!(
        path.exists(),
        "no real corpus at {} — operator step",
        path.display()
    );
    let reader = LogReader::open(&path).unwrap();
    let mut count = 0usize;
    let mut first = i64::MAX;
    let mut last = i64::MIN;
    for ev in reader {
        let ev = ev.unwrap();
        count += 1;
        first = first.min(ev.recv_ts_ns);
        last = last.max(ev.recv_ts_ns);
    }
    assert!(count > 0, "log decoded zero events — possible format break");
    assert!(last >= first, "non-monotonic recv_ts_ns");
    println!("bdc_6: {count} events, recv_ts {first}..{last} ns");
}

/// BDC-7: the workspace adopts `bincode-next` (the maintained bincode-2
/// line) with the serde feature; the canonical `bincode` crate stays out of
/// the manifest.
#[test]
fn bdc_7_adopts_bincode_next_line() {
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap();
    assert!(
        manifest.contains("bincode-next = { version = \"3\", features = [\"serde\"] }"),
        "workspace manifest must pin bincode-next 3.x with the serde feature"
    );
    assert!(
        !manifest.contains("bincode ="),
        "no canonical `bincode` dependency may remain"
    );
}

/// BDC-8: the on-disk format is frozen — spec 001 must not bump
/// `FORMAT_VER`/`SCHEMA_VER` (EVT-4/EVT-8, CONV-20). Pin their current
/// values; a deliberate future bump is a separate, spec-amended decision.
///
/// Schema-4 bump 2026-08-18: spec 001 amendment 033/034 (TradeWithAddr,
/// NetflowSnapshot, Venue::Ethereum) — append-only variant additions, reader
/// arm added in log.rs, `GOLDEN` restamped. Format version unchanged.
#[test]
fn bdc_8_wire_format_frozen() {
    assert_eq!(
        SCHEMA_VER, 4,
        "SCHEMA_VER must stay frozen by spec 001 (BDC-8)"
    );
    let log_src = std::fs::read_to_string(workspace_root().join("core/src/log.rs")).unwrap();
    assert!(
        log_src.contains("const FORMAT_VER: u16 = 1;"),
        "FORMAT_VER must stay 1 (frozen by spec 001 BDC-8)"
    );
}

/// BDC-9: the declared workspace MSRV was raised to the adopted line's
/// floor (`bincode-next` 3.x declares rust-version 1.90).
#[test]
fn bdc_9_rust_version_raised() {
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap();
    let floor = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("rust-version = "))
        .expect("workspace rust-version declared")
        .trim_matches('"');
    assert!(
        floor >= "1.90",
        "workspace rust-version must be >= 1.90 (bincode-next MSRV), got {floor}"
    );
}
