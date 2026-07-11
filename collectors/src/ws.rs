//! Live public-market-data WebSocket transport (COL-1). Behind the `live-ws`
//! feature. **PD-1: this connects to PUBLIC market-data endpoints only** — it
//! sends subscribe frames and reads data. It has no auth, no signing, and
//! cannot place orders. Order flow lives in `mp-oms` behind its own boundary.
//!
//! Bridges an async tokio-tungstenite socket to the synchronous
//! [`Transport`](crate::transport::Transport) trait via a bounded channel, so
//! the whole `Collector` driver stays transport-agnostic and unit-testable.

use crate::transport::{Transport, TransportEvent};
use futures_util::{SinkExt, StreamExt};
use std::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Endpoint + subscription messages for one venue connection. Endpoints are
/// public; put no credentials here.
#[derive(Debug, Clone)]
pub struct WsEndpoint {
    /// Public `wss://` URL (e.g. Bybit `wss://stream.bybit.com/v5/public/linear`).
    pub url: String,
    /// JSON subscribe frames to send on connect (venue-specific).
    pub subscribe: Vec<String>,
}

/// A live transport fed by a background tokio task. `poll` is non-blocking.
pub struct WsTransport {
    rx: mpsc::Receiver<TransportEvent>,
    _rt: tokio::runtime::Runtime,
}

impl WsTransport {
    /// Connect and start streaming. The `recv_ts_ns` clock is the OS clock
    /// stamped at socket read (COL-5) — the one place a collector reads wall
    /// time, which is allowed (it is not a decision path).
    pub fn connect(endpoint: WsEndpoint, buffer: usize) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;
        let (tx, rx) = mpsc::sync_channel(buffer.max(64));

        rt.spawn(async move {
            if let Err(e) = run(endpoint, tx.clone()).await {
                tracing::warn!(error = %e, "ws task ended");
            }
            // Signal disconnect so the driver reconnects (COL-1).
            let _ = tx.try_send(TransportEvent::Disconnected);
        });

        Ok(Self { rx, _rt: rt })
    }
}

impl Transport for WsTransport {
    fn poll(&mut self) -> Option<TransportEvent> {
        self.rx.try_recv().ok()
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
    tx: mpsc::SyncSender<TransportEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (ws, _resp) = tokio_tungstenite::connect_async(&endpoint.url).await?;
    let (mut write, mut read) = ws.split();

    for sub in &endpoint.subscribe {
        write.send(Message::Text(sub.clone())).await?;
    }

    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(t) => {
                if tx
                    .try_send(TransportEvent::Frame {
                        recv_ts_ns: now_ns(),
                        payload: t.into_bytes(),
                    })
                    .is_err()
                {
                    // Consumer overran; treat as a drop and keep reading.
                    tracing::warn!("ws frame channel full; dropping");
                }
            }
            Message::Binary(b) => {
                let _ = tx.try_send(TransportEvent::Frame {
                    recv_ts_ns: now_ns(),
                    payload: b,
                });
            }
            Message::Ping(p) => {
                write.send(Message::Pong(p)).await?;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}

/// Public endpoint presets (URLs only — no credentials). Verify against current
/// venue docs at implementation time; they drift (spec 002 pitfall #2).
pub mod endpoints {
    /// Bybit v5 linear public stream.
    pub const BYBIT_LINEAR: &str = "wss://stream.bybit.com/v5/public/linear";
    /// Binance USDⓈ-M Futures combined stream base.
    pub const BINANCE_FUTURES: &str = "wss://fstream.binance.com/stream";
    /// OKX v5 public.
    pub const OKX_PUBLIC: &str = "wss://ws.okx.com:8443/ws/v5/public";
    /// Coinbase Advanced Trade market data.
    pub const COINBASE_ADVANCED: &str = "wss://advanced-trade-ws.coinbase.com";
    /// Kraken Futures.
    pub const KRAKEN_FUTURES: &str = "wss://futures.kraken.com/ws/v1";
    /// Hyperliquid.
    pub const HYPERLIQUID: &str = "wss://api.hyperliquid.xyz/ws";
}
