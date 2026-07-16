//! Normalized event schema (spec 001, EVT-1). Field names are law — no
//! synonyms (CLAUDE.md naming rule). Every component speaks exactly this.

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// Interned symbol handle (EVT-8). Stable within a run; string form lives only
/// in the [`SymbolTable`](crate::SymbolTable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolId(pub u32);

/// Trading venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Venue {
    BinanceFutures,
    Bybit,
    Okx,
    Hyperliquid,
    Coinbase,
    KrakenFutures,
}

/// Aggressor side for trades/liquidations; resting side for book context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// A single price level: `(price, qty)`. `qty == 0.0` in a delta means remove.
pub type Level = (f64, f64);

/// Book levels for one side. Up to 8 inline before spilling to the heap
/// (EVT-2: book deltas may allocate; trades never do).
pub type Levels = SmallVec<[Level; 8]>;

/// Short free-text detail for [`MarketEvent::Status`]. Aliased to `String` in
/// v1 (Status is not on the per-trade hot path; see spec 001 Decisions).
pub type SmallString = String;

/// Why a [`MarketEvent::BookSnapshot`] was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotReason {
    /// First snapshot for the stream.
    Init,
    /// Snapshot taken to recover from a detected sequence gap.
    GapResync,
    /// Routine periodic snapshot.
    Periodic,
}

/// Stream/venue status — flows through the same pipe as market data because
/// gaps and disconnects are themselves signal (spec 001 Design).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusKind {
    Connected,
    Disconnected,
    GapDetected,
    Throttled,
    VenueHalt,
    Stale,
}

/// The normalized market event body (EVT-1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MarketEvent {
    Trade {
        price: f64,
        qty: f64,
        /// Aggressor side.
        side: Side,
        trade_id: u64,
    },
    BookDelta {
        bids: Levels,
        asks: Levels,
        first_seq: u64,
        last_seq: u64,
    },
    BookSnapshot {
        bids: Levels,
        asks: Levels,
        seq: u64,
        depth: u16,
        reason: SnapshotReason,
    },
    Funding {
        rate: f64,
        interval_s: u32,
        next_funding_ts_ns: i64,
    },
    MarkPrice {
        mark: f64,
        /// `f64::NAN` if the venue omits an index price.
        index: f64,
    },
    OpenInterest {
        oi_contracts: f64,
        /// `f64::NAN` if the venue omits notional.
        oi_notional: f64,
    },
    Liquidation {
        price: f64,
        qty: f64,
        /// Side being liquidated.
        side: Side,
    },
    IndexPrice {
        index: f64,
    },
    Status {
        kind: StatusKind,
        detail: SmallString,
    },
}

/// Envelope wrapping every event with routing + timing metadata (EVT-1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Schema version (CONV-20). See [`crate::SCHEMA_VER`].
    pub schema_ver: u16,
    pub venue: Venue,
    pub symbol: SymbolId,
    /// Exchange-reported time; `0` if the venue omits it (CONV-4).
    pub exch_ts_ns: i64,
    /// Local receive time, stamped at socket read before parse (COL-5).
    pub recv_ts_ns: i64,
    /// Venue sequence if provided, else collector-assigned monotonic.
    pub stream_seq: u64,
    pub body: MarketEvent,
}

impl EventEnvelope {
    /// Construct an envelope stamped with the current [`SCHEMA_VER`](crate::SCHEMA_VER).
    pub fn new(
        venue: Venue,
        symbol: SymbolId,
        exch_ts_ns: i64,
        recv_ts_ns: i64,
        stream_seq: u64,
        body: MarketEvent,
    ) -> Self {
        Self {
            schema_ver: crate::SCHEMA_VER,
            venue,
            symbol,
            exch_ts_ns,
            recv_ts_ns,
            stream_seq,
            body,
        }
    }

    /// Merge key for global ordering across venues/files (EVT-5).
    #[inline]
    pub fn merge_key(&self) -> (i64, u64) {
        (self.recv_ts_ns, self.stream_seq)
    }
}
