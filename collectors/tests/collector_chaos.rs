//! Mock-venue chaos tests (COL-14): reconnect, gap accounting, parse-failure
//! limit — all with no network. Test names embed IDs (CONV-21).

use mp_collectors::{
    Backoff, Collector, CollectorConfig, DriveOutcome, MockTransport, Transport, TransportEvent,
};
use mp_collectors::{BybitNormalizer, RateBudget, Staleness};

const SNAP1: &str = r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":1,"data":{"s":"BTCUSDT","b":[["100.0","5"]],"a":[["101.0","4"]],"u":1}}"#;
const SNAP2: &str = r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":9,"data":{"s":"BTCUSDT","b":[["100.0","6"]],"a":[["101.0","2"]],"u":50}}"#;

#[test]
fn col_1_reconnects_after_disconnect_with_backoff() {
    let mut c = Collector::new(BybitNormalizer::new(), CollectorConfig::default());
    let mut backoff = Backoff::new(10, 1000, 0xF00D);

    // First transport: a snapshot then a disconnect. Second: a resync snapshot.
    // Third connect returns None → stop.
    let mut step = 0;
    let connect = move || -> Option<Box<dyn Transport>> {
        step += 1;
        match step {
            1 => {
                let mut t = MockTransport::new();
                t.push_frame(1, SNAP1).push_disconnect();
                Some(Box::new(t))
            }
            2 => {
                let mut t = MockTransport::new();
                t.push_frame(2, SNAP2);
                Some(Box::new(t))
            }
            _ => None,
        }
    };

    let mut out = Vec::new();
    let delays = c.run_reconnecting(connect, &mut backoff, &mut out);

    assert_eq!(c.counters().reconnects, 1, "one disconnect handled");
    assert_eq!(delays.len(), 1, "one backoff applied");
    assert!(delays[0] <= 20, "full-jitter delay within first ceiling");
    // The reconnect delivered the resync snapshot after reset_books().
    assert!(
        c.counters().book_resyncs >= 1,
        "resync snapshot after reconnect"
    );
}

#[test]
fn col_6_parse_failure_limit_forces_reconnect() {
    let mut c = Collector::new(
        BybitNormalizer::new(),
        CollectorConfig {
            max_consecutive_parse_failures: 3,
        },
    );
    let mut t = MockTransport::new();
    for _ in 0..5 {
        t.push_frame(1, b"garbage".to_vec());
    }
    let mut out = Vec::new();
    let outcome = c.drive(&mut t, &mut out);
    assert_eq!(outcome, DriveOutcome::ParseFailureLimit);
    assert_eq!(c.counters().messages_dropped, 3);
    assert_eq!(out.len(), 0);
}

#[test]
fn col_6_good_frame_resets_failure_streak() {
    let mut c = Collector::new(
        BybitNormalizer::new(),
        CollectorConfig {
            max_consecutive_parse_failures: 3,
        },
    );
    let mut t = MockTransport::new();
    t.push_frame(1, b"garbage".to_vec()) // fail 1
        .push_frame(1, b"garbage".to_vec()) // fail 2
        .push_frame(1, SNAP1) // success → resets streak
        .push_frame(1, b"garbage".to_vec()) // fail 1 again
        .push_frame(1, b"garbage".to_vec()); // fail 2 — under limit
    let mut out = Vec::new();
    let outcome = c.drive(&mut t, &mut out);
    assert_eq!(outcome, DriveOutcome::Exhausted, "never hit the limit");
    assert_eq!(c.counters().messages_dropped, 4);
    assert_eq!(c.counters().events_emitted, 1);
}

#[test]
fn col_7_gap_accounted_in_counters() {
    let mut c = Collector::new(BybitNormalizer::new(), CollectorConfig::default());
    let mut t = MockTransport::new();
    t.push_frame(1, SNAP1)
        .push_frame(2, r#"{"topic":"orderbook.50.BTCUSDT","type":"delta","ts":2,"data":{"s":"BTCUSDT","b":[["100.0","1"]],"a":[],"u":5}}"#.as_bytes().to_vec());
    let mut out = Vec::new();
    c.drive(&mut t, &mut out);
    assert_eq!(c.counters().gaps_detected, 1);
}

// ---- policy units -----------------------------------------------------------

#[test]
fn col_1_backoff_is_bounded_and_deterministic() {
    let mut a = Backoff::new(10, 500, 123);
    let mut b = Backoff::new(10, 500, 123);
    let seq_a: Vec<u64> = (0..8).map(|_| a.next_delay_ms()).collect();
    let seq_b: Vec<u64> = (0..8).map(|_| b.next_delay_ms()).collect();
    assert_eq!(seq_a, seq_b, "same seed ⇒ same sequence (CONV-11)");
    assert!(seq_a.iter().all(|&d| d <= 500), "never exceeds cap");
    a.reset();
    assert_eq!(a.attempt(), 0);
}

#[test]
fn col_4_rate_budget_refills_over_time() {
    // 5 tokens, refill 10/sec.
    let mut rb = RateBudget::new(5.0, 10.0, 0);
    assert!(rb.try_take(0, 5.0), "spend full budget");
    assert!(!rb.try_take(0, 1.0), "empty now");
    // 0.5s later ⇒ +5 tokens.
    assert!(rb.try_take(500_000_000, 5.0));
    assert!(!rb.try_take(500_000_000, 0.1));
}

#[test]
fn col_2_staleness_flags_quiet_streams() {
    let mut s = Staleness::new(1_000_000_000); // 1s default
    s.observe("publicTrade.BTCUSDT", 0);
    s.observe("orderbook.50.BTCUSDT", 0);
    // 2s later, both are stale.
    let stale = s.stale_streams(2_000_000_000);
    assert_eq!(stale.len(), 2);
    // A fresh observation clears one.
    s.observe("publicTrade.BTCUSDT", 2_000_000_000);
    let stale = s.stale_streams(2_500_000_000);
    assert_eq!(stale, vec!["orderbook.50.BTCUSDT".to_string()]);
}

#[test]
fn col_14_disconnect_event_shape() {
    // Sanity: the mock yields exactly what was scripted, in order.
    let mut t = MockTransport::new();
    t.push_frame(7, b"x".to_vec()).push_disconnect();
    assert!(matches!(
        t.poll(),
        Some(TransportEvent::Frame { recv_ts_ns: 7, .. })
    ));
    assert!(matches!(t.poll(), Some(TransportEvent::Disconnected)));
    assert!(t.poll().is_none());
}
