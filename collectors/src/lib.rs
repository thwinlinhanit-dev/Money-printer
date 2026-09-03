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
pub mod backpressure;
pub mod binance;
pub mod binutil;
pub mod book_sync;
pub mod bybit;
pub mod coinalyze;
pub mod coinbase;
pub mod collector;
pub mod defillama;
pub mod deribit;
pub mod etherscan;
pub mod fred;
pub mod hyperliquid;
pub mod hyperliquid_positions;
pub mod ibit;
// A-4 (audit 2026-09-02): UNWIRED — spec 032 scaffold, referenced by no
// binary. Do not treat its presence as implemented multi-symbol collection.
pub mod json;
pub mod kraken;
pub mod multisym;
pub mod netflow;
pub mod normalize;
pub mod okx;
pub mod rate;
pub mod rng;
pub mod staleness;
pub mod transport;

#[cfg(feature = "live-ws")]
pub mod ws;

pub use backoff::Backoff;
pub use backpressure::BackpressurePolicy;
pub use binance::BinanceNormalizer;
pub use bybit::BybitNormalizer;
pub use coinalyze::CoinalyzeNormalizer;
pub use coinbase::CoinbaseNormalizer;
pub use collector::{Collector, CollectorConfig, DriveOutcome};
pub use defillama::DefiLlamaNormalizer;
pub use deribit::DeribitNormalizer;
pub use etherscan::EtherscanNormalizer;
pub use fred::FredNormalizer;
pub use hyperliquid::HyperliquidNormalizer;
pub use hyperliquid_positions::HyperliquidPositionsNormalizer;
pub use kraken::KrakenNormalizer;
pub use normalize::{HealthCounters, NormError, Normalizer};
pub use okx::OkxNormalizer;
pub use rate::RateBudget;
pub use staleness::Staleness;
pub use transport::{MockTransport, TeeTransport, Transport, TransportEvent};

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
        Deribit => Box::new(DeribitNormalizer::new()),
        // FRED is a REST poller (mp-macro), not a WS normalizer; the entry
        // exists so `normalizer_for` stays total over `Venue`.
        Fred => Box::new(FredNormalizer::new()),
        // Ethereum mainnet is a REST poller (mp-netflow), not a WS
        // normalizer; the entry exists so `normalizer_for` stays total.
        Ethereum => Box::new(EtherscanNormalizer::new()),
        // CBOE IBIT options chain is a REST poller (mp-ibit, spec 040),
        // not a WS normalizer; the entry exists so `normalizer_for` stays
        // total over `Venue`.
        Cboe => Box::new(crate::ibit::CboeChainNormalizer::new()),
        // DeFiLlama regime series are a REST poller (mp-defillama, spec
        // 046), not a WS normalizer; entry keeps `normalizer_for` total.
        DeFiLlama => Box::new(crate::defillama::DefiLlamaNormalizer::new()),
        // Coinalyze validation series are a REST poller (mp-coinalyze,
        // spec 047); entry keeps `normalizer_for` total over `Venue`.
        Coinalyze => Box::new(crate::coinalyze::CoinalyzeNormalizer::new()),
    }
}

/// Enforce the raw log's recv-clock invariant at the write boundary (spec 024,
/// decision 2026-08-04).
///
/// REST-injected events (open-interest polls, depth reseeds) can carry a
/// `recv_ts_ns` that is out of order relative to the WebSocket frames that
/// arrived while their HTTP call was in flight — e.g. an OI poll is stamped
/// *after* its ~1.5 s fetch but appended *before* the WS frames read during
/// it. `mp-audit` flags every regression as `recv_time_reversal`, which made
/// every recording DIRTY and the promotion gate unreachable.
///
/// This sorts the batch into `(recv_ts_ns, stream_seq)` order (the EVT-5/STO-4
/// convention) and clamps stragglers up to the running clock so appended
/// frames never regress. WS frames are monotonic because a single FIFO reader
/// stamps at socket read and `drive` drains until `DriveOutcome::Exhausted`
/// each iteration, so in practice the clamp only touches REST-injected
/// stragglers; if a burst ever leaves a WS frame past a batch boundary, its
/// recv would be smeared up to the running clock (accepted — bounded by one
/// iteration, never reorders, never loses data). Returns the new running
/// `recv_ts_ns` for the next batch.
pub fn monotonicize(events: &mut [mp_core::EventEnvelope], last_recv_ns: i64) -> i64 {
    events.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
    let mut running = last_recv_ns;
    for ev in events.iter_mut() {
        if ev.recv_ts_ns < running {
            ev.recv_ts_ns = running;
        } else {
            running = ev.recv_ts_ns;
        }
    }
    running
}

#[cfg(test)]
mod monotonicize_tests {
    use super::monotonicize;
    use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};

    fn ev(recv_ts_ns: i64, stream_seq: u64) -> EventEnvelope {
        EventEnvelope::new(
            Venue::BinanceFutures,
            SymbolId(0),
            0,
            recv_ts_ns,
            stream_seq,
            MarketEvent::Trade {
                price: 1.0,
                qty: 1.0,
                side: Side::Buy,
                trade_id: recv_ts_ns as u64,
            },
        )
    }

    #[test]
    fn sorts_rest_injected_events_into_recv_order() {
        // OI event stamped after its fetch (recv 102) arrives before two WS
        // frames read during the fetch (recv 100, 99) — the real 08-04 bug.
        let mut batch = vec![ev(102, 0), ev(100, 5), ev(99, 6)];
        let last = monotonicize(&mut batch, 98);
        let recvs: Vec<i64> = batch.iter().map(|e| e.recv_ts_ns).collect();
        assert_eq!(recvs, vec![99, 100, 102]);
        assert!(recvs.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(last, 102);
    }

    #[test]
    fn clamps_stragglers_below_running_clock() {
        // A straggler older than the previous batch's clock must be clamped
        // up, never allowed to regress the log.
        let mut batch = vec![ev(50, 9)];
        let last = monotonicize(&mut batch, 100);
        assert_eq!(batch[0].recv_ts_ns, 100);
        assert_eq!(last, 100);
    }

    #[test]
    fn leaves_already_monotonic_batch_untouched() {
        let mut batch = vec![ev(1, 1), ev(2, 2), ev(3, 3)];
        let last = monotonicize(&mut batch, 0);
        let recvs: Vec<i64> = batch.iter().map(|e| e.recv_ts_ns).collect();
        assert_eq!(recvs, vec![1, 2, 3]);
        assert_eq!(last, 3);
    }
}
