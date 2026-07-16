//! Symbol interning + metadata (EVT-8, CONV-6). String forms live here only;
//! everything downstream carries the compact [`SymbolId`]. Iteration order is
//! deterministic (`BTreeMap`, CONV-10) and the table is persisted in the event
//! log so replays resolve ids identically (EVT-8).

use crate::event::{SymbolId, Venue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Instrument kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstrumentKind {
    Spot,
    Perp,
    Future,
}

/// Per-symbol metadata (CONV-6). Rounding to these precisions happens only at
/// the venue boundary (oms).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolMeta {
    pub symbol_id: SymbolId,
    pub venue: Venue,
    /// Venue's own symbol string, e.g. "BTCUSDT".
    pub venue_symbol: String,
    pub base: String,
    pub quote: String,
    pub kind: InstrumentKind,
    pub tick_size: f64,
    pub step_size: f64,
    pub min_notional: f64,
    pub contract_multiplier: f64,
    pub listed_ts_ns: i64,
    /// `i64::MAX` while active.
    pub delisted_ts_ns: i64,
}

impl SymbolMeta {
    /// Minimal constructor for an active symbol with unit multiplier.
    // A market-data symbol legitimately has this many defining fields; grouping
    // them into a sub-struct would only move the argument list, not shorten it.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        symbol_id: SymbolId,
        venue: Venue,
        venue_symbol: impl Into<String>,
        base: impl Into<String>,
        quote: impl Into<String>,
        kind: InstrumentKind,
        tick_size: f64,
        step_size: f64,
        min_notional: f64,
    ) -> Self {
        Self {
            symbol_id,
            venue,
            venue_symbol: venue_symbol.into(),
            base: base.into(),
            quote: quote.into(),
            kind,
            tick_size,
            step_size,
            min_notional,
            contract_multiplier: 1.0,
            listed_ts_ns: 0,
            delisted_ts_ns: i64::MAX,
        }
    }
}

/// Interns `(venue, venue_symbol)` pairs to stable [`SymbolId`]s within a run.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    by_key: BTreeMap<(Venue, String), SymbolId>,
    metas: Vec<SymbolMeta>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern a symbol, inserting metadata on first sight. Returns the stable
    /// id; repeated calls with the same key return the same id (EVT-8).
    pub fn intern(
        &mut self,
        venue: Venue,
        venue_symbol: &str,
        make_meta: impl FnOnce(SymbolId) -> SymbolMeta,
    ) -> SymbolId {
        let key = (venue, venue_symbol.to_owned());
        if let Some(id) = self.by_key.get(&key) {
            return *id;
        }
        let id = SymbolId(self.metas.len() as u32);
        self.metas.push(make_meta(id));
        self.by_key.insert(key, id);
        id
    }

    /// Intern with default metadata (unknown precisions) — collectors refine
    /// later. Handy for tests and bootstrap.
    pub fn intern_default(&mut self, venue: Venue, venue_symbol: &str) -> SymbolId {
        self.intern(venue, venue_symbol, |id| {
            SymbolMeta::new(
                id,
                venue,
                venue_symbol,
                "",
                "",
                InstrumentKind::Perp,
                f64::NAN,
                f64::NAN,
                f64::NAN,
            )
        })
    }

    pub fn get(&self, id: SymbolId) -> Option<&SymbolMeta> {
        self.metas.get(id.0 as usize)
    }

    pub fn lookup(&self, venue: Venue, venue_symbol: &str) -> Option<SymbolId> {
        self.by_key.get(&(venue, venue_symbol.to_owned())).copied()
    }

    pub fn len(&self) -> usize {
        self.metas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.metas.is_empty()
    }

    /// Snapshot of all metadata in id order — what the log persists (EVT-8).
    pub fn metas(&self) -> &[SymbolMeta] {
        &self.metas
    }

    /// Rebuild a table from a persisted snapshot, preserving ids.
    pub fn from_metas(metas: Vec<SymbolMeta>) -> Self {
        let mut by_key = BTreeMap::new();
        for m in &metas {
            by_key.insert((m.venue, m.venue_symbol.clone()), m.symbol_id);
        }
        Self { by_key, metas }
    }
}
