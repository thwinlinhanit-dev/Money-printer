//! Statistical harnesses (SIM-9): walk-forward, parameter-plateau, and
//! Monte-Carlo block bootstrap. These turn a single backtest number into a
//! distribution and an out-of-sample story — the difference between a strategy
//! that works and one that was curve-fit.
//!
//! Deterministic: the Monte-Carlo RNG is a seeded splitmix64 (CONV-11), so a
//! resampled DD distribution is reproducible from its seed.

use mp_core::EventEnvelope;

/// A compact, copyable snapshot of one run's metrics for cross-window tables.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricsSummary {
    pub trades: u64,
    pub expectancy: f64,
    pub stress_expectancy_2x: f64,
    pub max_drawdown: f64,
}

/// Walk-forward window sizing (rolling; defaults 90d train / 30d test / 30d
/// step per spec 005).
#[derive(Debug, Clone, Copy)]
pub struct WalkForwardParams {
    pub train_ns: i64,
    pub test_ns: i64,
    pub step_ns: i64,
}

/// One walk-forward window's out-of-sample result.
#[derive(Debug, Clone, Copy)]
pub struct WindowResult {
    pub train_start_ns: i64,
    pub test_start_ns: i64,
    pub test_end_ns: i64,
    pub oos: MetricsSummary,
}

fn slice_by_recv(events: &[EventEnvelope], start_ns: i64, end_ns: i64) -> &[EventEnvelope] {
    // Events are in global recv order (SIM-1), so a contiguous window is a
    // sub-slice found by the first/last index in `[start, end)`.
    let lo = events.partition_point(|e| e.recv_ts_ns < start_ns);
    let hi = events.partition_point(|e| e.recv_ts_ns < end_ns);
    &events[lo..hi]
}

/// Roll `(train, test)` windows across the event span, calling `run` with each
/// window's train and test slices; `run` fits on train and applies on test,
/// returning the OOS summary. The harness only slices and steps — the fit is
/// the caller's (strategy-specific) business.
pub fn walk_forward<F>(
    events: &[EventEnvelope],
    p: WalkForwardParams,
    mut run: F,
) -> Vec<WindowResult>
where
    F: FnMut(&[EventEnvelope], &[EventEnvelope]) -> MetricsSummary,
{
    let mut out = Vec::new();
    if events.is_empty() {
        return out;
    }
    let first = events[0].recv_ts_ns;
    let last = events[events.len() - 1].recv_ts_ns;
    let mut train_start = first;
    while train_start + p.train_ns + p.test_ns <= last + 1 {
        let test_start = train_start + p.train_ns;
        let test_end = test_start + p.test_ns;
        let train = slice_by_recv(events, train_start, test_start);
        let test = slice_by_recv(events, test_start, test_end);
        out.push(WindowResult {
            train_start_ns: train_start,
            test_start_ns: test_start,
            test_end_ns: test_end,
            oos: run(train, test),
        });
        train_start += p.step_ns;
    }
    out
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

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
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
            Some(b) if b == bucket => blocks.last_mut().unwrap().push(pnl),
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
    let mut state = seed;
    let mut dds: Vec<f64> = Vec::with_capacity(resamples as usize);
    for _ in 0..resamples {
        // Build a resampled path of whole blocks until we cover ≥ target trades.
        let mut equity = 0.0_f64;
        let mut peak = 0.0_f64;
        let mut max_dd = 0.0_f64;
        let mut n = 0usize;
        while n < target {
            let idx = (splitmix64(&mut state) % blocks.len() as u64) as usize;
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
        worst_max_dd: *dds.last().unwrap(),
        resamples,
    }
}
