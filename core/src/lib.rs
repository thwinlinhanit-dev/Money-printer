//! mp-core — the normalized event vocabulary every component speaks
//! (spec 001). Event types, injected clock, symbol interning, the crash-safe
//! event log, the SPMC ring buffer, and book reconstruction.
//!
//! See `specs/001-event-schema.md`. Field names are law (no synonyms).
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod book;
pub mod codec;
pub mod event;
pub mod exec;
pub mod hash;
pub mod log;
pub mod ring;
pub mod rng;
pub mod symbol;
pub mod time;
pub mod wall_clock;

/// Serialized-schema version stamped on every envelope (CONV-20).
pub const SCHEMA_VER: u16 = 1;

pub use book::BookMirror;
pub use event::{
    EventEnvelope, Level, Levels, MarketEvent, Side, SmallString, SnapshotReason, StatusKind,
    SymbolId, Venue,
};
pub use exec::{
    Fill, IntentId, Liquidity, OrderIntent, OrderKind, SizeUnit, StrategyId, TimeInForce,
};
pub use hash::{fnv1a_64, fnv1a_64_str, fnv1a_absorb, FNV1A_OFFSET, FNV1A_PRIME};
pub use log::{merge_sorted_events, MergeReader};
pub use ring::{Consumer, Overrun, Producer, Ring};
pub use rng::SplitMix64;
pub use symbol::{InstrumentKind, SymbolMeta, SymbolTable};
pub use time::{Clock, Nanos, SimClock};
pub use wall_clock::WallClock;
