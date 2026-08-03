//! Live public-market-data WebSocket transport (COL-1). Behind the `live-ws`
//! feature. **PD-1: this connects to PUBLIC market-data endpoints only** — it
//! sends subscribe frames and reads data. It has no auth, no signing, and
//! cannot place orders. Order flow lives in `mp-oms` behind its own boundary.
//!
//! Bridges an async tokio-tungstenite socket to the synchronous
//! [`Transport`](crate::transport::Transport) trait via a bounded channel, so
//! the whole `Collector` driver stays transport-agnostic and unit-testable.

use crate::backpressure::BackpressurePolicy;
use crate::transport::{Transport, TransportEvent};
use futures_util::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// Endpoint + subscription messages for one venue connection. Endpoints are
/// public; put no credentials here.
#[derive(Debug, Clone)]
pub struct WsEndpoint {
    /// Public `wss://` URL (e.g. Bybit `wss://stream.bybit.com/v5/public/linear`).
    pub url: String,
    /// JSON subscribe frames to send on connect (venue-specific).
    pub subscribe: Vec<String>,
    /// Frame/message caps so a venue cannot memory-exhaust the process.
    /// Defaults: 1 MiB frames / 4 MiB messages (public market data is byte-sized).
    pub limits: WsLimits,
}

impl WsEndpoint {
    /// Convenience constructor with the default frame limits.
    pub fn new(url: impl Into<String>, subscribe: Vec<String>) -> Self {
        Self {
            url: url.into(),
            subscribe,
            limits: WsLimits::default(),
        }
    }
}

/// WebSocket frame/message size caps (security: bound attacker-controlled
/// memory). Applied on connect; over-limit frames fail the connection.
#[derive(Debug, Clone, Copy)]
pub struct WsLimits {
    pub max_frame_size: usize,
    pub max_message_size: usize,
}

impl Default for WsLimits {
    fn default() -> Self {
        Self {
            max_frame_size: 1024 * 1024,      // 1 MiB
            max_message_size: 4 * 1024 * 1024, // 4 MiB
        }
    }
}

/// A live transport fed by a background tokio task. `poll` is non-blocking.
pub struct WsTransport {
    queue: FrameQueue,
    _rt: tokio::runtime::Runtime,
}

/// Backpressure telemetry exported to the collector health/status path.
#[derive(Debug, Clone, Copy, Default)]
pub struct TransportMetrics {
    pub dropped_frames: u64,
    pub queue_high_water: usize,
}

#[derive(Default)]
struct QueueState {
    frames: VecDeque<TransportEvent>,
    dropped_frames: u64,
    queue_high_water: usize,
    closed: bool,
    disconnect_pending: bool,
}

#[derive(Clone)]
struct FrameQueue {
    state: Arc<(Mutex<QueueState>, Condvar)>,
    capacity: usize,
    policy: BackpressurePolicy,
}

enum PushResult {
    Queued,
    Dropped,
    TimedOut,
}

impl FrameQueue {
    fn new(capacity: usize, policy: BackpressurePolicy) -> Self {
        Self {
            state: Arc::new((Mutex::new(QueueState::default()), Condvar::new())),
            capacity: capacity.max(1),
            policy,
        }
    }

    fn push(&self, event: TransportEvent) -> PushResult {
        let (lock, available) = &*self.state;
        let mut state = lock.lock().expect("frame queue mutex poisoned");
        match self.policy {
            BackpressurePolicy::Unbounded => {}
            BackpressurePolicy::DropNewest if state.frames.len() >= self.capacity => {
                state.dropped_frames += 1;
                return PushResult::Dropped;
            }
            BackpressurePolicy::DropOldest if state.frames.len() >= self.capacity => {
                let _ = state.frames.pop_front();
                state.dropped_frames += 1;
            }
            BackpressurePolicy::Block => {
                while state.frames.len() >= self.capacity && !state.closed {
                    let (next, timeout) = available
                        .wait_timeout(state, Duration::from_millis(100))
                        .expect("frame queue mutex poisoned");
                    state = next;
                    if timeout.timed_out() && state.frames.len() >= self.capacity {
                        state.closed = true;
                        return PushResult::TimedOut;
                    }
                }
                if state.closed {
                    return PushResult::TimedOut;
                }
            }
            _ => {}
        }
        state.frames.push_back(event);
        state.queue_high_water = state.queue_high_water.max(state.frames.len());
        PushResult::Queued
    }

