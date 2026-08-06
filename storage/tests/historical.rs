//! Acceptance tests for spec 027 (historical bootstrap). Test names embed
//! requirement IDs (CONV-21). Local fixtures, no network (CONV-23).
//!
//! The core acceptance suite (hbs_2..hbs_7, hbs_9) is transport-agnostic and
//! runs with a plain `cargo test -p mp-storage`. HBS-1 (live Binance-archive
//! download) and HBS-8 (rate-limit/backoff) are exercised in the gated
//! `live_http` module against a LOCAL mock HTTP server — owner-approved
//! 2026-08-05 — and run with `cargo test -p mp-storage --features live-http`.

use mp_core::{MarketEvent, Side, SymbolId};
use mp_storage::historical::{
    bootstrap_day, historical_manifest_file, historical_trades_file, parse_aggtrades,
    parse_historical_config, HistoricalConfig, MockHistoricalSource,
};

const CSV: &str = "\
price,quantity,quote_quantity,time,is_buyer_maker,is_best_match
50000,1,50000,1700000000000,false,true
50010,2,100020,1700000001000,true,true
50005,0.5,25002.5,1700000002000,false,true
";

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mphist-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cfg() -> HistoricalConfig {
    HistoricalConfig::default()
}

#[test]
fn hbs_6_aggtrades_csv_parses_to_canonical_trades() {
    let events = parse_aggtrades(CSV, SymbolId(7)).unwrap();
    assert_eq!(events.len(), 3, "header row skipped (HBS-6)");
    // Row 0: price 50000 qty 1, isBuyerMaker=false ⇒ aggressor Buy.
    assert_eq!(events[0].exch_ts_ns, 1_700_000_000_000_000_000);
    assert_eq!(events[0].recv_ts_ns, events[0].exch_ts_ns); // honest replay (no recv clock)
    match events[0].body {
        MarketEvent::Trade {
            price, qty, side, ..
        } => {
            assert_eq!(price, 50000.0);
            assert_eq!(qty, 1.0);
            assert_eq!(side, Side::Buy);
        }
        _ => panic!("expected trade"),
    }
    // Row 1: isBuyerMaker=true ⇒ aggressor Sell (mirrors collectors::binance `m`).
    match events[1].body {
        MarketEvent::Trade { price, side, .. } => {
            assert_eq!(price, 50010.0);
            assert_eq!(side, Side::Sell);
        }
        _ => panic!("expected trade"),
    }
    // Row 2.
    match events[2].body {
        MarketEvent::Trade { qty, side, .. } => {
            assert_eq!(qty, 0.5);
            assert_eq!(side, Side::Buy);
        }
        _ => panic!("expected trade"),
    }
    // Deterministic stream_seq = row order (HBS-6).
    assert_eq!(events[0].stream_seq, 1);
    assert_eq!(events[1].stream_seq, 2);
}

#[test]
fn hbs_5_and_4_are_deterministic_and_idempotent() {
    let root = tmp("hbs54");
    let src = MockHistoricalSource { text: CSV.into() };
    let s1 = bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "gitsha", &cfg()).unwrap();
    assert_eq!(s1.files_written, 1);
    assert_eq!(s1.rows_written, 3);
    let f1 = historical_trades_file(&root, "BTCUSDT", "2026-08-01");
    let bytes1 = std::fs::read(&f1).unwrap();
    // HBS-4: re-run with the same content-hash is a skip; HBS-5: the Parquet
    // bytes are byte-identical.
    let s2 = bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "gitsha", &cfg()).unwrap();
    assert_eq!(s2.files_skipped, 1);
    assert_eq!(
        std::fs::read(&f1).unwrap(),
        bytes1,
        "idempotent + deterministic (HBS-4/5)"
    );
}

#[test]
fn hbs_4_rerun_skips_byte_identical() {
    // HBS-4: idempotent re-run — same source text ⇒ same content-hash ⇒ the
    // Parquet rewrite is skipped and the (identical) manifest stays present.
    let root = tmp("hbs4");
    let src = MockHistoricalSource { text: CSV.into() };
    let s1 = bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "g", &cfg()).unwrap();
    assert_eq!(s1.files_written, 1);
    let f1 = historical_trades_file(&root, "BTCUSDT", "2026-08-01");
    let bytes1 = std::fs::read(&f1).unwrap();
    let s2 = bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "g", &cfg()).unwrap();
    assert_eq!(s2.files_skipped, 1, "skip on same hash (HBS-4)");
    assert_eq!(s2.files_written, 0);
    assert_eq!(
        std::fs::read(&f1).unwrap(),
        bytes1,
        "byte-identical (HBS-4/5)"
    );
    assert!(historical_manifest_file(&root, "BTCUSDT", "2026-08-01").exists());
}

