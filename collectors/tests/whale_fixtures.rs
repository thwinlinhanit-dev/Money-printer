//! Acceptance tests for spec 028 (Hyperliquid whale positions). Fixtures are
//! SYNTHETIC-REPRESENTATIVE frames built to the documented API shapes — no
//! network (CONV-23). Test names embed requirement IDs (CONV-21).

#[cfg(feature = "live-http")]
use mp_collectors::hyperliquid_positions::leaderboard_addresses;
use mp_collectors::hyperliquid_positions::{gap_detected, HyperliquidPositionsNormalizer};
use mp_collectors::Normalizer;
use mp_core::codec::{decode_event, encode_event};
use mp_core::{EventEnvelope, MarketEvent, StatusKind, SymbolId, Venue};
use proptest::prelude::*;

fn norm(n: &mut dyn Normalizer, recv: i64, json: &str) -> Vec<EventEnvelope> {
    let mut out = Vec::new();
    n.normalize(recv, json.as_bytes(), &mut out).unwrap();
    out
}

/// A synthetic `clearinghouseState` response wrapped with the address (the
/// shape the mp-whale poller feeds the normalizer — see module docs).
fn wrapped_state(address: &str, positions_json: &str) -> String {
    format!(r#"{{"address":"{address}","time":1785900000123,"assetPositions":{positions_json}}}"#)
}

const TWO_POSITIONS: &str = r#"[
  {"position": {"coin":"BTC","szi":"1.5","entryPx":"97000.0","leverage":{"type":"isolated","value":"10"},"liquidationPx":"89000.0","positionValue":"145500.0"}},
  {"position": {"coin":"ETH","szi":"-20.0","entryPx":"3500.0","leverage":{"type":"cross","value":"5"},"liquidationPx":null,"positionValue":"70000.0"}}
]"#;

#[cfg(feature = "live-http")]
#[test]
fn whl_1_polls_public_rest_no_auth() {
    // The request bodies carry only the public info type + user/window — no
    // key, no auth, no secret (PD-2).
    use mp_collectors::hyperliquid_positions::rest::{clearinghouse_body, leaderboard_body};
    let lb = leaderboard_body("7d");
    assert_eq!(lb["type"], "leaderboard");
    assert_eq!(lb["timeWindow"], "7d");
    assert!(lb.get("api_key").is_none() && lb.get("secret").is_none());

    let ch = clearinghouse_body("0xdeadbeef");
    assert_eq!(ch["type"], "clearinghouseState");
    assert_eq!(ch["user"], "0xdeadbeef");
    assert!(ch.get("api_key").is_none() && ch.get("secret").is_none());

    // Recorded leaderboard fixture parses into ranked opaque addresses.
    let fixture = r#"[{"name":"alice","address":"0xaaaa000000000000000000000000000000000001","pnl":"1234"},
                      {"name":"bob","address":"0xbbbb000000000000000000000000000000000002","pnl":"900"}]"#;
    let addrs = leaderboard_addresses(fixture.as_bytes()).unwrap();
    assert_eq!(addrs.len(), 2);
    assert!(addrs.iter().all(|a| a.starts_with("0x")));
}

#[cfg(feature = "live-http")]
#[test]
fn whl_1_leaderboard_parse_errors_on_empty() {
    // An empty/undocumented leaderboard response must fail loudly, not look
    // like a successful (empty) top-N poll.
    assert!(leaderboard_addresses(b"[]").is_err());
    assert!(leaderboard_addresses(b"not json").is_err());
}

proptest! {
    #[test]
    fn whl_2_whaleposition_event_variant_roundtrips(
        address in "0x[0-9a-f]{8}",
        size in -1.0e9f64..1.0e9,
        entry in -1.0e9f64..1.0e9,
        leverage in 0.0f64..1.0e3,
        liq_price in -1.0e9f64..1.0e9,
    ) {
        let e = EventEnvelope::new(
            Venue::Hyperliquid,
            SymbolId(1),
            0,
            1,
            0,
            MarketEvent::WhalePosition { address, size, entry, leverage, liq_price },
        );
        let back = decode_event(&encode_event(&e).unwrap()).unwrap();
        prop_assert_eq!(e, back);
    }
}

