//! Feature engine (FEA-1..4). Feeds events in stream order to per-symbol
//! feature instances; as-of ordering is structural (a feature only ever sees
//! events up to now — there is no API to query external "current" state,
//! PD-3/FEA-2). Warmup outputs are suppressed (FEA-3). Deterministic: features
//! run in a fixed registration order, symbols in `BTreeMap` order (CONV-10),
//! so an identical event sequence yields an identical update sequence (FEA-4).

use crate::bar::{Bar, BarBuilder};
use mp_core::{EventEnvelope, SymbolId};
use std::collections::BTreeMap;

/// A single feature output at a point in time.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureUpdate {
    pub feature: String,
    pub symbol: SymbolId,
    pub ts_ns: i64,
    pub value: f64,
    pub ver: u16,
}

/// A feature computed on every event (order flow, book, derivatives passthrough).
pub trait TickFeature {
    fn id(&self) -> String;
    fn ver(&self) -> u16 {
        1
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
}

/// The streaming engine. Register feature factories, then feed events.
pub struct FeatureEngine {
    tick_factories: Vec<TickFactory>,
    bar_factories: Vec<BarFactory>,
    bar_tf_ns: i64,
    per_symbol: BTreeMap<SymbolId, SymbolState>,
}

impl FeatureEngine {
    pub fn new(bar_tf_ns: i64) -> Self {
        Self {
            tick_factories: Vec::new(),
            bar_factories: Vec::new(),
            bar_tf_ns,
            per_symbol: BTreeMap::new(),
        }
    }

    /// Register a tick-feature factory (one instance is built per symbol).
    pub fn register_tick(&mut self, f: impl Fn() -> Box<dyn TickFeature> + 'static) -> &mut Self {
        self.tick_factories.push(Box::new(f));
        self
    }

    /// Register a bar-feature factory.
    pub fn register_bar(&mut self, f: impl Fn() -> Box<dyn BarFeature> + 'static) -> &mut Self {
        self.bar_factories.push(Box::new(f));
        self
    }

    fn state_for(&mut self, sym: SymbolId) -> &mut SymbolState {
        let (ticks, bars, tf) = (&self.tick_factories, &self.bar_factories, self.bar_tf_ns);
        self.per_symbol.entry(sym).or_insert_with(|| SymbolState {
            ticks: ticks.iter().map(|f| f()).collect(),
            bars: bars.iter().map(|f| f()).collect(),
            builder: BarBuilder::new(tf),
        })
    }

    /// Feed one event; returns the feature updates it produced, in deterministic
    /// order (tick features first in registration order, then bar features when
    /// a bar closes).
    pub fn on_event(&mut self, ev: &EventEnvelope) -> Vec<FeatureUpdate> {
        let sym = ev.symbol;
        let ts = ev.recv_ts_ns;
        let st = self.state_for(sym);
        let mut out = Vec::new();

        for f in st.ticks.iter_mut() {
            if let Some(v) = f.on_event(ev) {
                if f.warm() {
                    out.push(FeatureUpdate {
                        feature: f.id(),
                        symbol: sym,
                        ts_ns: ts,
                        value: v,
                        ver: f.ver(),
                    });
                }
            }
        }

        if let Some(bar) = st.builder.on_event(ts, &ev.body) {
            let close_ts = bar.close_ts_ns;
            for f in st.bars.iter_mut() {
                if let Some(v) = f.on_bar(&bar) {
                    if f.warm() {
                        out.push(FeatureUpdate {
                            feature: f.id(),
                            symbol: sym,
                            ts_ns: close_ts,
                            value: v,
                            ver: f.ver(),
                        });
                    }
                }
            }
        }
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
