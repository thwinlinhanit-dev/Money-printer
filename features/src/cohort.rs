//! Wallet cohort grading (spec 042, WCG). Offline scorer classifying
//! Hyperliquid addresses into SmartMoney / Whale / Retail / Dormant from
//! recorded WhalePosition history — a pure function of (events, config)
//! (WCG-1, PD-3). Priority: Dormant > Whale > SmartMoney > Retail; the
//! first matching tier wins.
//!
//! The weekly snapshot is journaled atomically (`data/cohorts/{date}.json`,
//! WCG-9); the live [`CohortFeature`] family consumes it and fails closed
//! when the snapshot is stale (>7 days, WCG-8). Four aggregates are
//! registered (WCG-7): `cohort.whale_ratio`, `cohort.net_delta.{cohort}`,
//! `cohort.smart_flow.{w}`, `cohort.concentration`.

use mp_core::{EventEnvelope, MarketEvent};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Cohort label (BTreeMap iteration order = discriminant order, CONV-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Cohort {
    SmartMoney,
    Whale,
    Retail,
    Dormant,
}

impl Cohort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Cohort::SmartMoney => "smart_money",
            Cohort::Whale => "whale",
            Cohort::Retail => "retail",
            Cohort::Dormant => "dormant",
        }
    }
}

/// Config (spec 042 defaults; deny_unknown_fields per CONV-16).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortConfig {
    /// No activity for this long ⇒ Dormant (WCG-4). Default 30d.
    #[serde(default = "default_dormant_ns")]
    pub dormant_threshold_ns: i64,
    /// Mean |size × entry| at or above this ⇒ Whale regardless of PnL
    /// (WCG-5). Default $100k.
    #[serde(default = "default_whale_usd")]
    pub whale_threshold_usd: f64,
    /// Closed cycles required to qualify as Smart Money (WCG-6). Default 5.
    #[serde(default = "default_min_trades")]
    pub min_smart_trades: u64,
    /// Win-rate floor for Smart Money (default 0.45).
    #[serde(default = "default_win_rate")]
    pub win_rate_min: f64,
    /// Approx-Sharpe floor for Smart Money (default 0.5; needs ≥3 cycles).
    #[serde(default = "default_min_sharpe")]
    pub min_smart_sharpe: f64,
    /// Weekly-snapshot staleness cap for live features (WCG-8). Default 7d.
    #[serde(default = "default_snapshot_max_age")]
    pub snapshot_max_age_ns: i64,
    /// Smart-flow aggregation window (default 24h → `cohort.smart_flow.24h`).
    #[serde(default = "default_smart_flow_window")]
    pub smart_flow_window_ns: i64,
    /// Optional path to a pre-scored weekly snapshot
    /// (`data/cohorts/{date}.json`, WCG-9). When set at engine-build time the
    /// live features start from that snapshot; absent/None ⇒ fail-closed
    /// suppression until an operator loads one.
    #[serde(default)]
    pub snapshot_path: Option<String>,
    /// Enable the live `cohort.*` feature family registration.
    #[serde(default)]
    pub enabled: bool,
}

fn default_dormant_ns() -> i64 {
    30 * DAY_NS
}
fn default_whale_usd() -> f64 {
    100_000.0
}
fn default_min_trades() -> u64 {
    5
}
fn default_win_rate() -> f64 {
    0.45
}
fn default_min_sharpe() -> f64 {
    0.5
}
fn default_snapshot_max_age() -> i64 {
    7 * DAY_NS
}
fn default_smart_flow_window() -> i64 {
    DAY_NS
}
const DAY_NS: i64 = 86_400_000_000_000;

impl Default for CohortConfig {
    fn default() -> Self {
        Self {
            dormant_threshold_ns: default_dormant_ns(),
            whale_threshold_usd: default_whale_usd(),
            min_smart_trades: default_min_trades(),
            win_rate_min: default_win_rate(),
            min_smart_sharpe: default_min_sharpe(),
            snapshot_max_age_ns: default_snapshot_max_age(),
            smart_flow_window_ns: default_smart_flow_window(),
            snapshot_path: None,
            enabled: false,
        }
    }
}

/// Per-address metrics folded from WhalePosition history (spec 042 table).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WalletMetrics {
    pub realized_pnl: f64,
    pub trade_count: u64,
    pub wins: u64,
    pub avg_position_size: f64,
    pub last_activity_ts_ns: i64,
    cycle_pnls: Vec<f64>,
    position_samples: u64,
}