    fn poll(&self) -> Option<TransportEvent> {
        let (lock, available) = &*self.state;
        let mut state = lock.lock().expect("frame queue mutex poisoned");
        let event = state.frames.pop_front();
        if event.is_some() {
            available.notify_one();
            return event;
        }
        if state.closed && state.disconnect_pending {
            state.disconnect_pending = false;
            return Some(TransportEvent::Disconnected);
        }
        None
    }

    fn close(&self) {
        let (lock, available) = &*self.state;
        let mut state = lock.lock().expect("frame queue mutex poisoned");
        state.closed = true;
        state.disconnect_pending = true;
        available.notify_all();
    }

    fn take_metrics(&self) -> TransportMetrics {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("frame queue mutex poisoned");
        let metrics = TransportMetrics {
            dropped_frames: state.dropped_frames,
            queue_high_water: state.queue_high_water,
        };
        state.dropped_frames = 0;
        metrics
    }
}

impl WsTransport {
    /// Connect and start streaming. The `recv_ts_ns` clock is the OS clock
    /// stamped at socket read (COL-5) — the one place a collector reads wall
    /// time, which is allowed (it is not a decision path).
    pub fn connect(endpoint: WsEndpoint, buffer: usize) -> std::io::Result<Self> {
        Self::connect_with_policy(endpoint, buffer, BackpressurePolicy::default())
    }

    /// Connect with a specific backpressure policy (BKP-1).
    pub fn connect_with_policy(
        endpoint: WsEndpoint,
        buffer: usize,
        policy: BackpressurePolicy,
    ) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let queue = FrameQueue::new(buffer, policy);
        let producer = queue.clone();

        rt.spawn(async move {
            if let Err(e) = run(endpoint, producer.clone()).await {
                tracing::warn!(error = %e, "ws task ended");
            }
            producer.close();
        });

        Ok(Self { queue, _rt: rt })
    }

    /// Consume the aggregated loss/high-water telemetry since the prior call.
    /// The collector writes a durable status event and invalidates its book for
    /// every loss batch (BKP-3 and INT-3).
    pub fn take_metrics(&self) -> TransportMetrics {
        self.queue.take_metrics()
    }
}

impl Transport for WsTransport {
    fn poll(&mut self) -> Option<TransportEvent> {
        self.queue.poll()
    }
}

fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(_) => 0,
    }
}

async fn run(
    endpoint: WsEndpoint,
    queue: FrameQueue,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // tungstenite 0.24 exposes the limits as struct fields, not setters.
    let cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_frame_size: Some(endpoint.limits.max_frame_size),
        max_message_size: Some(endpoint.limits.max_message_size),
        ..Default::default()
    };
    let (ws, _resp) = tokio_tungstenite::connect_async_with_config(&endpoint.url, Some(cfg), false).await?;
    let (mut write, mut read) = ws.split();

    for sub in &endpoint.subscribe {
        write.send(Message::Text(sub.clone())).await?;
    }

    while let Some(msg) = read.next().await {
        let payload = match msg? {
            Message::Text(t) => t.into_bytes(),
            Message::Binary(b) => b,
            Message::Ping(p) => {
                write.send(Message::Pong(p)).await?;
                continue;
            }
            Message::Close(_) => break,
            _ => continue,
        };

        let ev = TransportEvent::Frame {
            recv_ts_ns: now_ns(),
            payload,
        };

        match queue.push(ev) {
            PushResult::Queued => {}
            PushResult::Dropped => tracing::warn!("ws frame dropped by backpressure policy"),
            PushResult::TimedOut => {
                tracing::warn!("ws block policy timed out; forcing reconnect");
                break;
            }
        }
    }
    Ok(())
}

