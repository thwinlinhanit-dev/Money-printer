//! Acceptance tests for spec 030 (macro: FRED + HIP-3). Fixtures are
//! synthetic-representative FRED responses (no network, no real API key —
//! CONV-23, PD-2). Test names embed requirement IDs (CONV-21).

use mp_collectors::fred::{parse_fred_date, FredNormalizer};
use mp_collectors::hyperliquid::HyperliquidNormalizer;
use mp_collectors::Normalizer;
use mp_core::codec::{decode_event, encode_event};
use mp_core::{EventEnvelope, InstrumentKind, MarketEvent, Side, SymbolId, SymbolMeta, Venue};
use proptest::prelude::*;

fn norm_fred(n: &mut dyn Normalizer, recv: i64, series: &str, json: &str) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    // The FRED poller wraps the venue response with the series_id (see
    // fred.rs module docs).
    let wrapped = format!(r#"{{"series_id":"{series}","observations":{json}}}"#);
    n.normalize(recv, wrapped.as_bytes(), &mut out).unwrap();
    out
}

const OBSERVATIONS: &str = r#"[
  {"realtime_start":"2026-08-04","realtime_end":"2026-08-04","date":"2026-08-01","value":"4.70"},
  {"realtime_start":"2026-08-04","realtime_end":"2026-08-04","date":"2026-08-02","value":"4.71"},
  {"realtime_start":"2026-08-04","realtime_end":"2026-08-04","date":"2026-08-03","value":"."}
]"#;