impl WalletMetrics {
    /// Sharpe approximation: realized_pnl / stddev(per-cycle PnL); None below
    /// 3 cycles (division-by-near-zero guard, spec 042 Decisions).
    pub fn sharpe_approx(&self) -> Option<f64> {
        if self.cycle_pnls.len() < 3 {
            return None;
        }
        let n = self.cycle_pnls.len() as f64;
        let mean = self.cycle_pnls.iter().sum::<f64>() / n;
        let var = self
            .cycle_pnls
            .iter()
            .map(|p| (p - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0);
        if !var.is_finite() || var <= 0.0 {
            return None;
        }
        Some(self.realized_pnl / var.sqrt())
    }

    pub fn win_rate(&self) -> f64 {
        if self.trade_count == 0 {
            return 0.0;
        }
        self.wins as f64 / self.trade_count as f64
    }
}

/// Per-address event fold state (open-position tracking for cycle PnL).
#[derive(Debug, Default)]
struct WalletFold {
    metrics: WalletMetrics,
    sum_abs_notional: f64,
    /// Open position: (side_sign, |size|, entry_price, open_ts_ns).
    open: Option<(f64, f64, f64, i64)>,
    open_bars_sum_ns: i64,
    open_cycles: u64,
}

/// Deterministic wallet scorer (WCG-1/2): BTreeMap keyed by address.
#[derive(Debug, Default)]
pub struct WalletScorer {
    folds: BTreeMap<String, WalletFold>,
}

impl WalletScorer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one recorded event; only WhalePosition bodies mutate state.
    pub fn feed(&mut self, ev: &EventEnvelope) -> bool {
        let MarketEvent::WhalePosition {
            address,
            size,
            entry,
            leverage,
            ..
        } = &ev.body
        else {
            return false;
        };
        if !size.is_finite() || !entry.is_finite() || *entry <= 0.0 {
            return false; // non-finite inputs fail closed (CONV-8), no panic
        }
        let fold = self.folds.entry(address.clone()).or_default();
        let m = &mut fold.metrics;
        m.last_activity_ts_ns = ev.recv_ts_ns;
        m.position_samples += 1;

        let notional = size.abs() * entry;
        fold.sum_abs_notional += notional;
        m.avg_position_size = fold.sum_abs_notional / m.position_samples as f64;
        let _ = leverage;

        match fold.open {
            None => {
                // Position opening (or first observation of an existing one —
                // entry is its baseline either way).
                fold.open = Some((size.signum(), size.abs(), *entry, ev.recv_ts_ns));
            }
            Some((sign, abs_size, entry_px, open_ts)) => {
                let same_side = size.signum() == sign && size.abs() >= abs_size * 0.999;
                if same_side {
                    // Scale-in/add-on: refresh the entry baseline by volume
                    // weight (deterministic two-point average).
                    let new_abs = size.abs();
                    let w = abs_size.max(f64::EPSILON);
                    fold.open = Some((
                        sign,
                        new_abs,
                        (entry_px * w + *entry * (new_abs - abs_size)) / new_abs,
                        open_ts,
                    ));
                } else {
                    // Position closed or flipped: realize the cycle PnL from
                    // exit price vs entry, on the smaller of the two sizes.
                    let closed_size = abs_size.min(size.abs());
                    let pnl = (*entry - entry_px) * closed_size * sign;
                    if pnl.is_finite() {
                        fold.metrics.realized_pnl += pnl;
                        fold.metrics.trade_count += 1;
                        fold.metrics.cycle_pnls.push(pnl);
                        if pnl > 0.0 {
                            fold.metrics.wins += 1;
                        }
                        fold.open_cycles += 1;
                        fold.open_bars_sum_ns += ev.recv_ts_ns - open_ts;
                    }
                    if size.abs() > f64::EPSILON {
                        fold.open = Some((size.signum(), size.abs(), *entry, ev.recv_ts_ns));
                    } else {
                        fold.open = None;
                    }
                }
            }
        }
        true
    }

    /// Per-address metrics snapshot (address ascending, CONV-10).
    pub fn metrics(&self) -> BTreeMap<&str, &WalletMetrics> {
        self.folds
            .iter()
            .map(|(a, f)| (a.as_str(), &f.metrics))
            .collect()
    }

    /// Classify every tracked address as of `now_ns` (WCG-2/3/4/5/6).
    pub fn classify(&self, now_ns: i64, cfg: &CohortConfig) -> BTreeMap<&str, Cohort> {
        self.folds
            .iter()
            .map(|(addr, fold)| (addr.as_str(), classify_one(&fold.metrics, now_ns, cfg)))
            .collect()
    }
}

fn classify_one(m: &WalletMetrics, now_ns: i64, cfg: &CohortConfig) -> Cohort {
    if m.last_activity_ts_ns < now_ns.saturating_sub(cfg.dormant_threshold_ns) {
        return Cohort::Dormant; // temporal state beats any prior label
    }
    if m.avg_position_size.is_finite() && m.avg_position_size >= cfg.whale_threshold_usd {
        return Cohort::Whale;
    }
    let sharpe_ok = m.sharpe_approx().is_some_and(|s| s >= cfg.min_smart_sharpe);
    if m.realized_pnl.is_finite()
        && m.realized_pnl > 0.0
        && m.trade_count >= cfg.min_smart_trades
        && m.win_rate() >= cfg.win_rate_min
        && sharpe_ok
    {
        return Cohort::SmartMoney;
    }
    Cohort::Retail
}

