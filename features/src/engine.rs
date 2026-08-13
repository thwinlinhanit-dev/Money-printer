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

/// A single feature output at a point in time. The `feature` field is an
/// interned `SymbolId` (spec 023 FEA-16 — no heap `String` in hot identity);
/// `name` carries the resolved feature name (e.g. `funding.rate`) so
/// strategies can self-filter on THEIR subscription set instead of trusting a
/// dispatcher to have done it (audit C1: carry-v1 once read any feature
/// update, e.g. CVD, as a funding rate).
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureUpdate {
    /// Interned feature name (SymbolId).
    pub feature: SymbolId,
    /// Resolved feature name (`id()` of the producing feature). Strategies use
    /// this to assert the signal they subscribed to is the one they got.
    pub name: String,
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
    /// Venue from the first event seen on this symbol (finish() stamps it).
    venue: Venue,
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
    /// Global tick feature factories: ONE instance total, fed EVERY event
    /// (any symbol/venue). The per-symbol model keys state by `SymbolId`, and
    /// cross-venue symbols are distinct ids (EVT-8 re-intern per (venue,
    /// name)) — so any feature that must compare state ACROSS venues or
    /// symbols (liq.delta.{a}_{b}, px.divergence, leadlag, cvd.agg) cannot
    /// live in per-symbol state; it registers here instead (spec 004 FEA-20).
    global_factories: Vec<TickFactory>,
    globals: Vec<Box<dyn TickFeature>>,
    global_ids: Vec<SymbolId>,
    global_names: Vec<String>,
    /// FEA-5: non-finite outputs suppressed (never emitted downstream).
    nan_suppressed: u64,
    /// Feature name intern table (spec 023 FEA-17): name → SymbolId.
    name_to_id: BTreeMap<String, SymbolId>,
    /// Reverse lookup: SymbolId → name (for resolution in logs/screener).
    id_to_name: BTreeMap<SymbolId, String>,
    next_feature_id: u32,
}

// Spec 023 FEA-18: SymbolId(0) is reserved as null/invalid.

impl FeatureEngine {
    pub fn new(bar_tf_ns: i64) -> Self {
        Self {
            tick_factories: Vec::new(),
            bar_factories: Vec::new(),
            bar_tf_ns,
            per_symbol: BTreeMap::new(),
            global_factories: Vec::new(),
            globals: Vec::new(),
            global_ids: Vec::new(),
            global_names: Vec::new(),
            nan_suppressed: 0,
            name_to_id: BTreeMap::new(),
            id_to_name: BTreeMap::new(),
            next_feature_id: 1, // spec 023 FEA-18: ids start from 1
        }
    }

