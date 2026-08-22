//! Statistical harnesses (SIM-9): walk-forward, parameter-plateau, and
//! Monte-Carlo block bootstrap. These turn a single backtest number into a
//! distribution and an out-of-sample story — the difference between a strategy
//! that works and one that was curve-fit.
//!
//! Deterministic: the Monte-Carlo RNG is a seeded splitmix64 (CONV-11), so a
//! resampled DD distribution is reproducible from its seed.

use mp_core::{EventEnvelope, SplitMix64};
use std::collections::BTreeMap;

/// A compact, copyable snapshot of one run's metrics for cross-window tables.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MetricsSummary {
    pub trades: u64,
    pub expectancy: f64,
    pub stress_expectancy_2x: f64,
    pub max_drawdown: f64,
    /// SWG-5: annualized bar-return Sharpe (None when the run had too few bar
    /// returns to be meaningful — FEA-3 warmup).
    pub sharpe: Option<f64>,
}

impl MetricsSummary {
    /// SWG-5: Deflated Sharpe for the given `bars_per_year` and `n_trials` —
    /// the multiple-testing-adjusted version of `sharpe`. None when the raw
    /// Sharpe is unavailable (warmup).
    pub fn deflated_sharpe(&self, _bars_per_year: f64, n_trials: u64) -> Option<f64> {
        let sr = self.sharpe?;
        if n_trials <= 1 {
            return Some(sr);
        }
        let euler_gamma = 0.577_215_664_901_532_9;
        let inv_n = 1.0 / n_trials as f64;
        let p1 = crate::metrics::probit(1.0 - inv_n);
        let p2 = crate::metrics::probit(1.0 - inv_n / std::f64::consts::E);
        let expected_max = (1.0 - euler_gamma) * p1 + euler_gamma * p2;
        Some(sr - expected_max)
    }
}

/// Walk-forward window sizing (rolling; defaults 90d train / 30d test / 30d
/// step per spec 005).
#[derive(Debug, Clone, Copy)]
pub struct WalkForwardParams {
    pub train_ns: i64,
    pub test_ns: i64,
    pub step_ns: i64,
    /// SWG-5 purged splits: gap inserted between the train slice and the test
    /// slice (embargo). The train slice ends at `train_start + train_ns`, the
    /// test slice begins at `train_start + train_ns + embargo_ns`. This
    /// prevents look-ahead leakage from overlapping labels (spec 005).
    pub embargo_ns: i64,
}

/// One walk-forward window's out-of-sample result.
#[derive(Debug, Clone, Copy)]
pub struct WindowResult {
    pub train_start_ns: i64,
    pub test_start_ns: i64,
    pub test_end_ns: i64,
    pub oos: MetricsSummary,
    /// True when no grid combo met the walk-forward's min-trades bar on the
    /// train slice (SIM-9 integrity): the argmax had nothing eligible to pick,
    /// so the window carries NO selection and its OOS is meaningless. A
    /// VACUOUS window must never read as a pass — the falsification gate
    /// grades only non-vacuous windows.
    pub vacuous: bool,
    /// True when the selected combo's OOS run itself failed (a `run_checked`
    /// error). An ERROR window must never read as a SELECTED pass — it is
    /// distinct from VACUOUS and must fail the window (audit M9).
    pub error: bool,
}

/// Generate the cartesian product of all param grid values.
/// Each element is a param_name→value map for one grid point.
pub fn param_combinations(grid: &BTreeMap<String, Vec<f64>>) -> Vec<BTreeMap<String, f64>> {
    let mut out = Vec::new();
    if grid.is_empty() {
        out.push(BTreeMap::new());
        return out;
    }
    let keys: Vec<&String> = grid.keys().collect();
    fn recurse(
        keys: &[&String],
        grid: &BTreeMap<String, Vec<f64>>,
        idx: usize,
        cur: &mut BTreeMap<String, f64>,
        out: &mut Vec<BTreeMap<String, f64>>,
    ) {
        if idx == keys.len() {
            out.push(cur.clone());
            return;
        }
        for v in &grid[keys[idx]] {
            cur.insert(keys[idx].clone(), *v);
            recurse(keys, grid, idx + 1, cur, out);
            cur.remove(keys[idx]);
        }
    }
    recurse(&keys, grid, 0, &mut BTreeMap::new(), &mut out);
    out
}

/// Slice events by reception-time range [start_ns, end_ns).
pub fn slice_by_recv(events: &[EventEnvelope], start_ns: i64, end_ns: i64) -> &[EventEnvelope] {
    // Events are in global recv order (SIM-1), so a contiguous window is a
    // sub-slice found by the first/last index in `[start, end)`.
    let lo = events.partition_point(|e| e.recv_ts_ns < start_ns);
    let hi = events.partition_point(|e| e.recv_ts_ns < end_ns);
    &events[lo..hi]
}