// ---- weekly snapshot + journal (WCG-9/10) -----------------------------------

/// Address → cohort map with the scoring timestamp.
pub type CohortSnapshot = (i64, BTreeMap<String, Cohort>);

/// Atomic-write the snapshot JSON (`tmp` + rename, W-6/WCG-9).
pub fn save_snapshot(path: &std::path::Path, snap: &CohortSnapshot) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let (ts, map): &(i64, BTreeMap<String, Cohort>) = snap;
    let json = serde_json::json!({ "scored_at_ns": ts, "cohorts": map });
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(&json).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Load a snapshot; None when absent/corrupt (caller suppresses, WCG-8).
pub fn load_snapshot(path: &std::path::Path) -> Option<CohortSnapshot> {
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let ts = v.get("scored_at_ns")?.as_i64()?;
    let map = v.get("cohorts")?.as_object()?;
    let mut out = BTreeMap::new();
    for (addr, c) in map {
        out.insert(
            addr.clone(),
            serde_json::from_value::<Cohort>(c.clone()).ok()?,
        );
    }
    Some((ts, out))
}

/// Append changed assignments to `journal/cohort_changes.jsonl` (WCG-9).
/// Each line carries per-address: old_cohort, new_cohort, score_breakdown
/// (from `new_metrics`), ts_ns. Returns the number of diff lines written.
pub fn journal_changes(
    journal_path: &std::path::Path,
    old: Option<&BTreeMap<String, Cohort>>,
    new: &BTreeMap<String, Cohort>,
    new_metrics: &BTreeMap<String, WalletMetrics>,
    ts_ns: i64,
) -> Result<usize, String> {
    use std::io::Write;
    let old_ref = old.cloned().unwrap_or_default();
    let mut lines = String::new();
    let mut n = 0usize;
    let mut all: BTreeSet<&String> = new.keys().collect();
    all.extend(old_ref.keys());
    for addr in all {
        let o = old_ref.get(addr);
        let w = new.get(addr);
        if o != w {
            let breakdown = new_metrics.get(addr).map(|m| {
                serde_json::json!({
                    "realized_pnl": m.realized_pnl,
                    "trade_count": m.trade_count,
                    "win_rate": m.win_rate(),
                    "avg_position_size": m.avg_position_size,
                    "sharpe_approx": m.sharpe_approx(),
                    "last_activity_ts_ns": m.last_activity_ts_ns,
                })
            });
            let line = serde_json::json!({
                "address": addr,
                "old_cohort": o.map(Cohort::as_str),
                "new_cohort": w.map(Cohort::as_str),
                "score_breakdown": breakdown,
                "ts_ns": ts_ns,
            });
            lines.push_str(&serde_json::to_string(&line).map_err(|e| e.to_string())?);
            lines.push('\n');
            n += 1;
        }
    }
    if n > 0 {
        if let Some(dir) = journal_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(journal_path)
            .map_err(|e| e.to_string())?;
        f.write_all(lines.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(n)
}

/// Which `cohort.*` aggregate a [`CohortFeature`] instance emits (WCG-7).
/// All four are GLOBAL tick features (FEA-20 — membership spans symbols);
/// each keeps per-symbol state internally and its output is stamped with the
/// triggering event's symbol by the engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CohortField {
    /// `cohort.whale_ratio.{symbol}` — whale notional / total tracked OI (0..1).
    WhaleRatio,
    /// `cohort.net_delta.{symbol}.{cohort}` — Σ signed notional of the cohort.
    NetDelta(Cohort),
    /// `cohort.smart_flow.{symbol}.{w}` — rolling windowed smart-money flow.
    SmartFlow,
    /// `cohort.concentration.{symbol}` — HHI of OI across the four cohorts.
    Concentration,
}

/// Per-symbol census state: address → (last_seen_ns, signed notional).
/// Last-wins upsert + stale eviction (the whale.net pattern, spec 028).
#[derive(Debug, Default)]
struct CohortSymState {
    members: BTreeMap<String, (i64, f64)>,
    /// Smart-flow event log within the window (ts_ns, Δnotional).
    flow: std::collections::VecDeque<(i64, f64)>,
}

impl CohortSymState {
    fn upsert(&mut self, addr: &str, ts_ns: i64, notional: f64, stale_after_ns: i64) {
        self.members.insert(addr.to_owned(), (ts_ns, notional));
        let cutoff = ts_ns - stale_after_ns;
        self.members.retain(|_, &mut (ts, _)| ts >= cutoff);
    }

    fn cohort_sum(&self, snap: &BTreeMap<String, Cohort>, c: Cohort) -> f64 {
        self.members
            .iter()
            .filter(|(a, _)| snap.get(*a) == Some(&c))
            .map(|(_, &(_, n))| n)
            .sum()
    }
}

/// Live global-tick feature family (WCG-7/8). Emits the selected [`CohortField`]
/// aggregate from WhalePosition events whose address IS a snapshot member;
/// suppressed (None) while the snapshot is missing or older than
/// `snapshot_max_age_ns` (fail-closed, never a heuristic fallback).
pub struct CohortFeature {
    field: CohortField,
    snapshot: Option<(i64, BTreeMap<String, Cohort>)>,
    max_age_ns: i64,
    smart_flow_window_ns: i64,
    stale_after_ns: i64,
    syms: BTreeMap<mp_core::SymbolId, CohortSymState>,
}

impl CohortFeature {
    pub fn new(snap: Option<CohortSnapshot>, field: CohortField, max_age_ns: i64) -> Self {
        Self::with_windows(snap, field, max_age_ns, DAY_NS)
    }

    pub fn with_windows(
        snap: Option<CohortSnapshot>,
        field: CohortField,
        max_age_ns: i64,
        smart_flow_window_ns: i64,
    ) -> Self {
        Self {
            field,
            snapshot: snap,
            max_age_ns,
            smart_flow_window_ns: smart_flow_window_ns.max(1),
            // Census refresh window (spec 028 cadence ×10, same as whale.rs):
            // an address absent from polls this long is out of the live book.
            stale_after_ns: 600_000_000_000,
            syms: BTreeMap::new(),
        }
    }

    fn id_for(field: &CohortField, smart_flow_window_ns: i64) -> String {
        match field {
            CohortField::WhaleRatio => "cohort.whale_ratio".into(),
            CohortField::NetDelta(c) => format!("cohort.net_delta.{}", c.as_str()),
            CohortField::SmartFlow => {
                let h = smart_flow_window_ns / 3_600_000_000_000;
                if smart_flow_window_ns % 3_600_000_000_000 == 0 && h > 0 {
                    format!("cohort.smart_flow.{h}h")
                } else {
                    format!("cohort.smart_flow.{}ns", smart_flow_window_ns)
                }
            }
            CohortField::Concentration => "cohort.concentration".into(),
        }
    }

    /// Fresh-snapshot guard (WCG-8): false when missing/stale. Deliberately
    /// returns a bool, not a reference, so callers may mutate other fields
    /// (disjoint borrows) while consulting it.
    fn snapshot_fresh(&self, now_ns: i64) -> bool {
        match &self.snapshot {
            Some((scored_at, _)) => now_ns.saturating_sub(*scored_at) <= self.max_age_ns,
            None => false,
        }
    }
}

impl crate::engine::TickFeature for CohortFeature {
    fn id(&self) -> String {
        Self::id_for(&self.field, self.smart_flow_window_ns)
    }

    fn on_event(&mut self, ev: &EventEnvelope) -> Option<f64> {
        let MarketEvent::WhalePosition {
            address,
            size,
            entry,
            ..
        } = &ev.body
        else {
            return None;
        };
        if !self.snapshot_fresh(ev.recv_ts_ns) {
            return None; // stale/missing snapshot — fail closed, never heuristic
        }
        let cohort = *self.snapshot.as_ref()?.1.get(address)?;
        let notional = size.abs() * entry;
        if !notional.is_finite() || *entry <= 0.0 {
            return None; // CONV-8: corrupt frame moves nothing
        }
        let signed = size.signum() * notional;
        let state = self.syms.entry(ev.symbol).or_default();
        state.upsert(address, ev.recv_ts_ns, signed, self.stale_after_ns);
        let (field, flow_w) = (self.field, self.smart_flow_window_ns);
        match field {
            CohortField::SmartFlow => {
                if cohort != Cohort::SmartMoney {
                    return None; // only smart-money prints move the flow
                }
                state.flow.push_back((ev.recv_ts_ns, signed));
                let cut = ev.recv_ts_ns - flow_w;
                while matches!(state.flow.front(), Some(&(t, _)) if t < cut) {
                    state.flow.pop_front();
                }
                Some(state.flow.iter().map(|&(_, d)| d).sum())
            }
            CohortField::WhaleRatio => {
                let total: f64 = state.members.values().map(|&(_, n)| n.abs()).sum();
                if total <= 0.0 {
                    return None;
                }
                let whale: f64 = state
                    .members
                    .iter()
                    .filter(|(a, _)| {
                        self.snapshot.as_ref().and_then(|m| m.1.get(*a)) == Some(&Cohort::Whale)
                    })
                    .map(|(_, &(_, n))| n.abs())
                    .sum();
                Some(whale / total)
            }
            CohortField::NetDelta(c) => Some(state.cohort_sum(&self.snapshot.as_ref()?.1, c)),
            CohortField::Concentration => {
                // HHI across the four cohorts over CURRENT census notional.
                let mut shares = [0.0f64; 4];
                let mut total = 0.0f64;
                for (a, &(_, n)) in &state.members {
                    let abs = n.abs();
                    match self.snapshot.as_ref().and_then(|s| s.1.get(a)) {
                        Some(Cohort::SmartMoney) => shares[0] += abs,
                        Some(Cohort::Whale) => shares[1] += abs,
                        Some(Cohort::Retail) => shares[2] += abs,
                        Some(Cohort::Dormant) => shares[3] += abs,
                        None => continue, // ungraded address — excluded entirely
                    }
                    total += abs;
                }
                if total <= 0.0 {
                    return None;
                }
                Some(shares.iter().map(|s| (s / total).powi(2)).sum())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TickFeature;
    use mp_core::Venue;
    use proptest::prelude::*;

    const NOW: i64 = 100 * DAY_NS;

    fn wp(addr: &str, _day: f64, size: f64, entry: f64, recv_day: i64) -> EventEnvelope {
        EventEnvelope::new(
            Venue::Hyperliquid,
            mp_core::SymbolId(9),
            recv_day * DAY_NS,
            recv_day * DAY_NS,
            1,
            MarketEvent::WhalePosition {
                address: addr.into(),
                size,
                entry,
                leverage: 2.0,
                liq_price: f64::NAN,
            },
        )
    }

    fn cfg() -> CohortConfig {
        CohortConfig {
            min_smart_trades: 3,
            min_smart_sharpe: 0.5,
            ..CohortConfig::default()
        }
    }

    /// Drive one address through `cycles` profitable round trips (varying
    /// exits so per-cycle PnL has nonzero variance for the Sharpe gate).
    fn profitable_wallet(scorer: &mut WalletScorer, addr: &str, cycles: usize, active_day: i64) {
        for k in 0..cycles {
            let d = active_day - cycles as i64 + k as i64;
            scorer.feed(&wp(addr, 0.0, 10.0, 100.0, d));
            scorer.feed(&wp(addr, 0.0, -10.0, 110.0 + k as f64 * 5.0, d)); // +50..+200
        }
        scorer.feed(&wp(addr, 0.0, 10.0, 100.0, active_day)); // stay active
    }

    #[test]
    fn wcg_3_degenerate_addresses_are_retail_or_skipped() {
        let mut s = WalletScorer::new();
        // Zero trades (single print) → cannot qualify Smart Money.
        s.feed(&wp("w-zero", 0.0, 10.0, 100.0, NOW / DAY_NS));
        // Non-finite entry is skipped entirely (never classified).
        assert!(!s.feed(&wp("w-nan", 0.0, f64::NAN, f64::NAN, NOW / DAY_NS)));
        let m = s.classify(NOW, &cfg());
        assert_eq!(m.get("w-zero"), Some(&Cohort::Retail));
        assert!(!s.folds.contains_key("w-nan"));
        // One losing cycle → Retail (negative PnL blocks Smart Money).
        let mut s2 = WalletScorer::new();
        s2.feed(&wp("w-loss", 0.0, 10.0, 110.0, NOW / DAY_NS - 1));
        s2.feed(&wp("w-loss", 0.0, -10.0, 100.0, NOW / DAY_NS));
        s2.feed(&wp("w-loss", 0.0, 10.0, 100.0, NOW / DAY_NS));
        assert_eq!(
            s2.classify(NOW, &cfg()).get("w-loss"),
            Some(&Cohort::Retail)
        );
    }

    #[test]
    fn wcg_4_dormancy_is_temporal_and_configurable() {
        let mut s = WalletScorer::new();
        profitable_wallet(&mut s, "w-smart", 4, 10);
        // Active at day 10 → Smart Money (PnL +400 over 4 cycles).
        assert_eq!(
            s.classify(10 * DAY_NS, &cfg()).get("w-smart"),
            Some(&Cohort::SmartMoney)
        );
        // Same wallet evaluated 31 days after its LAST activity → Dormant.
        assert_eq!(
            s.classify(41 * DAY_NS, &cfg()).get("w-smart"),
            Some(&Cohort::Dormant)
        );
        // Reactivation re-classifies from history (not reset to Retail).
        s.feed(&wp("w-smart", 0.0, 12.0, 105.0, 41));
        assert_ne!(
            s.classify(41 * DAY_NS, &cfg()).get("w-smart"),
            Some(&Cohort::Dormant)
        );
    }

    #[test]
    fn wcg_5_whale_by_notional_regardless_of_pnl() {
        let mut s = WalletScorer::new();
        // Mean notional $150k, single losing cycle → still Whale (priority).
        s.feed(&wp("w-whale", 0.0, 1500.0, 100.0, NOW / DAY_NS - 1));
        s.feed(&wp("w-whale", 0.0, -1500.0, 95.0, NOW / DAY_NS));
        s.feed(&wp("w-whale", 0.0, 1500.0, 100.0, NOW / DAY_NS));
        assert_eq!(s.classify(NOW, &cfg()).get("w-whale"), Some(&Cohort::Whale));
    }

    #[test]
    fn wcg_6_smart_money_requires_every_condition() {
        // High PnL but win rate too low → Retail.
        let mut low_wr = WalletScorer::new();
        for _ in 0..4 {
            low_wr.feed(&wp("a", 0.0, 10.0, 100.0, NOW / DAY_NS - 2));
            low_wr.feed(&wp("a", 0.0, -10.0, 130.0, NOW / DAY_NS - 1)); // loss
            low_wr.feed(&wp("a", 0.0, -10.0, 200.0, NOW / DAY_NS - 1)); // huge win short
        }
        assert_eq!(low_wr.classify(NOW, &cfg()).get("a"), Some(&Cohort::Retail));

        // High win rate but fewer than min trades → Retail.
        let mut few = WalletScorer::new();
        few.feed(&wp("b", 0.0, 10.0, 100.0, NOW / DAY_NS - 1));
        few.feed(&wp("b", 0.0, -10.0, 120.0, NOW / DAY_NS));
        few.feed(&wp("b", 0.0, 10.0, 100.0, NOW / DAY_NS));
        assert_eq!(few.classify(NOW, &cfg()).get("b"), Some(&Cohort::Retail));

        // All conditions met → Smart Money.
        let mut good = WalletScorer::new();
        profitable_wallet(&mut good, "c", 4, NOW / DAY_NS);
        assert_eq!(
            good.classify(NOW, &cfg()).get("c"),
            Some(&Cohort::SmartMoney)
        );
    }

    #[test]
    fn wcg_2_classification_respects_btreetmap_order() {
        // Identical metrics, inserted in OPPOSITE orders → byte-identical
        // classification maps with ascending address keys (CONV-10).
        let build = |order: &[&str]| {
            let mut s = WalletScorer::new();
            for addr in order {
                profitable_wallet(&mut s, addr, 3, 10);
            }
            s.classify(10 * DAY_NS, &cfg())
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect::<BTreeMap<String, Cohort>>()
        };
        let a = build(&["z-addr", "m-addr", "a-addr"]);
        let b = build(&["a-addr", "z-addr", "m-addr"]);
        let (ja, jb) = (
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
        );
        assert_eq!(ja, jb);
        let keys: Vec<_> = a.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["a-addr", "m-addr", "z-addr"],
            "ascending BTreeMap order"
        );
    }

    #[test]
    fn wcg_1_10_scoring_is_deterministic_and_idempotent() {
        let build = || {
            let mut s = WalletScorer::new();
            profitable_wallet(&mut s, "w-b", 4, 10);
            s.feed(&wp("w-a", 0.0, 2000.0, 50.0, 9));
            s.feed(&wp("w-c", 0.0, 1.0, 30.0, 8));
            s
        };
        let (sa, sb) = (build(), build());
        let ca = sa.classify(10 * DAY_NS, &cfg());
        let cb = sb.classify(10 * DAY_NS, &cfg());
        // Byte-identical assignment maps (WCG-1/10) incl. BTreeMap ordering.
        let ja = serde_json::to_string(&ca).unwrap();
        let jb = serde_json::to_string(&cb).unwrap();
        assert_eq!(ja, jb);
        let keys: Vec<_> = ca.keys().copied().collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "sorted order");
    }

    #[test]
    fn wcg_8_stale_snapshot_suppresses_feature() {
        let snap: CohortSnapshot = (40 * DAY_NS, BTreeMap::from([("w".into(), Cohort::Whale)]));
        let mut fresh = CohortFeature::new(Some(snap.clone()), CohortField::WhaleRatio, 7 * DAY_NS);
        let mut stale = CohortFeature::new(Some(snap), CohortField::WhaleRatio, 7 * DAY_NS);
        // Fresh: sole whale address holds 100% of tracked notional → 1.0.
        assert_eq!(fresh.on_event(&wp("w", 0.0, 500.0, 100.0, 41)), Some(1.0));
        // 40d snapshot read at day 90 → 50d stale > 7d cap → suppressed.
        assert_eq!(stale.on_event(&wp("w", 0.0, 500.0, 100.0, 90)), None);
        // Unknown address inside a fresh snapshot → not our pair → None.
        assert_eq!(fresh.on_event(&wp("unknown", 0.0, 500.0, 100.0, 41)), None);
    }

    #[test]
    fn wcg_7_catalog_registration_and_feature_ids() {
        use crate::engine::TickFeature as _;
        // All four WCG-7 families exist with stable catalog IDs (CONV-20).
        assert_eq!(
            CohortFeature::new(None, CohortField::WhaleRatio, DAY_NS).id(),
            "cohort.whale_ratio"
        );
        for (c, want) in [
            (Cohort::SmartMoney, "cohort.net_delta.smart_money"),
            (Cohort::Whale, "cohort.net_delta.whale"),
            (Cohort::Retail, "cohort.net_delta.retail"),
            (Cohort::Dormant, "cohort.net_delta.dormant"),
        ] {
            assert_eq!(
                CohortFeature::new(None, CohortField::NetDelta(c), DAY_NS).id(),
                want
            );
        }
        assert_eq!(
            CohortFeature::with_windows(None, CohortField::SmartFlow, DAY_NS, DAY_NS).id(),
            "cohort.smart_flow.24h",
            "default smart-flow window is 24h"
        );
        assert_eq!(
            CohortFeature::new(None, CohortField::Concentration, DAY_NS).id(),
            "cohort.concentration"
        );

        // Emission semantics on a mixed snapshot {A=SmartMoney, B=Whale}.
        let snap: CohortSnapshot = (
            40 * DAY_NS,
            BTreeMap::from([
                ("A".into(), Cohort::SmartMoney),
                ("B".into(), Cohort::Whale),
            ]),
        );
        let mut flow = CohortFeature::with_windows(
            Some(snap.clone()),
            CohortField::SmartFlow,
            7 * DAY_NS,
            DAY_NS,
        );
        let mut nd = CohortFeature::new(
            Some(snap.clone()),
            CohortField::NetDelta(Cohort::SmartMoney),
            7 * DAY_NS,
        );
        let mut hhi =
            CohortFeature::new(Some(snap.clone()), CohortField::Concentration, 7 * DAY_NS);
        let mut ratio = CohortFeature::new(Some(snap), CohortField::WhaleRatio, 7 * DAY_NS);
        // Smart print +$10k@100 → smart_flow emits the signed delta; net_delta
        // (smart) emits it too; whale_ratio starts at 0 (no whales yet).
        assert_eq!(
            flow.on_event(&wp("A", 0.0, 100.0, 100.0, 41)),
            Some(10_000.0)
        );
        assert_eq!(nd.on_event(&wp("A", 0.0, 100.0, 100.0, 41)), Some(10_000.0));
        assert_eq!(
            hhi.on_event(&wp("A", 0.0, 100.0, 100.0, 41)),
            Some(1.0),
            "one cohort holds everything"
        );
        assert_eq!(ratio.on_event(&wp("A", 0.0, 100.0, 100.0, 41)), Some(0.0));
        // Whale print of EQUAL notional doubles the book → ratio exactly 0.5;
        // HHI over two equal cohorts = 0.5² + 0.5² = 0.5.
        assert!((ratio.on_event(&wp("B", 0.0, -100.0, 100.0, 41)).unwrap() - 0.5).abs() < 1e-9);
        assert!((hhi.on_event(&wp("B", 0.0, -100.0, 100.0, 41)).unwrap() - 0.5).abs() < 1e-9);
        // A retail (ungraded-here) print moves NOTHING in these aggregates —
        // unknown addresses are excluded, never guessed.
        assert_eq!(flow.on_event(&wp("zz", 0.0, 900.0, 100.0, 41)), None);
    }

    #[test]
    fn wcg_10_idempotent_scoring_same_journal_hash() {
        let dir = std::env::temp_dir().join(format!("wcg10-{}", std::process::id()));
        let feed = |s: &mut WalletScorer| {
            profitable_wallet(s, "w-b", 4, 10);
            s.feed(&wp("w-a", 0.0, 2000.0, 50.0, 9));
        };
        let run = |p: &std::path::Path| {
            let mut s = WalletScorer::new();
            feed(&mut s);
            let cohorts: BTreeMap<String, Cohort> = s
                .classify(10 * DAY_NS, &cfg())
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect();
            let metrics: BTreeMap<String, WalletMetrics> = s
                .metrics()
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.clone()))
                .collect();
            save_snapshot(p, &(42 * DAY_NS, cohorts.clone())).unwrap();
            journal_changes(&dir.join("changes.jsonl"), None, &cohorts, &metrics, 123).unwrap();
            std::fs::read(p).unwrap()
        };
        let (f1, f2) = (
            dir.join("r1").join("cohorts.json"),
            dir.join("r2").join("cohorts.json"),
        );
        assert_eq!(run(&f1), run(&f2), "re-run must be byte-identical (WCG-10)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    proptest::proptest! {
        /// Random sweep around every threshold: classification ALWAYS yields a
        /// valid cohort AND equals the independent conjunction predicate
        /// (dormancy > whale-notional > smart-money conjunction > retail).
        #[test]
        fn wcg_11_proptest_cohort_boundary_classification(
            pnl in -2_000.0f64..2_000.0,
            trades in 0u64..9,
            wins in 0u64..9,
            avg_size in 0.0f64..250_000.0,
            days_since_activity in 0i64..45,
            sharpe_seed in -3.0f64..3.0,
        ) {
            let now = NOW;
            let m = WalletMetrics {
                realized_pnl: pnl,
                trade_count: trades,
                wins: wins.min(trades),
                avg_position_size: avg_size,
                last_activity_ts_ns: now - days_since_activity * DAY_NS,
                // Deterministic two-point cycle pattern: variance > 0 whenever
                // ≥3 cycles, so sharpe_approx is defined and scales with pnl.
                cycle_pnls: vec![sharpe_seed, -sharpe_seed, sharpe_seed / 2.0],
                position_samples: trades.max(1),
            };
            let cfg = cfg(); // min_smart_trades=3, min_smart_sharpe=0.5
            let got = classify_one(&m, now, &cfg);
            prop_assert!(matches!(
                got,
                Cohort::SmartMoney | Cohort::Whale | Cohort::Retail | Cohort::Dormant
            ));
            let dormant =
                m.last_activity_ts_ns < now - cfg.dormant_threshold_ns;
            let expected = if dormant {
                Cohort::Dormant
            } else if m.avg_position_size.is_finite()
                && m.avg_position_size >= cfg.whale_threshold_usd
            {
                Cohort::Whale
            } else if m.realized_pnl.is_finite()
                && m.realized_pnl > 0.0
                && m.trade_count >= cfg.min_smart_trades
                && m.win_rate() >= cfg.win_rate_min
                && m.sharpe_approx().is_some_and(|s| s >= cfg.min_smart_sharpe)
            {
                Cohort::SmartMoney
            } else {
                Cohort::Retail
            };
            prop_assert_eq!(got, expected, "pnl={} trades={} wins={} size={} idle={}", pnl, trades, wins, avg_size, days_since_activity);
        }
    }

    #[test]
    fn wcg_9_journal_records_only_changed_addresses() {
        let dir = std::env::temp_dir().join(format!("wcg9-{}", std::process::id()));
        let jp = dir.join("cohort_changes.jsonl");
        let old = BTreeMap::from([("a".into(), Cohort::Retail), ("b".into(), Cohort::Whale)]);
        let new = BTreeMap::from([
            ("a".into(), Cohort::SmartMoney),
            ("b".into(), Cohort::Whale),   // unchanged
            ("c".into(), Cohort::Dormant), // joined
        ]);
        let metrics = BTreeMap::from([(
            "a".into(),
            WalletMetrics {
                realized_pnl: 400.0,
                trade_count: 4,
                wins: 4,
                avg_position_size: 1_000.0,
                last_activity_ts_ns: 123,
                cycle_pnls: vec![100.0],
                position_samples: 5,
            },
        )]);
        let n = journal_changes(&jp, Some(&old), &new, &metrics, 123).unwrap();
        assert_eq!(n, 2, "a changed, c joined; b unchanged");
        let text = std::fs::read_to_string(&jp).unwrap();
        assert!(text.contains("\"old_cohort\":\"retail\""));
        assert!(text.contains("\"new_cohort\":\"smart_money\""));
        assert!(text.contains("\"new_cohort\":\"dormant\""));
        assert!(!text.contains("\"address\":\"b\""));
        assert!(
            text.contains("\"score_breakdown\":{"),
            "WCG-9 breakdown present"
        );
        assert!(text.contains("\"realized_pnl\":400.0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wcg_9_snapshot_atomic_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wcg9s-{}", std::process::id()));
        let p = dir.join("cohorts").join("2026-08-23.json");
        let snap: CohortSnapshot = (
            42 * DAY_NS,
            BTreeMap::from([("x".into(), Cohort::SmartMoney)]),
        );
        save_snapshot(&p, &snap).unwrap();
        assert!(!p.with_extension("tmp").exists(), "tmp renamed away");
        let loaded = load_snapshot(&p).unwrap();
        assert_eq!(loaded.0, snap.0);
        assert_eq!(loaded.1.get("x"), Some(&Cohort::SmartMoney));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Boundary sweep: every (pnl, trades, win_rate) combination around the
    /// thresholds yields a valid cohort, Smart Money exactly when ALL gates
    /// pass (exhaustive stand-in for the proptest at these axes).
    #[test]
    fn wcg_boundary_classification_is_exact_conjunction() {
        for pnl in [f64::NAN, -1.0, 1.0] {
            for trades in [2u64, 3, 6] {
                for wins in [0u64, 3] {
                    for sharpe_ok in [false, true] {
                        let m = WalletMetrics {
                            realized_pnl: pnl,
                            trade_count: trades,
                            wins,
                            avg_position_size: 10.0,
                            last_activity_ts_ns: NOW,
                            cycle_pnls: vec![1.0; 4],
                            position_samples: 4,
                        };
                        let mut m = m;
                        if !sharpe_ok {
                            m.cycle_pnls.clear(); // <3 cycles → no Sharpe
                        }
                        let c = classify_one(&m, NOW, &cfg());
                        let smart = pnl > 0.0
                            && trades >= 3
                            && (wins as f64 / trades as f64) >= 0.45
                            && m.sharpe_approx().is_some_and(|s| s >= 0.5);
                        assert_eq!(
                            c == Cohort::SmartMoney,
                            smart,
                            "pnl={pnl} trades={trades} wins={wins}"
                        );
                    }
                }
            }
        }
    }
}
