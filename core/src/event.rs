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
    /// Options venue (spec 031). Appended so old bincode frames keep their
    /// variant indices (CONV-20: schema 2→3 was append-only).
    Deribit,
    /// FRED (St. Louis Fed) economic series (spec 030). Not a trading venue;
    /// used as the envelope venue for [`MarketEvent::MacroPoint`] events.
    Fred,
}

impl Venue {
    /// Short lower-case slug for display and feature names.
    pub fn slug(&self) -> &'static str {
        match self {
            Venue::BinanceFutures => "binance",
            Venue::Bybit => "bybit",
            Venue::Okx => "okx",
            Venue::Hyperliquid => "hyperliquid",
            Venue::Coinbase => "coinbase",
            Venue::KrakenFutures => "kraken",
            Venue::Deribit => "deribit",
            Venue::Fred => "fred",
        }
    }

    /// Parse a venue from its slug. Accepts both the short [`slug`](Self::slug)
    /// form and the underscore partition slug written by the storage layout
    /// (e.g. `binance_futures`), so partition paths round-trip.
    pub fn from_slug(s: &str) -> Option<Self> {
        Some(match s {
            "binance" | "binancefutures" | "binance_futures" => Venue::BinanceFutures,
            "bybit" => Venue::Bybit,
            "okx" => Venue::Okx,
            "hyperliquid" => Venue::Hyperliquid,
            "coinbase" => Venue::Coinbase,
            "kraken" | "krakenfutures" | "kraken_futures" => Venue::KrakenFutures,
            "deribit" => Venue::Deribit,
            "fred" => Venue::Fred,
            _ => return None,
        })
    }
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
    /// One or more frames were dropped due to backpressure (spec 013).
    BackpressureDrop {
        /// Number of frames dropped in this batch.
        dropped: u64,
    },
    /// A periodic census poll completed (spec 028): the whale position
    /// collector records one per top-N/watchlist poll so the raw log stays
    /// fresh even when every tracked address is flat (a zero-position census
    /// is a real observation, not silence). Neutral for audit/manifest.
    Census,
}

/// Where an event came from within one collector connection.  The envelope
/// already carries the venue and symbol; this records the otherwise-lost
/// transport context needed to audit a raw recording (INT-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventProvenance {
    /// Logical market-data stream, for example `depth` or `mark_price`.
    pub stream: String,
    /// Exact public subscription or combined-stream path used by the venue.
    pub subscription: String,
    /// Monotonic collector-local connection identity.  Changes after a
    /// reconnect, making reconnect boundaries replayable.
    pub connection_id: u64,
    /// Origin of a book snapshot.  `None` is valid for non-book events.
    pub snapshot_source: SnapshotSource,
}

impl EventProvenance {
    /// Empty provenance used by deterministic fixtures and in-process
    /// generated events.  Production raw recordings must replace this before
    /// append.  Empty `String`s preserve EVT-2's allocation-free trade path.
    pub fn synthetic() -> Self {
        Self {
            stream: String::new(),
            subscription: String::new(),
            connection_id: 0,
            snapshot_source: SnapshotSource::None,
        }
    }
}

/// How a book snapshot was obtained.  Keeping this separate from
/// [`SnapshotReason`] answers both *why* a snapshot exists and *where* it was
/// sourced from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotSource {
    None,
    Rest,
    WebSocket,
    Synthetic,
}

/// Option type (spec 031).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionKind {
    Call,
    Put,
}

/// Option instrument metadata parsed from a venue instrument name such as
/// Deribit's `BTC-28JUN26-100000-C` (spec 031, OPT-2). Attached to every
/// option market event so downstream Parquet rows carry the full leg.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionLeg {
    /// Underlying asset, e.g. "BTC".
    pub underlying: String,
    /// Strike price in quote units (Deribit BTC strike is in cents of USD).
    pub strike: f64,
    /// Expiry date as ns at UTC midnight of the instrument's expiry date.
    pub expiry_ts_ns: i64,
    pub kind: OptionKind,
}

/// Greeks at record time (spec 031, OPT-2: "greeks-at-record if in the
/// ticker"). Recording only — no analytics in this scope (OPT-6). Missing
/// values use the `f64::NAN` sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OptionGreeks {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
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
    // ---- spec 028/030/031 additions (schema 2→3, append-only — do NOT
    // reorder or remove anything above: old bincode frames map by index).
    /// Hyperliquid on-chain per-user perpetual position census (spec 028,
    /// WHL-2). The envelope carries venue/symbol (the coin)/timestamps; this
    /// variant carries the position fields. DATA ONLY — never a strategy
    /// input before event-study grading (WHL-5).
    WhalePosition {
        /// Opaque 0x address (WHL-3 — never external labels/PII).
        address: String,
        /// Signed position size in base units (positive long, negative short).
        size: f64,
        /// Entry price.
        entry: f64,
        /// Leverage value (e.g. 10.0 = 10x).
        leverage: f64,
        /// Liquidation price; `f64::NAN` if the venue omits it.
        liq_price: f64,
    },
    /// FRED daily economic observation (spec 030, MAC-3). Envelope venue is
    /// [`Venue::Fred`]. Correlation-grade, not execution-grade (MAC-5).
    MacroPoint {
        /// FRED series id, e.g. "DGS10".
        series_id: String,
        /// Observation value.
        value: f64,
        /// Observation date as ns at UTC midnight.
        date: i64,
    },
    /// Deribit option trade (spec 031, OPT-2).
    OptionTrade {
        leg: OptionLeg,
        price: f64,
        qty: f64,
        /// Aggressor side.
        side: Side,
        trade_id: u64,
    },
    /// Deribit option book message (spec 031, OPT-2). `is_snapshot`
    /// distinguishes full-book snapshots from changes; `change_id` is the
    /// Deribit book change id used for continuity/gap detection (COL-7).
    OptionBook {
        leg: OptionLeg,
        bids: Levels,
        asks: Levels,
        change_id: u64,
        is_snapshot: bool,
    },
    /// Deribit option ticker — mark IV + greeks at record (spec 031, OPT-2).
    /// Recording only; analytics are out of scope (OPT-6). Missing values use
    /// the `f64::NAN` sentinel.
    OptionTicker {
        leg: OptionLeg,
        mark_iv: f64,
        mark_price: f64,
        underlying_price: f64,
        open_interest: f64,
        greeks: Option<OptionGreeks>,
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
    /// Transport provenance persisted with every event (INT-1).
    pub provenance: EventProvenance,
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
            provenance: EventProvenance::synthetic(),
            body,
        }
    }

    /// Attach live collector provenance immediately before writing a raw log.
    pub fn with_provenance(mut self, provenance: EventProvenance) -> Self {
        self.provenance = provenance;
        self
    }

    /// Merge key for global ordering across venues/files (EVT-5).
    #[inline]
    pub fn merge_key(&self) -> (i64, u64) {
        (self.recv_ts_ns, self.stream_seq)
    }
}