/// Public endpoint presets (URLs only — no credentials). Verify against current
/// venue docs at implementation time; they drift (spec 002 pitfall #2).
pub mod endpoints {
    /// Bybit v5 linear public stream.
    pub const BYBIT_LINEAR: &str = "wss://stream.bybit.com/v5/public/linear";
    /// Binance USDⓈ-M Futures raw WS base (dynamic SUBSCRIBE method).
    /// Must be fstream — spot `stream.binance.com` has no markPrice/funding/forceOrder.
    pub const BINANCE_FUTURES: &str = "wss://fstream.binance.com/ws";
    /// Binance USDⓈ-M Futures combined-stream base (append `?streams=a/b/c`).
    /// Preferred for the live collector — streams are active on connect.
    pub const BINANCE_FUTURES_COMBINED: &str = "wss://fstream.binance.com/stream";
    /// OKX v5 public.
    pub const OKX_PUBLIC: &str = "wss://ws.okx.com:8443/ws/v5/public";
    /// Coinbase Advanced Trade market data.
    pub const COINBASE_ADVANCED: &str = "wss://advanced-trade-ws.coinbase.com";
    /// Kraken Futures.
    pub const KRAKEN_FUTURES: &str = "wss://futures.kraken.com/ws/v1";
    /// Hyperliquid.
    pub const HYPERLIQUID: &str = "wss://api.hyperliquid.xyz/ws";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(byte: u8) -> TransportEvent {
        TransportEvent::Frame {
            recv_ts_ns: byte as i64,
            payload: vec![byte],
        }
    }

    fn byte(event: TransportEvent) -> u8 {
        match event {
            TransportEvent::Frame { payload, .. } => payload[0],
            TransportEvent::Disconnected => panic!("expected frame"),
        }
    }

    #[test]
    fn bkp_1_block_policy_waits_without_loss() {
        let queue = FrameQueue::new(1, BackpressurePolicy::Block);
        assert!(matches!(queue.push(frame(1)), PushResult::Queued));
        let producer = queue.clone();
        let join = std::thread::spawn(move || producer.push(frame(2)));
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(byte(queue.poll().unwrap()), 1);
        assert!(matches!(join.join().unwrap(), PushResult::Queued));
        assert_eq!(byte(queue.poll().unwrap()), 2);
        assert_eq!(queue.take_metrics().dropped_frames, 0);
    }

    #[test]
    fn bkp_2_drop_oldest_keeps_newest_frame() {
        let queue = FrameQueue::new(1, BackpressurePolicy::DropOldest);
        assert!(matches!(queue.push(frame(1)), PushResult::Queued));
        assert!(matches!(queue.push(frame(2)), PushResult::Queued));
        assert_eq!(byte(queue.poll().unwrap()), 2);
        assert_eq!(queue.take_metrics().dropped_frames, 1);
    }

    #[test]
    fn bkp_3_drop_newest_reports_loss() {
        let queue = FrameQueue::new(1, BackpressurePolicy::DropNewest);
        assert!(matches!(queue.push(frame(1)), PushResult::Queued));
        assert!(matches!(queue.push(frame(2)), PushResult::Dropped));
        assert_eq!(byte(queue.poll().unwrap()), 1);
        assert_eq!(queue.take_metrics().dropped_frames, 1);
    }

    #[test]
    fn bkp_4_unbounded_grows_and_tracks_high_water() {
        let queue = FrameQueue::new(1, BackpressurePolicy::Unbounded);
        for value in 0..5 {
            assert!(matches!(queue.push(frame(value)), PushResult::Queued));
        }
        let metrics = queue.take_metrics();
        assert_eq!(metrics.dropped_frames, 0);
        assert_eq!(metrics.queue_high_water, 5);
    }

    #[test]
    fn bkp_5_block_timeout_forces_disconnect() {
        let queue = FrameQueue::new(1, BackpressurePolicy::Block);
        assert!(matches!(queue.push(frame(1)), PushResult::Queued));
        assert!(matches!(queue.push(frame(2)), PushResult::TimedOut));
    }
}
