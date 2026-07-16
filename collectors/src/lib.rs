//! mp-collectors — venue WS collectors → normalized events (spec 002).
//!
//! This crate holds a **transport-agnostic** core (normalization, book sync,
//! reconnect/backoff, rate budgets, staleness) plus normalizers for every
//! venue in the [`mp_core::Venue`] enum, all driven by the
//! [`transport::Transport`] trait. The live WebSocket transport
//! (tokio-tungstenite) implements that trait and connects to **public
//! market-data** endpoints only — no auth, no trading (PD-1). It is gated
//! behind the `live-ws` cargo feature so the pure logic builds without a
//! network stack.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backoff;
pub mod binance;
pub mod book_sync;
pub mod bybit;
pub mod coinbase;
pub mod collector;
pub mod hyperliquid;
pub mod json;
pub mod kraken;
pub mod normalize;
pub mod okx;
pub mod rate;
pub mod rng;
pub mod staleness;
pub mod transport;

#[cfg(feature = "live-ws")]
pub mod ws;

pub use backoff::Backoff;
pub use binance::BinanceNormalizer;
pub use bybit::BybitNormalizer;
pub use coinbase::CoinbaseNormalizer;
pub use collector::{Collector, CollectorConfig, DriveOutcome};
pub use hyperliquid::HyperliquidNormalizer;
pub use kraken::KrakenNormalizer;
pub use normalize::{HealthCounters, NormError, Normalizer};
pub use okx::OkxNormalizer;
pub use rate::RateBudget;
pub use staleness::Staleness;
pub use transport::{MockTransport, Transport, TransportEvent};

/// Construct the right normalizer for a venue.
pub fn normalizer_for(venue: mp_core::Venue) -> Box<dyn Normalizer> {
    use mp_core::Venue::*;
    match venue {
        Bybit => Box::new(BybitNormalizer::new()),
        BinanceFutures => Box::new(BinanceNormalizer::new()),
        Okx => Box::new(OkxNormalizer::new()),
        Hyperliquid => Box::new(HyperliquidNormalizer::new()),
        Coinbase => Box::new(CoinbaseNormalizer::new()),
        KrakenFutures => Box::new(KrakenNormalizer::new()),
    }
}