#[test]
fn whl_3_addresses_are_opaque_no_external_labels() {
    let mut n = HyperliquidPositionsNormalizer::new();
    let events = norm(
        &mut n,
        1,
        &wrapped_state("0xaaaabbbbccccddddeeeeffff0000111122223333", TWO_POSITIONS),
    );
    assert_eq!(events.len(), 2);
    for ev in &events {
        match &ev.body {
            MarketEvent::WhalePosition { address, .. } => {
                assert!(
                    address.starts_with("0x"),
                    "address must stay an opaque 0x id"
                );
                assert!(
                    !address.contains("alice") && !address.contains("name"),
                    "external labels must never enter the record (WHL-3)"
                );
            }
            other => panic!("expected WhalePosition, got {other:?}"),
        }
    }
}

#[test]
fn whl_4_normalization_is_deterministic() {
    let mut n1 = HyperliquidPositionsNormalizer::new();
    let mut n2 = HyperliquidPositionsNormalizer::new();
    let a = norm(&mut n1, 42, &wrapped_state("0xaaaa", TWO_POSITIONS));
    let b = norm(&mut n2, 42, &wrapped_state("0xaaaa", TWO_POSITIONS));
    // NaN != NaN, so compare encoded byte streams instead of PartialEq.
    let enc = |evs: &[EventEnvelope]| -> Vec<Vec<u8>> {
        evs.iter().map(|e| encode_event(e).unwrap()).collect()
    };
    assert_eq!(
        enc(&a),
        enc(&b),
        "same input must produce identical events (golden)"
    );
    // Field mapping spot-check (entry/leverage/liq from the fixture).
    match &a[0].body {
        MarketEvent::WhalePosition {
            size,
            entry,
            leverage,
            liq_price,
            ..
        } => {
            assert_eq!(*size, 1.5);
            assert_eq!(*entry, 97000.0);
            assert_eq!(*leverage, 10.0);
            assert_eq!(*liq_price, 89000.0);
        }
        other => panic!("expected WhalePosition, got {other:?}"),
    }
    // Null liquidationPx → NaN sentinel, never a real price.
    match &a[1].body {
        MarketEvent::WhalePosition {
            size, liq_price, ..
        } => {
            assert_eq!(*size, -20.0);
            assert!(liq_price.is_nan());
        }
        other => panic!("expected WhalePosition, got {other:?}"),
    }
}

#[test]
fn whl_5_positions_are_data_only_not_strategy_input() {
    // PD-4 / WHL-5: the collectors crate must not depend on strategies, and
    // whale positions are a pure data type in core (no strategy/execution
    // wiring). Mechanical check on the manifest.
    let manifest = include_str!("../Cargo.toml");
    assert!(
        !manifest.contains("mp-strategies"),
        "collectors must not depend on the strategies crate (WHL-5/PD-4)"
    );
}

#[test]
fn whl_7_gap_detection_on_missed_poll() {
    // The event mp-whale records when a top-N or watchlist poll fails (WHL-7).
    let gap = gap_detected(
        SymbolId(0),
        123,
        "whale poll failed: top=true watch=false".into(),
    );
    assert_eq!(gap.recv_ts_ns, 123);
    match gap.body {
        MarketEvent::Status {
            kind: StatusKind::GapDetected,
            detail,
        } => {
            assert!(detail.contains("whale poll failed"));
        }
        other => panic!("expected Status::GapDetected, got {other:?}"),
    }
}

#[cfg(feature = "live-http")]
#[test]
fn whl_8_check_config_rejects_unknown_fields() {
    // CONV-16: config is deny_unknown_fields. The real binary must reject an
    // unknown key on --check-config.
    let bin = env!("CARGO_BIN_EXE_mp-whale");
    let dir = std::env::temp_dir().join(format!("mp-whl8-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.toml");
    std::fs::write(&good, "data_dir = \"data\"\ntop_n = 10\n").unwrap();
    let bad = dir.join("bad.toml");
    std::fs::write(&bad, "data_dir = \"data\"\nbogus_key = 1\n").unwrap();

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

#[test]
fn whl_9_fixtures_no_network_and_nan_fail_closed() {
    // No panic on malformed input (CONV-15) + NaN never recorded as real.
    let mut n = HyperliquidPositionsNormalizer::new();
    let mut out = Vec::new();
    assert!(n.normalize(1, b"not json", &mut out).is_err());
    assert!(
        n.normalize(1, b"{}", &mut out).is_err(),
        "wrapped response must have address"
    );
    // Malformed positions are skipped, never invented.
    let bad = wrapped_state(
        "0xaaaa",
        r#"[{"position":{"coin":"BTC"}},
            {"position":{"coin":"ETH","szi":"abc","entryPx":"1","leverage":{"value":"1"}}}]"#,
    );
    let events = norm(&mut n, 1, &bad);
    assert!(
        events.is_empty(),
        "malformed positions must be skipped: {events:?}"
    );
}
