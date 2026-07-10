//! The collector driver: transport → normalizer → event sink, with disconnect
//! handling and parse-failure limits (COL-1, COL-6). Transport-agnostic and
//! synchronous; the live async WS transport plugs in behind [`Transport`].

use crate::backoff::Backoff;
use crate::normalize::{HealthCounters, Normalizer};
use crate::transport::{Transport, TransportEvent};
use mp_core::{EventEnvelope, MarketEvent, SnapshotReason, StatusKind};

/// Driver configuration.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Consecutive parse failures on a stream before forcing a reconnect (COL-6).
    pub max_consecutive_parse_failures: u32,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            max_consecutive_parse_failures: 10,
        }
    }
}

/// Why [`Collector::drive`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveOutcome {
    /// Transport had nothing more to yield (mock: script exhausted).
    Exhausted,
    /// Transport reported a disconnect; caller should reconnect with backoff.
    Disconnected,
    /// Too many consecutive parse failures; caller should reconnect (COL-6).
    ParseFailureLimit,
}

/// Drives a [`Normalizer`] over a [`Transport`].
pub struct Collector<N: Normalizer> {
    normalizer: N,
    counters: HealthCounters,
    consecutive_parse_failures: u32,
    config: CollectorConfig,
}

impl<N: Normalizer> Collector<N> {
    pub fn new(normalizer: N, config: CollectorConfig) -> Self {
        Self {
            normalizer,
            counters: HealthCounters::default(),
            consecutive_parse_failures: 0,
            config,
        }
    }

    pub fn counters(&self) -> &HealthCounters {
        &self.counters
    }

    pub fn normalizer(&self) -> &N {
        &self.normalizer
    }

    /// Consume one transport until it disconnects or exhausts, appending
    /// normalized events to `out`.
    pub fn drive(
        &mut self,
        transport: &mut dyn Transport,
        out: &mut Vec<EventEnvelope>,
    ) -> DriveOutcome {
        while let Some(ev) = transport.poll() {
            match ev {
                TransportEvent::Frame {
                    recv_ts_ns,
                    payload,
                } => {
                    let before = out.len();
                    match self.normalizer.normalize(recv_ts_ns, &payload, out) {
                        Ok(()) => {
                            self.consecutive_parse_failures = 0;
                            self.account(&out[before..]);
                        }
                        Err(_e) => {
                            self.counters.messages_dropped += 1;
                            self.consecutive_parse_failures += 1;
                            if self.consecutive_parse_failures
                                >= self.config.max_consecutive_parse_failures
                            {
                                return DriveOutcome::ParseFailureLimit;
                            }
                        }
                    }
                }
                TransportEvent::Disconnected => {
                    self.normalizer.reset_books();
                    self.counters.reconnects += 1;
                    return DriveOutcome::Disconnected;
                }
            }
        }
        DriveOutcome::Exhausted
    }

    fn account(&mut self, new_events: &[EventEnvelope]) {
        for e in new_events {
            self.counters.events_emitted += 1;
            match &e.body {
                MarketEvent::Status {
                    kind: StatusKind::GapDetected,
                    ..
                } => self.counters.gaps_detected += 1,
                MarketEvent::BookSnapshot {
                    reason: SnapshotReason::GapResync,
                    ..
                } => self.counters.book_resyncs += 1,
                _ => {}
            }
        }
    }

    /// Reconnecting run loop (COL-1). Pulls transports from `connect` until it
    /// returns `None`; on a disconnect or parse-failure limit it applies
    /// `backoff` and reconnects. Returns the sequence of backoff delays used —
    /// in the live impl these become sleeps; here they are asserted in tests.
    pub fn run_reconnecting(
        &mut self,
        mut connect: impl FnMut() -> Option<Box<dyn Transport>>,
        backoff: &mut Backoff,
        out: &mut Vec<EventEnvelope>,
    ) -> Vec<u64> {
        let mut delays = Vec::new();
        while let Some(mut t) = connect() {
            match self.drive(t.as_mut(), out) {
                DriveOutcome::Exhausted => break,
                DriveOutcome::Disconnected | DriveOutcome::ParseFailureLimit => {
                    self.consecutive_parse_failures = 0;
                    delays.push(backoff.next_delay_ms());
                }
            }
        }
        delays
    }
}
