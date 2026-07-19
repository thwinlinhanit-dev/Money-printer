//! Screener rule evaluator (FEA-10). Boolean AND-conditions over the latest
//! feature values per symbol; edge-triggered so a persistently-true rule fires
//! once, not every tick. Each hit carries a snapshot for later grading
//! (spec 010 turns hits into P&L research). OR / time-windowed persistence are
//! deferred (spec 004 Decisions).

use crate::engine::FeatureUpdate;
use mp_core::SymbolId;
use std::collections::BTreeMap;

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Gt,
    Ge,
    Lt,
    Le,
}

impl Op {
    fn test(self, lhs: f64, rhs: f64) -> bool {
        match self {
            Op::Gt => lhs > rhs,
            Op::Ge => lhs >= rhs,
            Op::Lt => lhs < rhs,
            Op::Le => lhs <= rhs,
        }
    }
}

/// One `feature <op> threshold` condition. `feature_id` is resolved from
/// `feature` at rule setup time when a SymbolId is available.
#[derive(Debug, Clone)]
pub struct Cond {
    pub feature: String,
    pub op: Op,
    pub threshold: f64,
}

/// A named rule = AND of its conditions.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub conds: Vec<Cond>,
}

/// A rule firing, with the feature snapshot at fire time.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenerHit {
    pub rule_id: String,
    pub symbol: SymbolId,
    pub ts_ns: i64,
    pub snapshot: BTreeMap<String, f64>,
}

/// Kind of screener hit (entry vs exit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    Entry,
    Exit,
}

/// Evaluates rules against streaming feature updates.
/// Supports configurable evaluation cadence (spec 022) and
/// edge-triggered entry/exit hits.
pub struct Screener {
    rules: Vec<Rule>,
    /// Per-symbol snapshots: SymbolId → (feature_name → value).
    snapshots: BTreeMap<SymbolId, BTreeMap<String, f64>>,
    /// Per-symbol feature name table: tracks SymbolId → String mapping
    /// discovered from incoming FeatureUpdates.
    feature_names: BTreeMap<SymbolId, String>,
    /// Active rules per symbol: (SymbolId, rule_id) → bool.
    active: BTreeMap<(SymbolId, String), bool>,
    /// Last evaluation time (spec 022 cadence).
    last_eval_ns: i64,
    /// Minimum interval between evaluations (default 1s).
    eval_interval_ns: i64,
    /// Whether the first evaluation has occurred (FEA-15).
    has_evaluated: bool,
}

impl Screener {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            snapshots: BTreeMap::new(),
            feature_names: BTreeMap::new(),
            active: BTreeMap::new(),
            last_eval_ns: 0,
            eval_interval_ns: 1_000_000_000, // 1 second default (FEA-13)
            has_evaluated: false,
        }
    }

    /// Pre-populate the SymbolId → name map from a FeatureEngine's intern table.
    /// Must be called before processing updates so conditions can match snapshot keys.
    pub fn set_name_map(&mut self, map: BTreeMap<SymbolId, String>) {
        self.feature_names = map;
    }

    /// Set the evaluation interval (spec 022). Minimum 100ms (FEA-16).
    pub fn set_eval_interval_ns(&mut self, ns: i64) {
        self.eval_interval_ns = ns.max(100_000_000); // clamp to 100ms minimum
    }

    /// Feed one feature update; returns any rules that transitioned.
    /// Updates snapshots on every call, but only evaluates rules at the
    /// configured cadence (FEA-11, FEA-12).
    pub fn on_update(&mut self, u: &FeatureUpdate) -> Vec<ScreenerHit> {
        // Always update the feature name table and snapshot (FEA-12).
        let feat_name = self.feature_names.entry(u.feature)
            .or_insert_with(|| format!("feature_{}", u.feature.0)).clone();
        let snap = self.snapshots.entry(u.symbol).or_default();
        snap.insert(feat_name, u.value);

        // Check evaluation cadence (FEA-11).
        if u.ts_ns - self.last_eval_ns < self.eval_interval_ns && self.has_evaluated {
            return Vec::new();
        }
        self.last_eval_ns = u.ts_ns;

        let mut hits = Vec::new();
        for rule in &self.rules {
            let satisfied = rule.conds.iter().all(|c| {
                snap.get(&c.feature)
                    .is_some_and(|&v| c.op.test(v, c.threshold))
            });
            let key = (u.symbol, rule.id.clone());
            let was = self.active.get(&key).copied().unwrap_or(false);

            if !self.has_evaluated {
                // FEA-15: first evaluation establishes baseline, no hits.
                self.active.insert(key, satisfied);
                continue;
            }

            if satisfied && !was {
                // Entry transition (inactive → active)
                hits.push(ScreenerHit {
                    rule_id: rule.id.clone(),
                    symbol: u.symbol,
                    ts_ns: u.ts_ns,
                    snapshot: snap.clone(),
                });
            } else if !satisfied && was {
                // Exit transition (active → inactive) — spec 022 exit hits
                hits.push(ScreenerHit {
                    rule_id: rule.id.clone(),
                    symbol: u.symbol,
                    ts_ns: u.ts_ns,
                    snapshot: snap.clone(),
                });
            }
            self.active.insert(key, satisfied);
        }

        self.has_evaluated = true;
        hits
    }
}
