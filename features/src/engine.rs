//! Feature engine (FEA-1..4). Feeds events in stream order to per-symbol
//! feature instances; as-of ordering is structural (a feature only ever sees
//! events up to now — there is no API to query external "current" state,
//! PD-3/FEA-2). Warmup outputs are suppressed (FEA-3). Deterministic: features
//! run in a fixed registration order, symbols in `BTreeMap` order (CONV-10),
//! so an identical event sequence yields an identical update sequence (FEA-4).

use crate::bar::{Bar, BarBuilder};
use mp_core::event::EventEnvelope;
use mp_core::{SymbolId, Venue};
use std::collections::BTreeMap;

/// A single feature output at a point in time (FEA-16: feature is interned SymbolId).
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureUpdate {
    /// Interned feature name (SymbolId). String form resolved via engine's name table.
    pub feature: SymbolId,
    /// Venue the source event came from — strategies need this for
    /// `OrderIntent.venue` (multi-venue feeds must not invent a default).
    pub venue: Venue,
    pub symbol: SymbolId,
    pub ts_ns: i64,
    pub value: f64,
    pub ver: u16,
}

/// Where a feature may run (FEA-9). `Offline` features (e.g. `leadlag.*`) are
/// too expensive for the live path; the engine refuses to run them live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    Online,
    Offline,
    Both,
}

/// A feature computed on every event (order flow, book, derivatives passthrough).
pub trait TickFeature {
    fn id(&self) -> String;
    fn ver(&self) -> u16 {
        1
    }
    /// Where this feature may run (FEA-9). Defaults to `Both`.
    fn locality(&self) -> Locality {
        Locality::Both
    }
    /// Whether warmup is satisfied (FEA-3). Emissions while `false` are dropped.
    fn warm(&self) -> bool {
        true
    }
    /// Update on one event; return `Some(value)` to emit.
    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64>;
}

/// A feature computed on bar close (no intra-bar repaint).
pub trait BarFeature {
    fn id(&self) -> String;
    fn ver(&self) -> u16 {
        1
    }
    fn warm(&self) -> bool {
        true
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64>;
}

type TickFactory = Box<dyn Fn() -> Box<dyn TickFeature>>;
type BarFactory = Box<dyn Fn() -> Box<dyn BarFeature>>;

struct SymbolState {
    ticks: Vec<Box<dyn TickFeature>>,
    bars: Vec<Box<dyn BarFeature>>,
    builder: BarBuilder,
    /// Pre-computed SymbolIds for each tick feature (index-matched to `ticks`).
    tick_ids: Vec<SymbolId>,
    /// Pre-computed feature names (for diagnostics without engine borrow).
    tick_names: Vec<String>,
    /// Pre-computed SymbolIds for each bar feature.
    bar_ids: Vec<SymbolId>,
    /// Pre-computed bar feature names.
    bar_names: Vec<String>,
}

/// The streaming engine. Register feature factories, then feed events.
/// Maintains an intern table for feature names → SymbolId (spec 023).
pub struct FeatureEngine {
    tick_factories: Vec<TickFactory>,
    bar_factories: Vec<BarFactory>,
    bar_tf_ns: i64,
    per_symbol: BTreeMap<SymbolId, SymbolState>,
    /// FEA-5: non-finite outputs suppressed (never emitted downstream).
    nan_suppressed: u64,
    /// Feature name intern table (FEA-17): name → SymbolId.
    name_to_id: BTreeMap<String, SymbolId>,
    /// Reverse lookup: SymbolId → name (for resolution in logs/screener).
    id_to_name: BTreeMap<SymbolId, String>,
    next_feature_id: u32,
}

/// SymbolId(0) is reserved as null/invalid (FEA-18).

impl FeatureEngine {
    pub fn new(bar_tf_ns: i64) -> Self {
        Self {
            tick_factories: Vec::new(),
            bar_factories: Vec::new(),
            bar_tf_ns,
            per_symbol: BTreeMap::new(),
            nan_suppressed: 0,
            name_to_id: BTreeMap::new(),
            id_to_name: BTreeMap::new(),
            next_feature_id: 1, // FEA-18: start from 1
        }
    }

    /// Intern a feature name, returning a stable SymbolId (FEA-17).
    fn intern(&mut self, name: &str) -> SymbolId {
        if let Some(&id) = self.name_to_id.get(name) {
            return id;
        }
        let id = SymbolId(self.next_feature_id);
        self.next_feature_id += 1;
        self.name_to_id.insert(name.to_string(), id);
        self.id_to_name.insert(id, name.to_string());
        id
    }

    /// Return the full SymbolId → name map (for screener wiring, spec 017).
    pub fn name_map(&self) -> &BTreeMap<SymbolId, String> {
        &self.id_to_name
    }

    /// Resolve a SymbolId back to its feature name. Returns "unknown" if not found.
    pub fn resolve_name(&self, id: SymbolId) -> &str {
        self.id_to_name.get(&id).map_or("unknown", |s| s.as_str())
    }

    /// Look up a feature name's SymbolId. Returns None if not yet interned.
    pub fn name_to_id(&self, name: &str) -> Option<SymbolId> {
        self.name_to_id.get(name).copied()
    }

    /// Returns true if the name is already interned.
    pub fn is_interned(&self, name: &str) -> bool {
        self.name_to_id.contains_key(name)
    }

