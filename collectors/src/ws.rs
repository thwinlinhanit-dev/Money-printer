//! Live public-market-data WebSocket transport (COL-1). Behind the `live-ws`
//! feature. **PD-1: this connects to PUBLIC market-data endpoints only** — it
//! sends subscribe frames and reads data. It has no auth, no signing, and
//! cannot place orders. Order flow lives in `mp-oms` behind its own boundary.
//!
//! Bridges an async tokio-tungstenite socket to the synchronous
//! [`Transport`](crate::transport::Transport) trait via a bounded channel, so
//! the whole `Collector` driver stays transport-agnostic and unit-testable.

use crate::backpressure::BackpressurePolicy;
use crate::transport::{Transport, TransportEvent, TransportMetrics};
use futures_util::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

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
    /// Optional egress proxy for the WebSocket connection (spec 024
    /// 2026-08-04 incident: some networks are geo-filtered at the venue edge
    /// — Binance futures silently drops non-book streams — so a proxy/VPN in
    /// an allowed region restores full data).
    /// Supported: `http://host:port` (HTTP CONNECT) and `socks5://host:port`.
    pub proxy: Option<String>,
}

impl WsEndpoint {
    /// Convenience constructor with the default frame limits and no proxy.
    pub fn new(url: impl Into<String>, subscribe: Vec<String>) -> Self {
        Self {
            url: url.into(),
            subscribe,
            limits: WsLimits::default(),
            proxy: None,
        }
    }

    /// Route this connection through an HTTP CONNECT or SOCKS5 proxy.
    pub fn with_proxy(mut self, proxy: Option<String>) -> Self {
        self.proxy = proxy;
        self
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
            max_frame_size: 1024 * 1024,       // 1 MiB
            max_message_size: 4 * 1024 * 1024, // 4 MiB
        }
    }
}

/// A live transport fed by a background tokio task. `poll` is non-blocking.
pub struct WsTransport {
    queue: FrameQueue,
    _rt: tokio::runtime::Runtime,
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
}

impl Transport for WsTransport {
    fn poll(&mut self) -> Option<TransportEvent> {
        self.queue.poll()
    }

    /// Consume the aggregated loss/high-water telemetry since the prior call.
    /// The collector writes a durable status event and invalidates its book
    /// for every loss batch (BKP-3 and INT-3).
    fn take_metrics(&mut self) -> TransportMetrics {
        self.queue.take_metrics()
    }
}

fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(_) => 0,
    }
}

/// Type-erased stream so the proxied path (TCP -> CONNECT/SOCKS5 -> TLS) and
/// the direct path produce the same downstream type.
/// `dyn A + B` is not allowed for non-auto traits, so we define a supertrait.
trait BoxedIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> BoxedIo for T {}
type BoxedStream = Box<dyn BoxedIo>;

