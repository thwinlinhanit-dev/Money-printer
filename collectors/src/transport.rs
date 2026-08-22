//! Transport abstraction (COL-1). The collector driver is written against this
//! trait; the live WebSocket implementation is deferred until the network
//! dependency is approved. [`MockTransport`] drives all offline tests, scripting
//! disconnects and gaps (COL-14).

/// One thing that can come off a connection.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// A raw frame with its local receive time (stamped at read, COL-5).
    Frame { recv_ts_ns: i64, payload: Vec<u8> },
    /// The connection dropped; the driver must reconnect (with backoff).
    Disconnected,
}

/// Backpressure telemetry exported to the collector health/status path
/// (spec 013 BKP-3). Defaults to zero for transports that track nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportMetrics {
    pub dropped_frames: u64,
    pub queue_high_water: usize,
}

/// A source of transport events. `poll` returns `None` when the transport has
/// nothing more to yield right now (in the mock: script exhausted).
pub trait Transport {
    fn poll(&mut self) -> Option<TransportEvent>;

    /// Consume the aggregated loss/high-water telemetry since the prior call.
    /// The live WS transport implements this (BKP-3/INT-3); the mock reports
    /// nothing and the tee forwards its inner transport's metrics. Default so
    /// `Box<dyn Transport>` stays usable by every collector binary.
    fn take_metrics(&mut self) -> TransportMetrics {
        TransportMetrics::default()
    }
}

/// Scripted in-memory transport for tests. No network, fully deterministic.
#[derive(Debug, Default)]
pub struct MockTransport {
    script: std::collections::VecDeque<TransportEvent>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a frame.
    pub fn push_frame(&mut self, recv_ts_ns: i64, payload: impl Into<Vec<u8>>) -> &mut Self {
        self.script.push_back(TransportEvent::Frame {
            recv_ts_ns,
            payload: payload.into(),
        });
        self
    }

    /// Queue a disconnect.
    pub fn push_disconnect(&mut self) -> &mut Self {
        self.script.push_back(TransportEvent::Disconnected);
        self
    }
}

impl Transport for MockTransport {
    fn poll(&mut self) -> Option<TransportEvent> {
        self.script.pop_front()
    }
}

/// A [`Transport`] wrapper that tees every raw frame to an append-only text
/// file — `recv_ts_ns` tab-prefixed, one frame per line (spec 031 OPT-3,
/// COL-9: capture frames verbatim pre-parse so a schema/venue drift can be
/// re-normalized later). Written at the transport boundary, so the bytes are
/// exactly what the venue sent. Never rewrites existing lines (append-only,
/// W-6).
///
/// v1 writes uncompressed `.ndjson`; zstd compression is deferred (see spec
/// 031 Decisions — adding a zstd dependency needs owner sign-off).
pub struct TeeTransport<T: Transport> {
    inner: T,
    sink: Option<std::fs::File>,
}

impl<T: Transport> TeeTransport<T> {
    pub fn new(inner: T, sink: std::fs::File) -> Self {
        Self {
            inner,
            sink: Some(sink),
        }
    }

    pub fn take_sink(&mut self) -> Option<std::fs::File> {
        self.sink.take()
    }
}

impl<T: Transport> Transport for TeeTransport<T> {
    fn poll(&mut self) -> Option<TransportEvent> {
        let ev = self.inner.poll();
        if let Some(TransportEvent::Frame {
            recv_ts_ns,
            payload,
        }) = &ev
        {
            if let Some(f) = self.sink.as_mut() {
                use std::io::Write;
                let line: std::borrow::Cow<[u8]> = if payload.ends_with(b"\n") {
                    payload.into()
                } else {
                    [payload.as_slice(), b"\n"].concat().into()
                };
                let _ = write!(f, "{recv_ts_ns}\t");
                let _ = f.write_all(&line);
            }
        }
        ev
    }

    /// The tee tracks no loss of its own (it consumes every frame its inner
    /// yields), so forward the inner transport's backpressure telemetry
    /// verbatim. Without this override, raw_capture mode zeroed out the
    /// inner's drops and the collector never emitted
    /// `Status::BackpressureDrop` (BKP-3/INT-1; audit).
    fn take_metrics(&mut self) -> TransportMetrics {
        self.inner.take_metrics()
    }
}
