//! mp-core — the normalized event vocabulary every component speaks
//! (spec 001). Event types, injected clock, symbol interning, the crash-safe
//! event log, the SPMC ring buffer, and book reconstruction.
//!
//! See `specs/001-event-schema.md`. Field names are law (no synonyms).
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod book;
pub mod codec;
pub mod event;
pub mod log;
pub mod ring;
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
pub use ring::{Consumer, Overrun, Producer, Ring};
pub use symbol::{InstrumentKind, SymbolMeta, SymbolTable};
pub use time::{Clock, Nanos, SimClock};
pub use wall_clock::WallClock;