async fn run(
    endpoint: WsEndpoint,
    queue: FrameQueue,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // tungstenite 0.24 exposes the limits as struct fields, not setters.
    let cfg = WebSocketConfig {
        max_frame_size: Some(endpoint.limits.max_frame_size),
        max_message_size: Some(endpoint.limits.max_message_size),
        ..Default::default()
    };
    let (ws, _resp) = connect_through_proxy(&endpoint.url, endpoint.proxy.as_deref(), &cfg).await?;
    let (mut write, mut read) = ws.split();

    for sub in &endpoint.subscribe {
        write.send(Message::Text(sub.clone())).await?;
    }

    // Read timeout (COL-2 defense): a half-open TCP socket can hold
    // `read.next()` forever (no FIN, no data, no keepalive). depth@100ms +
    // markPrice@1s mean any real subscription delivers within seconds, so a
    // 20s silence is a dead connection — end the task so the collector sees
    // `Disconnected` and reconnects instead of freezing silently.
    const READ_TIMEOUT: Duration = Duration::from_secs(20);
    loop {
        let next = tokio::time::timeout(READ_TIMEOUT, read.next()).await;
        let msg = match next {
            Err(_) => {
                tracing::warn!(
                    "ws read timed out after {}s of silence; reconnecting",
                    READ_TIMEOUT.as_secs()
                );
                break;
            }
            Ok(None) => break, // stream closed by the peer
            Ok(Some(msg)) => msg,
        };
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

/// Connect to `url`, optionally through an HTTP CONNECT (`http://`) or SOCKS5
/// (`socks5://`) proxy.  Used when the venue edge geo-filters this network
/// (spec 024 2026-08-04 incident: Binance futures delivers only depth streams
/// from some regions).  The TLS handshake (for `wss://`) is done against the
/// *target* host with webpki roots — the proxy only carries bytes, it never
/// terminates TLS.  With no proxy this is a plain direct connection (same
/// handshake, one code path, one stream type).
///
/// Returns `(WebSocketStream, Response)` — same shape as
/// `tokio_tungstenite::connect_async_with_config` — so callers are unchanged.
async fn connect_through_proxy(
    url: &str,
    proxy: Option<&str>,
    cfg: &WebSocketConfig,
) -> Result<(WebSocketStream<BoxedStream>, Response), Box<dyn std::error::Error + Send + Sync>> {
    let target = url::Url::parse(url)?;
    let target_host = target
        .host_str()
        .ok_or("target URL has no host")?
        .to_string();
    let target_port = target
        .port_or_known_default()
        .ok_or("target URL has no port")?;

    // Open TCP, optionally tunneled through the proxy (CONNECT/SOCKS5).
    let tcp: BoxedStream = if let Some(proxy) = proxy {
        let proxy_url = url::Url::parse(proxy)?;
        // Proxy authentication is not supported: reject URLs that embed
        // credentials so they can never be silently dropped (and the raw URL,
        // which would contain them, is never logged here).
        if !proxy_url.username().is_empty() || proxy_url.password().is_some() {
            return Err("proxy URLs with embedded credentials are not supported".into());
        }
        let proxy_host = proxy_url
            .host_str()
            .ok_or("proxy URL has no host")?
            .to_string();
        let proxy_port = proxy_url.port().unwrap_or(match proxy_url.scheme() {
            "socks5" | "socks5h" => 1080,
            _ => 3128,
        });
        let tcp = TcpStream::connect((proxy_host.as_str(), proxy_port)).await?;
        match proxy_url.scheme() {
            "http" | "https" => {
                Box::new(http_connect_tunnel(tcp, &target_host, target_port).await?)
            }
            "socks5" | "socks5h" => Box::new(socks5_tunnel(tcp, &target_host, target_port).await?),
            other => return Err(format!("unsupported proxy scheme: {other}").into()),
        }
    } else {
        Box::new(TcpStream::connect((target_host.as_str(), target_port)).await?)
    };

    // wss:// = TLS over the (tunneled) TCP stream, then the WS handshake.
    // TLS is always terminated against the *target* venue with webpki roots —
    // the proxy only carries ciphertext.  Uses the ring provider explicitly
    // (builder_with_provider) so this path does not depend on any global
    // `install_default()` having been called by the caller.
    let stream: BoxedStream = if target.scheme() == "wss" {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls protocol config: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = rustls::pki_types::ServerName::try_from(target_host.clone())
            .map_err(|e| format!("bad server name {target_host}: {e}"))?;
        Box::new(connector.connect(server_name, tcp).await?)
    } else {
        tcp
    };

    let request = url.to_string().into_client_request()?;
    let (ws, resp) =
        tokio_tungstenite::client_async_with_config(request, stream, Some(*cfg)).await?;
    Ok((ws, resp))
}

/// HTTP CONNECT tunnel: `CONNECT host:port HTTP/1.1`, require `200`.
async fn http_connect_tunnel(
    mut tcp: TcpStream,
    host: &str,
    port: u16,
) -> Result<TcpStream, Box<dyn std::error::Error + Send + Sync>> {
    let request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
    tcp.write_all(request.as_bytes()).await?;
    // Read until end of headers (\r\n\r\n), bounded.
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = tcp.read(&mut chunk).await?;
        if n == 0 {
            return Err("proxy closed during CONNECT".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err("proxy CONNECT response headers too large".into());
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let status_line = head.lines().next().unwrap_or("");
    // Parse the numeric status from "HTTP/1.1 200 Connection established".
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok());
    match code {
        Some(200) => Ok(tcp),
        other => Err(format!("proxy CONNECT rejected: {status_line} (status {other:?})").into()),
    }
}

/// SOCKS5 tunnel with no-auth: greeting, then CONNECT with a domain target.
async fn socks5_tunnel(
    mut tcp: TcpStream,
    host: &str,
    port: u16,
) -> Result<TcpStream, Box<dyn std::error::Error + Send + Sync>> {
    if host.len() > 255 {
        return Err("SOCKS5 domain too long".into());
    }
    // Greeting: version 5, one method, no-auth (0x00).
    tcp.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut reply = [0u8; 2];
    tcp.read_exact(&mut reply).await?;
    if reply != [0x05, 0x00] {
        return Err(format!("SOCKS5 no-auth not accepted: {reply:?}").into());
    }
    // CONNECT request, ATYP=0x03 (domain).
    let mut request = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&port.to_be_bytes());
    tcp.write_all(&request).await?;
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(format!("SOCKS5 connect failed: rep={}", head[1]).into());
    }
    // Consume the server's bound-address payload (ATYP-dependent).
    let addr_len = match head[3] {
        0x01 => 4usize, // IPv4
        0x03 => {
            let mut len = [0u8; 1];
            tcp.read_exact(&mut len).await?;
            len[0] as usize
        }
        0x04 => 16usize, // IPv6
        _ => return Err(format!("SOCKS5 unexpected ATYP: {}", head[3]).into()),
    };
    let mut rest = vec![0u8; addr_len + 2]; // address + port
    tcp.read_exact(&mut rest).await?;
    Ok(tcp)
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
    /// Deribit public market-data WS (spec 031 — no auth for market data).
    pub const DERIBIT: &str = "wss://www.deribit.com/ws/api/v2";
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

    /// HTTP CONNECT tunnel: happy path (proxy answers 200) and rejection
    /// (proxy answers 403). Uses a local mock proxy listener — no network.
    #[tokio::test]
    async fn proxy_http_connect_accepts_200_and_rejects_403() {
        async fn mock_proxy(respond: &'static str) -> u16 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 512];
                let _ = sock.read(&mut buf).await.unwrap();
                let _ = sock.write_all(respond.as_bytes()).await;
            });
            port
        }

        // 200 -> tunnel succeeds.
        let port = mock_proxy("HTTP/1.1 200 Connection established\r\n\r\n").await;
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let ok = http_connect_tunnel(tcp, "example.com", 443).await;
        assert!(ok.is_ok(), "200 CONNECT must succeed: {ok:?}");

        // 403 -> tunnel refused.
        let port = mock_proxy("HTTP/1.1 403 Forbidden\r\n\r\n").await;
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let err = http_connect_tunnel(tcp, "example.com", 443).await;
        assert!(err.is_err(), "403 CONNECT must be refused");
    }

    /// SOCKS5 tunnel: greeting + CONNECT handshake against a mock server that
    /// answers no-auth and a successful connect with an IPv4 bound address.
    #[tokio::test]
    async fn proxy_socks5_handshake_completes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            // Greeting: version 5, 1 method, no-auth.
            let _ = sock.read(&mut buf).await.unwrap();
            let _ = sock.write_all(&[0x05, 0x00]).await;
            // CONNECT request (domain).
            let _ = sock.read(&mut buf).await.unwrap();
            // Reply: ver=5 rep=0 rsv=0 atyp=1 (IPv4), then 4 addr bytes + 2 port.
            let _ = sock
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await;
        });
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let result = socks5_tunnel(tcp, "example.com", 443).await;
        assert!(result.is_ok(), "SOCKS5 handshake must complete: {result:?}");
    }
}