#[test]
fn mac_1_hip3_via_existing_hyperliquid_normalizer() {
    // MAC-1: HIP-3 TradFi symbols flow through the EXISTING hyperliquid
    // normalizer with asset_class = tradfi_synthetic metadata — zero new code
    // paths. Seed the symbol with the TradFiSynthetic kind (the exact shape
    // mp-collector --hip3-symbols uses) and normalize a HIP-3 trade frame.
    let mut n = HyperliquidNormalizer::new();
    let coin = "xyz:XYZ100";
    n.symbols_mut().intern(Venue::Hyperliquid, coin, |id| {
        SymbolMeta::new(
            id,
            Venue::Hyperliquid,
            coin,
            "",
            "",
            InstrumentKind::TradFiSynthetic,
            f64::NAN,
            f64::NAN,
            f64::NAN,
        )
    });
    let meta = n
        .symbols()
        .lookup(Venue::Hyperliquid, coin)
        .and_then(|id| n.symbols().get(id));
    assert_eq!(
        meta.map(|m| m.kind),
        Some(InstrumentKind::TradFiSynthetic),
        "HIP-3 symbols must carry tradfi_synthetic asset_class metadata"
    );
    // A HIP-3 trades frame normalizes through the existing channels.
    let mut out = Vec::new();
    n.normalize(
        1,
        r#"{"channel":"trades","data":[{"coin":"xyz:XYZ100","side":"B","px":"25372.0","sz":"0.0353","time":1,"tid":7}]}"#
            .as_bytes(),
        &mut out,
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    match &out[0].body {
        MarketEvent::Trade {
            price, qty, side, ..
        } => {
            assert_eq!(*price, 25372.0);
            assert_eq!(*qty, 0.0353);
            assert_eq!(*side, Side::Buy);
        }
        other => panic!("expected Trade, got {other:?}"),
    }
}

#[test]
fn mac_2_fred_key_from_env_not_repo() {
    // PD-2 / MAC-2: no FRED_API_KEY-like value anywhere in the example
    // config or fixtures (the key is env-only).
    let example = include_str!("../macro.toml.example");
    // The example mentions `api_key` only in the docs URL text — no key is
    // ever assigned (env-only, MAC-2/PD-2).
    assert!(
        !example.lines().any(|l| l.contains("api_key")
            && !l.contains("api_key.html")
            && !l.contains("FRED_API_KEY")),
        "config must not carry an api_key assignment"
    );
    assert!(example.contains("FRED_API_KEY"));
    assert!(
        !example.lines().any(|l| l.contains("FRED_API_KEY =")),
        "key must not be assigned"
    );
    // Fixtures carry no key field.
    assert!(!OBSERVATIONS.contains("api_key"));
}

proptest! {
    #[test]
    fn mac_3_macropoint_event_variant_roundtrips(
        series_id in "[A-Z0-9]{2,12}",
        value in -1.0e6f64..1.0e6,
        date in -4_000_000_000_000_000_000i64..4_000_000_000_000_000_000i64,
    ) {
        let e = EventEnvelope::new(
            Venue::Fred,
            SymbolId(0),
            date,
            1,
            0,
            MarketEvent::MacroPoint { series_id, value, date },
        );
        let back = decode_event(&encode_event(&e).unwrap()).unwrap();
        prop_assert_eq!(e, back);
    }
}

#[test]
fn mac_4_normalization_deterministic() {
    let mut n1 = FredNormalizer::new();
    let mut n2 = FredNormalizer::new();
    let a = norm_fred(&mut n1, 42, "DGS10", OBSERVATIONS);
    let b = norm_fred(&mut n2, 42, "DGS10", OBSERVATIONS);
    assert_eq!(a, b, "same input must produce identical events (golden)");
    // Two published observations recorded; the "." observation is skipped.
    assert_eq!(a.len(), 2);
    match &a[0].body {
        MarketEvent::MacroPoint {
            series_id,
            value,
            date,
        } => {
            assert_eq!(series_id, "DGS10");
            assert_eq!(*value, 4.70);
            // 2026-08-01T00:00:00Z in ns.
            assert_eq!(*date, parse_fred_date("2026-08-01").unwrap());
        }
        other => panic!("expected MacroPoint, got {other:?}"),
    }
    assert_eq!(a[1].recv_ts_ns, 42);
}

#[test]
fn mac_5_fidelity_labeled_correlation_not_execution() {
    // MAC-5: macro data is correlation-grade only. The collectors crate must
    // have no strategy/execution dependency (PD-4), and HIP-3 symbols are
    // explicitly TradFiSynthetic (never treated as execution instruments).
    let manifest = include_str!("../Cargo.toml");
    assert!(
        !manifest.contains("mp-strategies"),
        "collectors must not depend on strategies (MAC-5/PD-4)"
    );
    // The example config documents correlation-grade, not execution-grade.
    let example = include_str!("../macro.toml.example").to_lowercase();
    assert!(
        example.contains("correlation-grade") && example.contains("not execution-grade"),
        "fidelity must be labeled correlation-grade (MAC-5)"
    );
}

#[test]
fn mac_8_fixtures_no_network_no_real_key() {
    // The FRED normalizer is a pure function of the recorded fixture — no
    // network, no key (CONV-23 / PD-2). "." observations are skipped, never
    // invented; malformed input errors instead of panicking (CONV-15).
    let mut n = FredNormalizer::new();
    let mut out = Vec::new();
    assert!(n.normalize(1, b"not json", &mut out).is_err());
    assert!(
        n.normalize(1, b"{}", &mut out).is_err(),
        "wrapped response must have series_id"
    );
    let events = norm_fred(&mut n, 1, "SOFR", OBSERVATIONS);
    assert_eq!(events.len(), 2);
}

#[cfg(feature = "live-http")]
#[test]
fn mac_7_check_config_rejects_unknown_fields() {
    // CONV-16: the real binary rejects unknown config keys on --check-config.
    let bin = env!("CARGO_BIN_EXE_mp-macro");
    let dir = std::env::temp_dir().join(format!("mp-mac7-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.toml");
    std::fs::write(&good, "data_dir = \"data\"\nseries = [\"DGS10\"]\n").unwrap();
    let bad = dir.join("bad.toml");
    std::fs::write(
        &bad,
        "data_dir = \"data\"\napi_key = \"0123456789abcdef0123456789abcdef\"\n",
    )
    .unwrap();

    let ok = std::process::Command::new(bin)
        .args(["--check-config", "--config"])
        .arg(&good)
        .output()
        .unwrap();
    assert!(ok.status.success(), "good config must pass: {:?}", ok);

    let err = std::process::Command::new(bin)
        .args(["--check-config", "--config"])
        .arg(&bad)
        .output()
        .unwrap();
    assert!(
        !err.status.success(),
        "unknown field must be rejected (CONV-16)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