    /// Intern a feature name, returning a stable SymbolId (spec 023 FEA-17).
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
        let mut out: Vec<String> = self
            .tick_factories
            .iter()
            .map(|f| f())
            .filter(|f| f.locality() == Locality::Offline)
            .map(|f| f.id())
            .collect();
        out.extend(
            self.global_factories
                .iter()
                .map(|f| f())
                .filter(|f| f.locality() == Locality::Offline)
                .map(|f| f.id()),
        );
        out
    }

    /// Register a tick-feature factory (one instance is built per symbol).
    /// Registers and interns the feature name at setup time (spec 023 FEA-17:
    /// interning happens at setup, not at runtime).
    pub fn register_tick(&mut self, f: impl Fn() -> Box<dyn TickFeature> + 'static) -> &mut Self {
        // Create a sample instance to extract the name for interning.
        let sample = f();
        let name = sample.id();
        self.intern(&name);
        self.tick_factories.push(Box::new(f));
        self
    }

    /// Register a GLOBAL tick-feature factory: ONE instance receives every
    /// event regardless of symbol/venue (spec 004 FEA-20 — the cross-venue
    /// seam). Same name interning at setup as `register_tick`. A global
    /// feature must be venue/symbol-agnostic in its *state* (it may still
    /// key its own internal maps by venue/symbol); its output is stamped
    /// with the triggering event's venue/symbol.
    pub fn register_global_tick(
        &mut self,
        f: impl Fn() -> Box<dyn TickFeature> + 'static,
    ) -> &mut Self {
        let sample = f();
        let name = sample.id();
        self.intern(&name);
        self.global_factories.push(Box::new(f));
        self
    }

    /// Lazily materialize the global feature instances on first event (they
    /// are built ONCE, not per symbol).
    fn ensure_globals(&mut self) {
        if !self.globals.is_empty() || self.global_factories.is_empty() {
            return;
        }
        let names: Vec<String> = self.global_factories.iter().map(|f| f().id()).collect();
        let ids: Vec<SymbolId> = names.iter().map(|n| self.intern(n)).collect();
        let globals: Vec<Box<dyn TickFeature>> = self.global_factories.iter().map(|f| f()).collect();
        self.global_names = names;
        self.global_ids = ids;
        self.globals = globals;
    }

    /// Register a bar-feature factory.
    /// Registers and interns the feature name at setup time (spec 023 FEA-17).
    pub fn register_bar(&mut self, f: impl Fn() -> Box<dyn BarFeature> + 'static) -> &mut Self {
        let sample = f();
        let name = sample.id();
        self.intern(&name);
        self.bar_factories.push(Box::new(f));
        self
    }

    fn state_for(&mut self, sym: SymbolId, venue: Venue) -> &mut SymbolState {
        if self.per_symbol.contains_key(&sym) {
            // SAFETY (CONV-13): the contains_key guard above proves the entry.
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
        self.per_symbol.insert(
            sym,
            SymbolState {
                ticks,
                bars,
                builder: BarBuilder::new(tf),
                venue,
                tick_ids,
                tick_names,
                bar_ids,
                bar_names,
            },
        );
        // SAFETY (CONV-13): we just inserted this key above.
        self.per_symbol.get_mut(&sym).unwrap()
    }

    /// Feed one event; returns the feature updates it produced, in deterministic
    /// order (tick features first in registration order, then bar features when
    /// a bar closes).
    pub fn on_event(&mut self, ev: &mp_core::EventEnvelope) -> Vec<FeatureUpdate> {
        let sym = ev.symbol;
        let venue = ev.venue;
        let ts = ev.recv_ts_ns;
        let st = self.state_for(sym, venue);
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
                        name: st.tick_names[i].clone(),
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
                            name: st.bar_names[i].clone(),
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
        // Global (cross-venue) tick features: ONE instance, fed every event.
        // Runs after the per-symbol ticks in registration order (deterministic
        // update order — spec 018). Outputs are stamped with THIS event's
        // venue/symbol so downstream rows are per-symbol as usual (FEA-20).
        self.ensure_globals();
        for i in 0..self.globals.len() {
            let fid = self.global_ids[i];
            if let Some(v) = self.globals[i].on_event(ev) {
                if !v.is_finite() {
                    suppressed += 1;
                    tracing::warn!(
                        feature = %self.global_names[i],
                        symbol = sym.0,
                        "non-finite global feature output suppressed (FEA-5)"
                    );
                    continue;
                }
                if self.globals[i].warm() {
                    out.push(FeatureUpdate {
                        feature: fid,
                        name: self.global_names[i].clone(),
                        venue,
                        symbol: sym,
                        ts_ns: ts,
                        value: v,
                        ver: self.globals[i].ver(),
                    });
                }
            }
        }
        self.nan_suppressed += suppressed;
        out
    }

    /// End-of-stream hook: close every symbol's in-flight partial bar through
    /// [`BarBuilder::finish`] and run bar features on the final bars, returning
    /// their updates in symbol (BTreeMap) order. Offline/replay loops MUST call
    /// this once after the event loop, or the last partial bar per symbol is
    /// silently dropped. Live mode never needs it: the next tick after a bucket
    /// boundary closes the bar naturally (`now_ns` must be >= the last event
    /// time; it documents the call site's end-of-stream time).
    pub fn finish(&mut self, now_ns: i64) -> Vec<FeatureUpdate> {
        let mut out = Vec::new();
        let mut suppressed = 0u64;
        for (&sym, st) in &mut self.per_symbol {
            let Some(bar) = st.builder.finish(now_ns) else {
                continue;
            };
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
                            name: st.bar_names[i].clone(),
                            venue: st.venue,
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
    /// guarantee (FEA-4). Does NOT auto-finish partial bars: a live runner
    /// calls this in a loop forever; an offline runner calls [`Self::finish`]
    /// once afterwards to emit the final partial bars.
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