#[test]
fn hbs_10_fixtures_no_network() {
    // HBS-10/CONV-23: tests use a sanitized aggTrades CSV fixture inline (the
    // `CSV` const above) — no network. The transport-agnostic core never
    // opens a socket; the only egress path is the feature-gated live source.
    let events = parse_aggtrades(CSV, SymbolId(1)).unwrap();
    assert_eq!(events.len(), 3, "fixture parses offline (HBS-10)");
    let manifest = include_str!("../Cargo.toml");
    assert!(
        manifest.contains("reqwest") && manifest.contains("optional = true"),
        "network deps stay feature-gated (HBS-10/PD-4)"
    );
}

#[test]
fn hbs_2_writes_only_to_cold_historical_namespace() {
    let root = tmp("hbs2");
    let src = MockHistoricalSource { text: CSV.into() };
    bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "g", &cfg()).unwrap();
    assert!(historical_trades_file(&root, "BTCUSDT", "2026-08-01").exists());
    // HBS-2/W-6: NOTHING under cold/trades or raw — no collision with the live
    // recorder's namespace, and no destructive write path.
    assert!(
        !root.join("trades").exists(),
        "no cold/trades write (HBS-2)"
    );
    assert!(!root.join("raw").exists(), "no raw write (HBS-2)");
}

#[test]
fn hbs_3_provenance_and_fidelity_labeled() {
    let root = tmp("hbs3");
    let src = MockHistoricalSource { text: CSV.into() };
    bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "g", &cfg()).unwrap();
    let m = historical_manifest_file(&root, "BTCUSDT", "2026-08-01");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&m).unwrap()).unwrap();
    assert_eq!(json["source"], "external_archive");
    assert_eq!(json["fidelity"], "aggregated");
    assert_eq!(json["schema_ver"], mp_core::SCHEMA_VER);
    assert_eq!(json["rows"], 3);
    // Parquet footer carries the content source_log_hash (STO-8 / HBS-4).
    let f = historical_trades_file(&root, "BTCUSDT", "2026-08-01");
    let hash = mp_storage::parquet_trades::read_source_hash(&f)
        .unwrap()
        .unwrap();
    assert!(!hash.is_empty());
}

#[test]
fn hbs_7_resumable_across_dates() {
    let root = tmp("hbs7");
    let src = MockHistoricalSource { text: CSV.into() };
    // One date at a time (resumable per-date, HBS-7): two dates → two files,
    // each independently idempotent.
    bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "g", &cfg()).unwrap();
    bootstrap_day(&root, &src, "BTCUSDT", "2026-08-02", "g", &cfg()).unwrap();
    assert!(historical_trades_file(&root, "BTCUSDT", "2026-08-01").exists());
    assert!(historical_trades_file(&root, "BTCUSDT", "2026-08-02").exists());
    let again = bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "g", &cfg()).unwrap();
    assert_eq!(again.files_skipped, 1);
}

#[test]
fn hbs_9_check_config_rejects_unknown_fields() {
    // HBS-9/CONV-16: unknown keys error, never a silent default.
    assert!(parse_historical_config("bogus = 1").is_err());
    assert!(parse_historical_config("[historical]\n").is_err());
    // Default fidelity.
    assert_eq!(parse_historical_config("").unwrap().fidelity, "aggregated");
    // Explicit fidelity round-trips.
    assert_eq!(
        parse_historical_config("fidelity = \"tick\"")
            .unwrap()
            .fidelity,
        "tick"
    );
}

