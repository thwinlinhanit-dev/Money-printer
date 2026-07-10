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

/// One `feature <op> threshold` condition.
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

/// Evaluates rules against streaming feature updates.
pub struct Screener {
    rules: Vec<Rule>,
    snapshots: BTreeMap<SymbolId, BTreeMap<String, f64>>,
    active: BTreeMap<(SymbolId, String), bool>,
}

impl Screener {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            snapshots: BTreeMap::new(),
            active: BTreeMap::new(),
        }
    }

    /// Feed one feature update; returns any rules that transitioned to true.
    pub fn on_update(&mut self, u: &FeatureUpdate) -> Vec<ScreenerHit> {
        let snap = self.snapshots.entry(u.symbol).or_default();
        snap.insert(u.feature.clone(), u.value);
        let snap = self.snapshots.get(&u.symbol).unwrap();

        let mut hits = Vec::new();
        for rule in &self.rules {
            let satisfied = rule.conds.iter().all(|c| {
                snap.get(&c.feature)
                    .is_some_and(|&v| c.op.test(v, c.threshold))
            });
            let key = (u.symbol, rule.id.clone());
            let was = self.active.get(&key).copied().unwrap_or(false);
            if satisfied && !was {
                hits.push(ScreenerHit {
                    rule_id: rule.id.clone(),
                    symbol: u.symbol,
                    ts_ns: u.ts_ns,
                    snapshot: snap.clone(),
                });
            }
            self.active.insert(key, satisfied);
        }
        hits
    }
}