    /// FEA-5: how many non-finite feature outputs were validated away. A live
    /// runner alerts when this grows (spec 009 P2) — suppression is counted
    /// and WARNed, never silent.
    pub fn nan_suppressed(&self) -> u64 {
        self.nan_suppressed
    }

    /// FEA-9 enforcement, called by the LIVE runner after registration:
    /// returns the ids of registered offline-only features, which a live
    /// process MUST treat as a startup error (offline features never run
    /// live). Empty ⇒ safe to go live. Offline/backtest runners skip this.
    pub fn offline_only_features(&self) -> Vec<String> {
        self.tick_factories
            .iter()
            .map(|f| f())
            .filter(|f| f.locality() == Locality::Offline)
            .map(|f| f.id())
            .collect()
    }

    /// Register a tick-feature factory (one instance is built per symbol).
    /// Registers and interns the feature name at setup time (FEA-17).
    pub fn register_tick(&mut self, f: impl Fn() -> Box<dyn TickFeature> + 'static) -> &mut Self {
        // Create a sample instance to extract the name for interning.
        let sample = f();
        let name = sample.id();
        self.intern(&name);
        self.tick_factories.push(Box::new(f));
        self
    }

    /// Register a bar-feature factory.
    /// Registers and interns the feature name at setup time (FEA-17).
    pub fn register_bar(&mut self, f: impl Fn() -> Box<dyn BarFeature> + 'static) -> &mut Self {
        let sample = f();
        let name = sample.id();
        self.intern(&name);
        self.bar_factories.push(Box::new(f));
        self
    }

    fn state_for(&mut self, sym: SymbolId) -> &mut SymbolState {
        if self.per_symbol.contains_key(&sym) {
            return self.per_symbol.get_mut(&sym).unwrap();
        }
        // Phase 1: extract all names from factories (immutable borrow).
        let tick_names: Vec<String> = self.tick_factories.iter().map(|f| f().id()).collect();
        let bar_names: Vec<String> = self.bar_factories.iter().map(|f| f().id()).collect();
        // Phase 2: intern all names (mutable borrow, no outstanding imm borrows).
        let tick_ids: Vec<SymbolId> = tick_names.iter().map(|n| self.intern(n)).collect();
        let bar_ids: Vec<SymbolId> = bar_names.iter().map(|n| self.intern(n)).collect();
        // Phase 3: build factories (immutable borrow again).
        let ticks: Vec<Box<dyn TickFeature>> = self.tick_factories.iter().map(|f| f()).collect();
        let bars: Vec<Box<dyn BarFeature>> = self.bar_factories.iter().map(|f| f()).collect();
        let tf = self.bar_tf_ns;
        self.per_symbol.insert(sym, SymbolState {
            ticks,
            bars,
            builder: BarBuilder::new(tf),
            tick_ids,
            tick_names,
            bar_ids,
            bar_names,
        });
        self.per_symbol.get_mut(&sym).unwrap()
    }

    /// Feed one event; returns the feature updates it produced, in deterministic
    /// order (tick features first in registration order, then bar features when
    /// a bar closes).
    pub fn on_event(&mut self, ev: &mp_core::EventEnvelope) -> Vec<FeatureUpdate> {
        let sym = ev.symbol;
        let venue = ev.venue;
        let ts = ev.recv_ts_ns;
        let st = self.state_for(sym);
        let mut out = Vec::new();

        let mut suppressed = 0u64;
        for i in 0..st.ticks.len() {
            let fid = st.tick_ids[i];
            if let Some(v) = st.ticks[i].on_event(ev) {
                // FEA-5 / CONV-8: validate → suppress non-finite → count → WARN.
                if !v.is_finite() {
                    suppressed += 1;
                    tracing::warn!(feature = %st.tick_names[i], symbol = sym.0, "non-finite feature output suppressed (FEA-5)");
                    continue;
                }
                if st.ticks[i].warm() {
                    out.push(FeatureUpdate {
                        feature: fid,
                        venue,
                        symbol: sym,
                        ts_ns: ts,
                        value: v,
                        ver: st.ticks[i].ver(),
                    });
                }
            }
        }

        if let Some(bar) = st.builder.on_event(ts, &ev.body) {
            let close_ts = bar.close_ts_ns;
            for i in 0..st.bars.len() {
                let fid = st.bar_ids[i];
                if let Some(v) = st.bars[i].on_bar(&bar) {
                    if !v.is_finite() {
                        suppressed += 1;
                        tracing::warn!(feature = %st.bar_names[i], symbol = sym.0, "non-finite feature output suppressed (FEA-5)");
                        continue;
                    }
                    if st.bars[i].warm() {
                        out.push(FeatureUpdate {
                            feature: fid,
                            venue,
                            symbol: sym,
                            ts_ns: close_ts,
                            value: v,
                            ver: st.bars[i].ver(),
                        });
                    }
                }
            }
        }
        self.nan_suppressed += suppressed;
        out
    }

    /// Convenience: run a whole sequence and collect all updates. Used by both
    /// live and offline paths — identical output proves the one-code-path
    /// guarantee (FEA-4).
    pub fn run<'a>(
        &mut self,
        events: impl IntoIterator<Item = &'a EventEnvelope>,
    ) -> Vec<FeatureUpdate> {
        let mut out = Vec::new();
        for ev in events {
            out.extend(self.on_event(ev));
        }
        out
    }
}