/// HBS-1 + HBS-8 acceptance tests — compiled and run only with the `live-http`
/// feature (the network transport the owner approved 2026-08-05). The archive
/// is emulated by a local mock HTTP server; no real egress (CONV-23).
#[cfg(feature = "live-http")]
mod live_http {
    use super::historical_trades_file;
    use mp_storage::{
        bootstrap_day, day_complete, historical_manifest_file, BinanceVisionSource,
        HistoricalConfig, HistoricalSource,
    };
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn make_zip(csv: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let opts = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mut w = zip::ZipWriter::new(&mut buf);
            w.start_file("BTCUSDT-aggTrades-2026-08-01.csv", opts)
                .expect("zip writer accepts entry (test fixture)");
            w.write_all(csv.as_bytes())
                .expect("zip writer accepts csv (test fixture)");
            w.finish().expect("zip closes (test fixture)");
        }
        buf.into_inner()
    }

    /// Minimal HTTP/1.1 fixture server (CONV-23: no real network). Sends
    /// `transient_failures` × 503 first, then `final_status` for every later
    /// request. Reports total request count and the request lines seen.
    fn spawn_archive_server(
        transient_failures: usize,
        final_status: u16,
        body: Vec<u8>,
    ) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let req_lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let r = Arc::clone(&requests);
        let lines = Arc::clone(&req_lines);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 8192];
                let Ok(n) = stream.read(&mut buf) else {
                    continue;
                };
                if n == 0 {
                    continue;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                lines
                    .lock()
                    .expect("test lock")
                    .push(req.lines().next().unwrap_or_default().to_string());
                let count = r.fetch_add(1, Ordering::SeqCst) + 1;
                let (status_line, out) = if count <= transient_failures {
                    ("503 Service Unavailable", Vec::<u8>::new())
                } else if final_status == 200 {
                    ("200 OK", body.clone())
                } else {
                    ("404 Not Found", Vec::<u8>::new())
                };
                let head = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&out);
            }
        });
        (format!("http://{addr}"), requests, req_lines)
    }

    #[test]
    fn hbs_1_downloads_live_zip_to_separate_namespace() {
        let root = std::env::temp_dir().join(format!("mphist-live-{}-hbs1", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("tmp dir");
        let (base, requests, req_lines) = spawn_archive_server(0, 200, make_zip(super::CSV));
        let cfg = HistoricalConfig {
            download_base_url: base,
            download_rate_per_sec: 0.0, // fixture: no sleep in tests
            ..HistoricalConfig::default()
        };
        let src = BinanceVisionSource::new(&cfg);
        // The full core pipeline drives the LIVE source end-to-end (HBS-1).
        let stats = bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "g", &cfg)
            .expect("bootstrap from live source");
        assert_eq!(stats.rows_written, 3);
        assert!(historical_trades_file(&root, "BTCUSDT", "2026-08-01").exists());
        // HBS-1/HBS-2: ONLY the cold/historical namespace is written.
        assert!(!root.join("trades").exists(), "no cold/trades write");
        assert!(!root.join("raw").exists(), "no raw write");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        let lines = req_lines.lock().expect("test lock");
        assert!(
            lines[0].contains(
                "/data/futures/um/daily/aggTrades/BTCUSDT/BTCUSDT-aggTrades-2026-08-01.zip"
            ),
            "HBS-1 must hit the Binance archive daily-aggTrades layout, got: {}",
            lines[0]
        );
    }

    #[test]
    fn hbs_8_retries_transient_http_with_backoff() {
        // Two transient 503s must be retried (backoff), third request succeeds.
        let (base, requests, _) = spawn_archive_server(2, 200, make_zip(super::CSV));
        let cfg = HistoricalConfig {
            download_base_url: base,
            download_rate_per_sec: 0.0,
            download_max_retries: 3,
            download_backoff_base_ms: 1,
            download_backoff_cap_ms: 2,
            ..HistoricalConfig::default()
        };
        let src = BinanceVisionSource::new(&cfg);
        let text = src.fetch_day("BTCUSDT", "2026-08-01").expect("retried 200");
        assert!(text.contains("50000"));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            3,
            "2 transient failures ⇒ 3 total requests (HBS-8)"
        );
    }

    #[test]
    fn hbs_8_not_found_is_not_retried() {
        // A definitive 404 (no such (symbol, date) in the archive) is NOT a
        // transient fault — one request, an error, never a fabricated day.
        let (base, requests, _) = spawn_archive_server(0, 404, Vec::new());
        let cfg = HistoricalConfig {
            download_base_url: base,
            download_rate_per_sec: 0.0,
            download_max_retries: 5,
            ..HistoricalConfig::default()
        };
        let src = BinanceVisionSource::new(&cfg);
        let err = src
            .fetch_day("BTCUSDT", "2078-01-01")
            .expect_err("404 errors");
        assert!(
            err.to_string().contains("404"),
            "error names the fault: {err}"
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "no retry on 404 (HBS-8)"
        );
    }

    #[test]
    fn hbs_7_completion_marker_tracks_completed_days() {
        // The mp-bootstrap date loop consults `day_complete` BEFORE fetching
        // so a completed (symbol, date) — parquet + manifest both present —
        // costs zero network on a re-run (HBS-4/7 resumability). `bootstrap_day`
        // itself stays deterministic and always re-reads the source; the marker
        // is the binary's skip guard.
        let root = std::env::temp_dir().join(format!("mphist-live-{}-hbs7", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("tmp dir");
        assert!(!day_complete(&root, "BTCUSDT", "2026-08-01"), "nothing yet");
        let (base, requests, _) = spawn_archive_server(0, 200, make_zip(super::CSV));
        let cfg = HistoricalConfig {
            download_base_url: base,
            download_rate_per_sec: 0.0,
            ..HistoricalConfig::default()
        };
        let src = BinanceVisionSource::new(&cfg);
        bootstrap_day(&root, &src, "BTCUSDT", "2026-08-01", "g", &cfg).expect("first day");
        assert!(
            day_complete(&root, "BTCUSDT", "2026-08-01"),
            "parquet + manifest ⇒ complete (HBS-7 marker)"
        );
        assert!(historical_manifest_file(&root, "BTCUSDT", "2026-08-01").exists());
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "one fetch built the day"
        );
    }
}