/// Roll `(train, test)` windows across the event span, calling `run` with each
/// window's boundaries and train/test slices; `run` fits on train and applies
/// on test, returning the OOS summary. The harness only slices and steps — the
/// fit is the caller's (strategy-specific) business.
///
/// SWG-5 purged splits: the test slice starts at `train_start + train_ns +
/// embargo_ns`, so the `embargo_ns` gap between the train slice's end and the
/// test slice's start is NOT in either slice — it is purged from the fit
/// entirely, blocking leakage from labels that straddle the boundary.
pub fn walk_forward<F>(
    events: &[EventEnvelope],
    p: WalkForwardParams,
    mut run: F,
) -> Vec<WindowResult>
where
    F: FnMut(i64, i64, i64, &[EventEnvelope], &[EventEnvelope]) -> MetricsSummary,
{
    let mut out = Vec::new();
    if events.is_empty() {
        return out;
    }
    let first = events[0].recv_ts_ns;
    let last = events[events.len() - 1].recv_ts_ns;
    let embargo = p.embargo_ns.max(0);
    let mut train_start = first;
    while train_start + p.train_ns + embargo + p.test_ns <= last + 1 {
        let train_end = train_start + p.train_ns;
        let test_start = train_end + embargo;
        let test_end = test_start + p.test_ns;
        let train = slice_by_recv(events, train_start, train_end);
        let test = slice_by_recv(events, test_start, test_end);
        out.push(WindowResult {
            train_start_ns: train_start,
            test_start_ns: test_start,
            test_end_ns: test_end,
            oos: run(train_start, test_start, test_end, train, test),
            vacuous: false,
            error: false,
        });
        train_start += p.step_ns;
    }
    out
}

/// Pick the grid combo with the best in-sample expectancy among those that
/// trade at least `min_trades` times (SIM-9 integrity). Combos that error or
/// under-trade are ineligible; returns `(Some(params), exp)` for the best
/// eligible combo or `(None, NEG_INFINITY)` when NOTHING is eligible — the
/// caller must mark the window VACUOUS in that case, never report a pass.
/// This is what keeps a 0-trade combo scoring 0.0 from beating a combo that
/// trades and loses (the degeneracy that made the first real-data
/// walk-forward read as a false pass).
pub fn pick_best_eligible(
    combos: &[BTreeMap<String, f64>],
    min_trades: u64,
    mut score: impl FnMut(&BTreeMap<String, f64>) -> Option<MetricsSummary>,
) -> (Option<BTreeMap<String, f64>>, f64) {
    let mut best_exp = f64::NEG_INFINITY;
    let mut best_params = None;
    for combo in combos {
        if let Some(s) = score(combo) {
            if s.trades >= min_trades && s.expectancy > best_exp {
                best_exp = s.expectancy;
                best_params = Some(combo.clone());
            }
        }
    }
    (best_params, best_exp)
}

/// Parameter-plateau check (curve-fit detector). Given the base expectancy and
/// `(pct_delta, expectancy)` points for parameter perturbations, the strategy
/// FAILS the plateau if any point within ±30% flips the sign of expectancy
/// versus the base — a real edge is a plateau, not a spike.
pub fn plateau_ok(base_expectancy: f64, points: &[(f64, f64)]) -> bool {
    let base_sign = base_expectancy > 0.0;
    for &(delta, exp) in points {
        if delta.abs() <= 0.30 + 1e-9 && (exp > 0.0) != base_sign {
            return false;
        }
    }
    true
}

/// Monte-Carlo block-bootstrap result (SIM-9). `p95_max_dd` is the sizing
/// input `p95(maxDD)` (RSK-5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct McResult {
    pub p50_max_dd: f64,
    pub p95_max_dd: f64,
    pub worst_max_dd: f64,
    pub resamples: u32,
}

/// Block-bootstrap the trade P&L sequence (block = `block_ns`, default 1 day):
/// resample whole daily blocks with replacement to a path of ≥ the original
/// trade count, walk the equity curve, record its max drawdown, repeat
/// `resamples` times, and report the DD distribution. Seeded (CONV-11).
pub fn monte_carlo(
    trade_pnls: &[(i64, f64)],
    resamples: u32,
    seed: u64,
    block_ns: i64,
) -> McResult {
    // Group trades into contiguous daily blocks (preserving intra-block order).
    let mut blocks: Vec<Vec<f64>> = Vec::new();
    let mut cur_bucket: Option<i64> = None;
    for &(ts, pnl) in trade_pnls {
        let bucket = ts.div_euclid(block_ns.max(1));
        match cur_bucket {
            Some(b) if b == bucket => {
                if let Some(blk) = blocks.last_mut() {
                    blk.push(pnl);
                }
            }
            _ => {
                cur_bucket = Some(bucket);
                blocks.push(vec![pnl]);
            }
        }
    }
    if blocks.is_empty() {
        return McResult {
            p50_max_dd: 0.0,
            p95_max_dd: 0.0,
            worst_max_dd: 0.0,
            resamples,
        };
    }
    let target = trade_pnls.len();
    let mut rng = SplitMix64::new(seed);
    let mut dds: Vec<f64> = Vec::with_capacity(resamples as usize);
    for _ in 0..resamples {
        // Build a resampled path of whole blocks until we cover ≥ target trades.
        let mut equity = 0.0_f64;
        let mut peak = 0.0_f64;
        let mut max_dd = 0.0_f64;
        let mut n = 0usize;
        while n < target {
            let idx = rng.below(blocks.len() as u64) as usize;
            for &pnl in &blocks[idx] {
                equity += pnl;
                peak = peak.max(equity);
                max_dd = max_dd.max(peak - equity);
                n += 1;
            }
        }
        dds.push(max_dd);
    }
    dds.sort_by(|a, b| a.total_cmp(b));
    let pct = |q: f64| -> f64 {
        let idx = ((q * (dds.len() as f64 - 1.0)).round() as usize).min(dds.len() - 1);
        dds[idx]
    };
    McResult {
        p50_max_dd: pct(0.50),
        p95_max_dd: pct(0.95),
        // blocks is non-empty here (early-returned above), so dds has ≥ 1
        // element; the unwrap_or keeps it panic-free regardless.
        worst_max_dd: dds.last().copied().unwrap_or(0.0),
        resamples,
    }
}
