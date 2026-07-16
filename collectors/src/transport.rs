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

/// A source of transport events. `poll` returns `None` when the transport has
/// nothing more to yield right now (in the mock: script exhausted).
pub trait Transport {
    fn poll(&mut self) -> Option<TransportEvent>;
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
