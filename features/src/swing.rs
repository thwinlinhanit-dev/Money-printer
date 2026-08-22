//! Swing-horizon bar-aggregated features (spec 035, SWG-2; spec 036, SLQ).
//! Regime and structure computed from BARS only — never order book or
//! trade-tape (SWG-2 MUST NOT require tick inputs). Pure + deterministic
//! (PD-3).

use crate::bar::Bar;
use crate::engine::BarFeature;
use std::collections::VecDeque;

/// Realized vol as a fraction of price over `closes`. None when <2 samples or
/// non-finite (fail-closed, CONV-8). `sqrt_bars_per_year` annualizes.
pub fn realized_vol(closes: &[f64], sqrt_bars_per_year: f64) -> Option<f64> {
    if closes.len() < 2 {
        return None;
    }
    let mean = closes.iter().sum::<f64>() / closes.len() as f64;
    let var = closes.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / (closes.len() - 1) as f64;
    let sigma = var.sqrt();
    if !sigma.is_finite() || sigma <= f64::EPSILON {
        return None;
    }
    Some(sigma / mean.abs().max(f64::EPSILON) * sqrt_bars_per_year)
}

/// Signed HTF trend: net close move over `lookback` bars / mean abs bar move.
/// |x|>1 = drift>noise; sign = direction. None until `lookback` bars.
pub fn trend_strength(closes: &[f64], lookback: usize) -> Option<f64> {
    if closes.len() < lookback + 1 || lookback == 0 {
        return None;
    }
    let n = closes.len();
    let net = closes[n - 1] - closes[n - lookback - 1];
    let mut abs_sum = 0.0_f64;
    for w in closes[n - lookback..].windows(2) {
        abs_sum += (w[1] - w[0]).abs();
    }
    if !abs_sum.is_finite() || abs_sum <= f64::EPSILON {
        return None;
    }
    Some(net / abs_sum)
}

/// Value area from closed bars — OHLCV TPO approximation (spec 035 SWG-2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueArea {
    pub poc_price: f64,
    pub va_high: f64,
    pub va_low: f64,
    pub volume_frac: f64,
}

/// Full close-bucketed profile level set (spec 036 SLQ-V): POC / value-area
/// boundaries plus HVN and LVN price levels. All outputs are OHLCV-approximate
/// by construction (`approx: true` family) and never blended with tick-derived
/// volume.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeLevels {
    pub poc_price: f64,
    pub va_high: f64,
    pub va_low: f64,
    /// Local volume maxima above `hvn_frac` × POC volume (bucket mid prices,
    /// ascending).
    pub hvn: Vec<f64>,
    /// Local minima below `lvn_frac` × POC volume (bucket mid prices,
    /// ascending).
    pub lvn: Vec<f64>,
}

/// Shared close-bucket accumulation result: occupied buckets (ascending index
/// list), POC bucket, and the 70% value-area span as inclusive indices.
struct ProfileCore {
    buckets: std::collections::BTreeMap<i64, f64>,
    idxs: Vec<i64>,
    poc: i64,
    va_lo: i64,
    va_hi: i64,
}

fn profile_core(bars: &[Bar], bucket_size: f64) -> Option<ProfileCore> {
    if bars.is_empty() || !bucket_size.is_finite() || bucket_size <= 0.0 {
        return None;
    }
    let mut buckets: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
    let mut total_vol = 0.0_f64;
    for b in bars {
        if !b.vol.is_finite() || b.vol <= 0.0 {
            continue;
        }
        let idx = (b.close / bucket_size).floor();
        if !idx.is_finite() {
            continue;
        }
        *buckets.entry(idx as i64).or_insert(0.0) += b.vol;
        total_vol += b.vol;
    }
    if buckets.is_empty() || !total_vol.is_finite() || total_vol <= 0.0 {
        return None;
    }
    let idxs: Vec<i64> = buckets.keys().copied().collect();
    let mut poc = idxs[0];
    let mut poc_vol = f64::NEG_INFINITY;
    for &i in &idxs {
        if buckets[&i] > poc_vol {
            poc_vol = buckets[&i];
            poc = i;
        }
    }
    let target = total_vol * 0.70;
    let mut lo = poc;
    let mut hi = poc;
    let mut covered = buckets[&poc];
    while covered < target {
        let lo_vol = if lo > idxs[0] {
            buckets.get(&(lo - 1)).copied()
        } else {
            None
        };
        let hi_vol = if hi < idxs[idxs.len() - 1] {
            buckets.get(&(hi + 1)).copied()
        } else {
            None
        };
        match (lo_vol, hi_vol) {
            (Some(l), Some(h)) if h > l => {
                hi += 1;
                covered += h;
            }
            (Some(l), _) => {
                lo -= 1;
                covered += l;
            }
            (None, Some(h)) => {
                hi += 1;
                covered += h;
            }
            _ => break,
        }
    }
    Some(ProfileCore {
        buckets,
        idxs,
        poc,
        va_lo: lo,
        va_hi: hi,
    })
}

/// Bucket mid price for index `i`.
fn bucket_mid(i: i64, bucket_size: f64) -> f64 {
    i as f64 * bucket_size + bucket_size / 2.0
}

/// POC + value area over closed bars (spec 035 SWG-2 behavior unchanged;
/// now computed through [`volume_levels`]' shared core). None for no bars /
/// degenerate input (CONV-8).
pub fn value_area(bars: &[Bar], bucket_size: f64) -> Option<ValueArea> {
    let core = profile_core(bars, bucket_size)?;
    let covered: f64 = (core.va_lo..=core.va_hi)
        .filter_map(|i| core.buckets.get(&i))
        .sum();
    let total: f64 = core.buckets.values().sum();
    if !covered.is_finite() || total <= 0.0 {
        return None;
    }
    Some(ValueArea {
        poc_price: bucket_mid(core.poc, bucket_size),
        va_low: core.va_lo as f64 * bucket_size,
        va_high: core.va_hi as f64 * bucket_size + bucket_size,
        volume_frac: covered / total,
    })
}

/// HVN/LVN level set over closed bars (spec 036 SLQ-V). Local extrema are
/// judged per occupied bucket against its adjacent OCCUPIED neighbors; a
/// missing neighbor at the series edge counts as satisfied. Ties exclude a
/// bucket from both sets (deterministic).
pub fn volume_levels(
    bars: &[Bar],
    bucket_size: f64,
    hvn_frac: f64,
    lvn_frac: f64,
) -> Option<VolumeLevels> {
    let core = profile_core(bars, bucket_size)?;
    if !hvn_frac.is_finite() || !lvn_frac.is_finite() {
        return None;
    }
    let poc_vol = core.buckets[&core.poc];
    let hvn_cut = poc_vol * hvn_frac;
    let lvn_cut = poc_vol * lvn_frac;
    let mut hvn = Vec::new();
    let mut lvn = Vec::new();
    for (pos, &i) in core.idxs.iter().enumerate() {
        let v = core.buckets[&i];
        if !v.is_finite() {
            continue;
        }
        let prev = if pos > 0 {
            core.buckets.get(&core.idxs[pos - 1]).copied()
        } else {
            None
        };
        let next = if pos + 1 < core.idxs.len() {
            core.buckets.get(&core.idxs[pos + 1]).copied()
        } else {
            None
        };
        let lt_prev = prev.is_none_or(|p| v < p);
        let lt_next = next.is_none_or(|n| v < n);
        let gt_prev = prev.is_none_or(|p| v > p);
        let gt_next = next.is_none_or(|n| v > n);
        if gt_prev && gt_next && v > hvn_cut {
            hvn.push(bucket_mid(i, bucket_size));
        }
        if lt_prev && lt_next && v < lvn_cut {
            lvn.push(bucket_mid(i, bucket_size));
        }
    }
    Some(VolumeLevels {
        poc_price: bucket_mid(core.poc, bucket_size),
        va_high: core.va_hi as f64 * bucket_size + bucket_size,
        va_low: core.va_lo as f64 * bucket_size,
        hvn,
        lvn,
    })
}

/// Rolling n-bar VWAP band — a structural swing level. None until n pushed.
#[derive(Debug, Clone)]
pub struct RollingVwap {
    pub n_bars: usize,
    buf: VecDeque<(f64, f64)>, // (price·vol, vol)
}

impl RollingVwap {
    pub fn new(n_bars: usize) -> Self {
        Self {
            n_bars: n_bars.max(1),
            buf: VecDeque::new(),
        }
    }
    pub fn push(&mut self, bar: &Bar) {
        if bar.vol.is_finite() && bar.vol > 0.0 {
            self.buf.push_back((bar.vwap * bar.vol, bar.vol));
            while self.buf.len() > self.n_bars {
                self.buf.pop_front();
            }
        }
    }
    /// (vwap, deviation of `last_close` from vwap in %).
    pub fn current(&self, last_close: f64) -> Option<(f64, f64)> {
        if self.buf.is_empty() {
            return None;
        }
        let (pv_sum, vol_sum) = self
            .buf
            .iter()
            .fold((0.0_f64, 0.0_f64), |(a, b), (pv, v)| (a + pv, b + v));
        if !vol_sum.is_finite() || vol_sum <= 0.0 {
            return None;
        }
        let vwap = pv_sum / vol_sum;
        let dev = if vwap.abs() > f64::EPSILON {
            (last_close - vwap) / vwap * 100.0
        } else {
            0.0
        };
        Some((vwap, dev))
    }
    /// True once at least one valid bar has been pushed.
    pub fn is_warm(&self) -> bool {
        !self.buf.is_empty()
    }
}

// ---- BarFeature adapters (spec 035 SWG-2): register the pure swing
// computations as engine bar features. Bar-only: no order book, no trade-tape
// (SWG-2 MUST NOT require tick inputs). Each keeps its own bounded window and
// emits once per closed bar, honoring FEA-3 warmup.

/// `swing.realized_vol.{window}` — annualized realized vol over the trailing
/// `window` closes (spec 035 SWG-2 §4 realized-vol regime).
pub struct SwingRealizedVol {
    window: usize,
    sqrt_bars_per_year: f64,
    closes: VecDeque<f64>,
}
impl SwingRealizedVol {
    pub fn new(window: usize, sqrt_bars_per_year: f64) -> Self {
        Self {
            window: window.max(2),
            sqrt_bars_per_year,
            closes: VecDeque::new(),
        }
    }
}
impl BarFeature for SwingRealizedVol {
    fn id(&self) -> String {
        format!("swing.realized_vol.{}", self.window)
    }
    fn warm(&self) -> bool {
        self.closes.len() >= 2
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.closes.push_back(bar.close);
        while self.closes.len() > self.window {
            self.closes.pop_front();
        }
        let closes: Vec<f64> = self.closes.iter().copied().collect();
        realized_vol(&closes, self.sqrt_bars_per_year)
    }
}

/// `swing.trend_strength.{lookback}` — signed HTF trend over the trailing
/// `lookback` bars (spec 035 SWG-2 §4 HTF trend/regime state).
pub struct SwingTrendStrength {
    lookback: usize,
    closes: VecDeque<f64>,
}
impl SwingTrendStrength {
    pub fn new(lookback: usize) -> Self {
        Self {
            lookback: lookback.max(1),
            closes: VecDeque::new(),
        }
    }
}
impl BarFeature for SwingTrendStrength {
    fn id(&self) -> String {
        format!("swing.trend_strength.{}", self.lookback)
    }
    fn warm(&self) -> bool {
        self.closes.len() > self.lookback
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.closes.push_back(bar.close);
        while self.closes.len() > self.lookback + 1 {
            self.closes.pop_front();
        }
        let closes: Vec<f64> = self.closes.iter().copied().collect();
        trend_strength(&closes, self.lookback)
    }
}

/// Which value-area output a [`SwingValueArea`] instance emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueAreaField {
    /// POC price (`va.poc_price`).
    Poc,
    /// Value-area high (`va.va_high`).
    High,
    /// Value-area low (`va.va_low`).
    Low,
}

/// `swing.value_area.{poc|high|low}.{window}` — one value-area level over the
/// trailing `window` closed bars (spec 035 SWG-2 §4 value-area context from
/// bars). Register one instance per level.
pub struct SwingValueArea {
    window: usize,
    bucket_size: f64,
    field: ValueAreaField,
    bars: VecDeque<Bar>,
}
impl SwingValueArea {
    pub fn new(window: usize, bucket_size: f64, field: ValueAreaField) -> Self {
        Self {
            window: window.max(1),
            bucket_size,
            field,
            bars: VecDeque::new(),
        }
    }
}
impl BarFeature for SwingValueArea {
    fn id(&self) -> String {
        let level = match self.field {
            ValueAreaField::Poc => "poc",
            ValueAreaField::High => "high",
            ValueAreaField::Low => "low",
        };
        format!("swing.value_area.{level}.{}", self.window)
    }
    fn warm(&self) -> bool {
        !self.bars.is_empty()
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.bars.push_back(*bar);
        while self.bars.len() > self.window {
            self.bars.pop_front();
        }
        let bars: Vec<Bar> = self.bars.iter().copied().collect();
        let va = value_area(&bars, self.bucket_size)?;
        Some(match self.field {
            ValueAreaField::Poc => va.poc_price,
            ValueAreaField::High => va.va_high,
            ValueAreaField::Low => va.va_low,
        })
    }
}

/// `swing.rolling_vwap.{n}` — rolling n-bar VWAP structural level (spec 035
/// SWG-2 §4 structural levels).
pub struct SwingRollingVwap {
    inner: RollingVwap,
}
impl SwingRollingVwap {
    pub fn new(n_bars: usize) -> Self {
        Self {
            inner: RollingVwap::new(n_bars),
        }
    }
}
impl BarFeature for SwingRollingVwap {
    fn id(&self) -> String {
        format!("swing.rolling_vwap.{}", self.inner.n_bars)
    }
    fn warm(&self) -> bool {
        self.inner.is_warm()
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.inner.push(bar);
        self.inner.current(bar.close).map(|(vwap, _)| vwap)
    }
}
// ---- Spec 036 (SLQ): liquidity-structure features --------------------------
// Still BAR-ONLY per SWG-2: ATR (SLQ-A), HVN/LVN nearest levels (SLQ-V), and
// the compressed-range sweep detector (SLQ-R). Pure helpers are free
// functions; stateful windows live in thin BarFeature adapters that honor
// FEA-3 warmup and emit at most once per closed bar. Degenerate input fails
// closed (CONV-8): non-finite prices/vols yield no emission, never NaN.

/// True range of `bar` given the previous close (`None` on series start →
/// high − low). None when the bar carries non-finite or negative spans.
fn true_range(bar: &Bar, prev_close: Option<f64>) -> Option<f64> {
    let hl = bar.high - bar.low;
    if !bar.high.is_finite() || !bar.low.is_finite() || !hl.is_finite() || hl < 0.0 {
        return None;
    }
    let tr = match prev_close {
        Some(pc) if pc.is_finite() => hl.max((bar.high - pc).abs()).max((bar.low - pc).abs()),
        _ => hl,
    };
    if !tr.is_finite() || tr < 0.0 {
        None
    } else {
        Some(tr)
    }
}

/// Wilder ATR over the trailing `n` closed bars of `bars` (spec 036 SLQ-A),
/// absolute price units. The oldest bar of the window has no prev close →
/// TR = high − low (spec §2.1). Requires `bars.len() >= n`; zero-volatility
/// and degenerate windows fail closed.
pub fn atr(bars: &[Bar], n: usize) -> Option<f64> {
    if n == 0 || bars.len() < n {
        return None;
    }
    let win = &bars[bars.len() - n..];
    let mut sum = 0.0_f64;
    for (i, b) in win.iter().enumerate() {
        let prev = if i == 0 { None } else { Some(win[i - 1].close) };
        sum += true_range(b, prev)?;
    }
    let a = sum / n as f64;
    if !a.is_finite() || a <= 0.0 {
        return None;
    }
    Some(a)
}

/// Compressed-range boundaries over the LAST `range_n` bars of `bars`
/// (spec 036 SLQ-R): active iff mean true-range of the window <
/// `compress_frac` × `baseline_atr`. Returns (range_high, range_low).
/// Baseline ATR must come from the bars BEFORE the window (no
/// self-reference); callers enforce that via [`atr`] on a prior slice.
pub fn compressed_range(
    bars: &[Bar],
    range_n: usize,
    baseline_atr: f64,
    compress_frac: f64,
) -> Option<(f64, f64)> {
    if range_n == 0 || bars.len() < range_n {
        return None;
    }
    if !baseline_atr.is_finite() || baseline_atr <= 0.0 {
        return None;
    }
    if !compress_frac.is_finite() || compress_frac <= 0.0 {
        return None;
    }
    let win = &bars[bars.len() - range_n..];
    let mut sum_tr = 0.0_f64;
    for (i, b) in win.iter().enumerate() {
        let prev = if i == 0 { None } else { Some(win[i - 1].close) };
        sum_tr += true_range(b, prev)?;
    }
    let mean_tr = sum_tr / range_n as f64;
    if !mean_tr.is_finite() || mean_tr >= compress_frac * baseline_atr {
        return None;
    }
    let hi = win.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
    let lo = win.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    if !hi.is_finite() || !lo.is_finite() || hi < lo {
        return None;
    }
    Some((hi, lo))
}

/// Which boundary a sweep violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepSide {
    /// Wick below the range low (support swept).
    Low,
    /// Wick above the range high (resistance swept).
    High,
}

/// A classified liquidity sweep (spec 036 SLQ-R): the wick extreme, the range
/// it violated, and the sweep-bar volume ratio vs the range-window average.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepEvent {
    pub side: SweepSide,
    pub extreme: f64,
    pub range_high: f64,
    pub range_low: f64,
    pub volume_ratio: f64,
}

/// Classify ONE bar as a sweep of `[range_low, range_high]`: wick beyond the
/// boundary by ≥ `atr_mult × baseline_atr` AND sweep-bar volume ≥
/// `vol_mult × window_avg_vol` (wick-volume filter). Low takes precedence if
/// both boundaries were violated on one bar (cannot happen under compression
/// in practice; deterministic tie-break regardless).
pub fn sweep_of(
    bar: &Bar,
    range_high: f64,
    range_low: f64,
    baseline_atr: f64,
    atr_mult: f64,
    window_avg_vol: f64,
    vol_mult: f64,
) -> Option<SweepEvent> {
    if !baseline_atr.is_finite()
        || baseline_atr <= 0.0
        || !window_avg_vol.is_finite()
        || window_avg_vol <= 0.0
        || !bar.low.is_finite()
        || !bar.high.is_finite()
        || !bar.vol.is_finite()
        || bar.vol <= 0.0
    {
        return None;
    }
    let volume_ratio = bar.vol / window_avg_vol;
    if !(atr_mult.is_finite() && vol_mult.is_finite()) || volume_ratio < vol_mult {
        return None;
    }
    if bar.low < range_low - atr_mult * baseline_atr {
        return Some(SweepEvent {
            side: SweepSide::Low,
            extreme: bar.low,
            range_high,
            range_low,
            volume_ratio,
        });
    }
    if bar.high > range_high + atr_mult * baseline_atr {
        return Some(SweepEvent {
            side: SweepSide::High,
            extreme: bar.high,
            range_high,
            range_low,
            volume_ratio,
        });
    }
    None
}

/// Shared detector engine behind the `swing.sweep.*` adapters (SLQ-R). Holds
/// `range_n + atr_n` bars; classifies sweeps; waits up to `reclaim_z`
/// SUBSEQUENT closes back inside the frozen sweep-time range for
/// confirmation. A sweep whose own close lands back inside the range
/// confirms at offset 0 (the classic rejection-wick candle). A newer sweep
/// replaces an unconfirmed pending one. Exactly one pending sweep at a time.
#[derive(Debug)]
pub(crate) struct SweepDetector {
    range_n: usize,
    compress_frac: f64,
    atr_n: usize,
    atr_mult: f64,
    reclaim_z: u32,
    vol_mult: f64,
    stop_buffer_atr: f64,
    bars: VecDeque<Bar>,
    pending: Option<PendingSweep>,
}

#[derive(Debug)]
struct PendingSweep {
    event: SweepEvent,
    bars_since: u32,
}

impl SweepDetector {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        range_n: usize,
        compress_frac: f64,
        atr_n: usize,
        atr_mult: f64,
        reclaim_z: u32,
        vol_mult: f64,
        stop_buffer_atr: f64,
    ) -> Self {
        Self {
            range_n: range_n.max(1),
            compress_frac,
            atr_n: atr_n.max(1),
            atr_mult,
            reclaim_z,
            vol_mult,
            stop_buffer_atr,
            bars: VecDeque::new(),
            pending: None,
        }
    }

    fn baseline_atr(&self) -> Option<f64> {
        // ATR over the atr_n bars BEFORE the trailing range_n window.
        let split = self.bars.len().saturating_sub(self.range_n);
        let base: Vec<Bar> = self.bars.iter().take(split).copied().collect();
        atr(&base, self.atr_n)
    }

    fn range(&self, baseline: f64) -> Option<(f64, f64)> {
        let win: Vec<Bar> = self
            .bars
            .iter()
            .skip(self.bars.len() - self.range_n)
            .copied()
            .collect();
        compressed_range(&win, self.range_n, baseline, self.compress_frac)
    }

    fn window_avg_vol(&self) -> Option<f64> {
        let win = self.bars.iter().skip(self.bars.len() - self.range_n);
        let mut sum = 0.0_f64;
        for b in win {
            if !b.vol.is_finite() {
                return None;
            }
            sum += b.vol;
        }
        let avg = sum / self.range_n as f64;
        if !avg.is_finite() || avg <= 0.0 {
            None
        } else {
            Some(avg)
        }
    }

    /// Feed one closed bar; returns `(event, invalidation_stop)` exactly when
    /// a sweep-reclaim CONFIRMS on this bar. Stop = extreme ∓
    /// `stop_buffer_atr` × current baseline ATR (confirmation-time volatility).
    ///
    /// Classification always runs against the PRE-BAR history: the range
    /// boundaries, baseline ATR, and volume average are frozen from the bars
    /// before the candidate, so a sweep wick can never absorb itself into the
    /// range it violates.
    pub(crate) fn push(&mut self, bar: &Bar) -> Option<(SweepEvent, f64)> {
        if !bar.close.is_finite() {
            return None;
        }
        // History snapshot (before this bar joins the windows).
        let warm = self.bars.len() >= self.range_n + self.atr_n;
        let history = if warm {
            let base = self.baseline_atr();
            let rng = base.and_then(|b| self.range(b));
            let avg = self.window_avg_vol();
            Some((base, rng, avg))
        } else {
            None
        };

        // Absorb the bar.
        self.bars.push_back(*bar);
        while self.bars.len() > self.range_n + self.atr_n {
            self.bars.pop_front();
        }

        let mut emitted: Option<(SweepEvent, f64)> = None;

        // 1. Reclaim check against an existing pending sweep (this close,
        // judged on the sweep bar's FROZEN range stored in the pending).
        if self.pending.is_some() {
            let ev = self.pending.as_ref().unwrap().event.clone();
            let inside = bar.close >= ev.range_low && bar.close <= ev.range_high;
            if inside {
                if let Some(base) = history.as_ref().and_then(|(b, _, _)| *b) {
                    let stop = match ev.side {
                        SweepSide::Low => ev.extreme - self.stop_buffer_atr * base,
                        SweepSide::High => ev.extreme + self.stop_buffer_atr * base,
                    };
                    if stop.is_finite() {
                        emitted = Some((ev, stop));
                    }
                }
                self.pending = None;
            } else {
                let p = self.pending.as_mut().unwrap();
                p.bars_since += 1;
                if p.bars_since > self.reclaim_z {
                    self.pending = None;
                }
            }
        }

        // 2. Fresh sweep classification on this bar vs the historical range
        // (replaces unconfirmed pending; same-bar close-back-inside confirms
        // at offset 0).
        if let Some((Some(base), Some((rh, rl)), Some(avg_vol))) = history {
            if let Some(ev) = sweep_of(bar, rh, rl, base, self.atr_mult, avg_vol, self.vol_mult) {
                let inside = bar.close >= rl && bar.close <= rh;
                if inside && emitted.is_none() {
                    let stop = match ev.side {
                        SweepSide::Low => ev.extreme - self.stop_buffer_atr * base,
                        SweepSide::High => ev.extreme + self.stop_buffer_atr * base,
                    };
                    if stop.is_finite() {
                        emitted = Some((ev, stop));
                    }
                } else {
                    self.pending = Some(PendingSweep {
                        event: SweepEvent {
                            side: ev.side,
                            extreme: ev.extreme,
                            range_high: rh,
                            range_low: rl,
                            volume_ratio: ev.volume_ratio,
                        },
                        bars_since: 0,
                    });
                }
            }
        }
        emitted
    }
}

/// Which value a sweep adapter emits on the confirming bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepField {
    /// The sweep wick extreme (`swing.sweep.{low|high}.{n}`).
    Extreme,
    /// The invalidation price (`swing.sweep.{low|high}.stop.{n}`).
    Stop,
}

/// `swing.sweep.{low|high}[.stop].{range_n}` — sweep-reclaim event pair
/// (spec 036 SLQ-R). Four registered instances (side × field) each run an
/// identical detector filtered to their side; all emit ONLY on the confirming
/// bar, so a consumer sees a self-contained (extreme, stop) pair per side
/// with no cross-feature ordering assumptions. The stop travels WITH the
/// event: it is feature-family config (`sweep_stop_buffer_atr`), never a
/// strategy parameter.
pub struct SwingSweep {
    detector: SweepDetector,
    side_filter: Option<SweepSide>,
    field: SweepField,
}

impl SwingSweep {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        range_n: usize,
        compress_frac: f64,
        atr_n: usize,
        atr_mult: f64,
        reclaim_z: u32,
        vol_mult: f64,
        stop_buffer_atr: f64,
        side_filter: Option<SweepSide>,
        field: SweepField,
    ) -> Self {
        Self {
            detector: SweepDetector::new(
                range_n,
                compress_frac,
                atr_n,
                atr_mult,
                reclaim_z,
                vol_mult,
                stop_buffer_atr,
            ),
            side_filter,
            field,
        }
    }
}

impl BarFeature for SwingSweep {
    fn id(&self) -> String {
        let side = match self.side_filter {
            Some(SweepSide::Low) => "low",
            Some(SweepSide::High) => "high",
            None => "any",
        };
        match self.field {
            SweepField::Extreme => format!("swing.sweep.{}.{}", side, self.detector.range_n),
            SweepField::Stop => format!("swing.sweep.{}.stop.{}", side, self.detector.range_n),
        }
    }
    fn warm(&self) -> bool {
        self.detector.bars.len() >= self.detector.range_n + self.detector.atr_n
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        let (ev, stop) = self.detector.push(bar)?;
        if let Some(want) = self.side_filter {
            if ev.side != want {
                return None;
            }
        }
        Some(match self.field {
            SweepField::Extreme => ev.extreme,
            SweepField::Stop => stop,
        })
    }
}

/// Which range boundary a [`SwingRange`] emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeField {
    High,
    Low,
}

/// `swing.range.{high|low}.{range_n}` — live compressed-range boundaries
/// (SLQ-R); emitted every closed bar while a compressed range exists, absent
/// otherwise. Baseline ATR comes from the bars before the window (no
/// self-reference), identical to the sweep adapters' definition.
pub struct SwingRange {
    bars: VecDeque<Bar>,
    range_n: usize,
    compress_frac: f64,
    atr_n: usize,
    field: RangeField,
}

impl SwingRange {
    pub fn new(range_n: usize, compress_frac: f64, atr_n: usize, field: RangeField) -> Self {
        Self {
            bars: VecDeque::new(),
            range_n: range_n.max(1),
            compress_frac,
            atr_n: atr_n.max(1),
            field,
        }
    }
}

impl BarFeature for SwingRange {
    fn id(&self) -> String {
        let f = match self.field {
            RangeField::High => "high",
            RangeField::Low => "low",
        };
        format!("swing.range.{}.{}", f, self.range_n)
    }
    fn warm(&self) -> bool {
        self.bars.len() >= self.range_n + self.atr_n
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.bars.push_back(*bar);
        while self.bars.len() > self.range_n + self.atr_n {
            self.bars.pop_front();
        }
        if self.bars.len() < self.range_n + self.atr_n {
            return None;
        }
        let split = self.bars.len() - self.range_n;
        let base: Vec<Bar> = self.bars.iter().take(split).copied().collect();
        let baseline = atr(&base, self.atr_n)?;
        let win: Vec<Bar> = self.bars.iter().skip(split).copied().collect();
        let (rh, rl) = compressed_range(&win, self.range_n, baseline, self.compress_frac)?;
        Some(match self.field {
            RangeField::High => rh,
            RangeField::Low => rl,
        })
    }
}

/// `swing.atr.{n}` — Wilder ATR(n) in absolute price units (SLQ-A).
pub struct SwingAtr {
    n: usize,
    bars: VecDeque<Bar>,
}

impl SwingAtr {
    pub fn new(n: usize) -> Self {
        Self {
            n: n.max(1),
            bars: VecDeque::new(),
        }
    }
}

impl BarFeature for SwingAtr {
    fn id(&self) -> String {
        format!("swing.atr.{}", self.n)
    }
    fn warm(&self) -> bool {
        self.bars.len() >= self.n
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.bars.push_back(*bar);
        while self.bars.len() > self.n {
            self.bars.pop_front();
        }
        let win: Vec<Bar> = self.bars.iter().copied().collect();
        atr(&win, self.n)
    }
}

/// `swing.close` — the closed bar's close price (SLQ-S). Trivial passthrough,
/// but it is the ONLY way a bar-only strategy sees the entry-timeframe close:
/// every exit rule in spec 036 §3 (hard invalidation, T1, trail) is
/// close-evaluated by design.
pub struct SwingClose;
impl BarFeature for SwingClose {
    fn id(&self) -> String {
        "swing.close".into()
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        if bar.close.is_finite() {
            Some(bar.close)
        } else {
            None
        }
    }
}

/// Nearest profile level vs the latest close (SLQ-V target selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelKind {
    Hvn,
    Lvn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelSide {
    Above,
    Below,
}

/// `swing.profile.{hvn|lvn}_{above|below}.{window}` — the closest level of
/// the requested kind strictly above/below the latest close; absent when no
/// such level exists in the window's profile.
pub struct SwingNearestLevel {
    window: usize,
    bucket_size: f64,
    hvn_frac: f64,
    lvn_frac: f64,
    kind: LevelKind,
    side: LevelSide,
    bars: VecDeque<Bar>,
}

impl SwingNearestLevel {
    pub fn new(
        window: usize,
        bucket_size: f64,
        hvn_frac: f64,
        lvn_frac: f64,
        kind: LevelKind,
        side: LevelSide,
    ) -> Self {
        Self {
            window: window.max(1),
            bucket_size,
            hvn_frac,
            lvn_frac,
            kind,
            side,
            bars: VecDeque::new(),
        }
    }
}

impl BarFeature for SwingNearestLevel {
    fn id(&self) -> String {
        let k = match self.kind {
            LevelKind::Hvn => "hvn",
            LevelKind::Lvn => "lvn",
        };
        let s = match self.side {
            LevelSide::Above => "above",
            LevelSide::Below => "below",
        };
        format!("swing.profile.{}_{}.{}", k, s, self.window)
    }
    fn warm(&self) -> bool {
        self.bars.len() >= self.window
    }
    fn on_bar(&mut self, bar: &Bar) -> Option<f64> {
        self.bars.push_back(*bar);
        while self.bars.len() > self.window {
            self.bars.pop_front();
        }
        if self.bars.len() < self.window {
            return None;
        }
        let win: Vec<Bar> = self.bars.iter().copied().collect();
        let levels = volume_levels(&win, self.bucket_size, self.hvn_frac, self.lvn_frac)?;
        let candidates: &[f64] = match self.kind {
            LevelKind::Hvn => &levels.hvn,
            LevelKind::Lvn => &levels.lvn,
        };
        let close = bar.close;
        let best = candidates
            .iter()
            .filter(|&&l| match self.side {
                LevelSide::Above => l > close,
                LevelSide::Below => l < close,
            })
            .copied()
            .reduce(|a, b| match self.side {
                LevelSide::Above => {
                    if b < a {
                        b
                    } else {
                        a
                    }
                }
                LevelSide::Below => {
                    if b > a {
                        b
                    } else {
                        a
                    }
                }
            })?;
        if best.is_finite() {
            Some(best)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::Bar;

    fn bar(close: f64, vol: f64) -> Bar {
        Bar {
            open: close,
            high: close,
            low: close,
            close,
            vol,
            buy_vol: vol,
            sell_vol: 0.0,
            vwap: close,
            n_trades: 1,
            first_ts_ns: 0,
            last_ts_ns: 0,
            close_ts_ns: 0,
        }
    }

    #[test]
    fn swg_2_realized_vol_positive_finite_and_cold() {
        let v = realized_vol(&[100.0, 101.0, 99.0, 102.0], 1.0).unwrap();
        assert!(v.is_finite() && v > 0.0);
        assert!(
            realized_vol(&[1.0, 1.0], 1.0).is_none(),
            "flat series → zero variance → fail-closed None"
        );
        assert!(realized_vol(&[1.0], 1.0).is_none());
    }

    #[test]
    fn swg_2_trend_strength_signs_and_warmup() {
        let up: Vec<f64> = (0..=20).map(|i| 100.0 + i as f64).collect();
        assert!(trend_strength(&up, 10).unwrap() > 1.0);
        let down: Vec<f64> = (0..=20).map(|i| 200.0 - i as f64).collect();
        assert!(trend_strength(&down, 10).unwrap() < -1.0);
        assert!(trend_strength(&[100.0, 101.0], 5).is_none());
    }

    #[test]
    fn swg_2_value_area_covers_around_poc() {
        let mut bars = Vec::new();
        for _ in 0..5 {
            bars.push(bar(200.0, 1.0));
            bars.push(bar(100.0, 10.0));
        }
        let va = value_area(&bars, 10.0).unwrap();
        assert!(
            (va.poc_price - 100.0).abs() < 6.0,
            "poc ~100 got {}",
            va.poc_price
        );
        assert!(va.va_low <= va.poc_price && va.poc_price <= va.va_high);
        assert!(va.volume_frac >= 0.5 && va.volume_frac <= 1.0);
        assert!(value_area(&[], 10.0).is_none());
    }

    #[test]
    fn swg_2_rolling_vwap_band() {
        let mut rv = RollingVwap::new(3);
        for c in [100.0_f64, 110.0, 120.0] {
            rv.push(&bar(c, 1.0));
        }
        let (vwap, dev) = rv.current(120.0).unwrap();
        assert!(vwap == 110.0);
        assert!((dev - (120.0 - 110.0) / 110.0 * 100.0).abs() < 1e-6);
        rv.push(&bar(200.0, 1.0));
        let (v2, _) = rv.current(200.0).unwrap();
        assert!((v2 - (110.0 + 120.0 + 200.0) / 3.0).abs() < 1e-9);
    }

    #[test]
    fn swg_2_bar_feature_realized_vol_warmup_and_emit() {
        let mut f = SwingRealizedVol::new(4, 1.0);
        assert!(!f.warm());
        assert!(f.on_bar(&bar(100.0, 1.0)).is_none(), "needs >= 2 closes");
        assert!(f.on_bar(&bar(102.0, 1.0)).is_some(), "warm at 2 closes");
        assert_eq!(f.id(), "swing.realized_vol.4");
    }

    #[test]
    fn swg_2_bar_feature_trend_strength_warmup_and_sign() {
        let mut f = SwingTrendStrength::new(3);
        assert!(!f.warm());
        for _ in 0..3 {
            assert!(f.on_bar(&bar(100.0, 1.0)).is_none(), "needs lookback+1=4");
        }
        assert!(!f.warm(), "3 closes < lookback+1, still cold");
        // 4th close warms it and yields a finite trend.
        let v = f.on_bar(&bar(101.0, 1.0)).unwrap();
        assert!(v.is_finite());
        assert!(f.warm());
        assert_eq!(f.id(), "swing.trend_strength.3");
    }

    #[test]
    fn swg_2_bar_feature_value_area_levels_emit() {
        let mut poc = SwingValueArea::new(5, 10.0, ValueAreaField::Poc);
        let mut hi = SwingValueArea::new(5, 10.0, ValueAreaField::High);
        let mut lo = SwingValueArea::new(5, 10.0, ValueAreaField::Low);
        // 5 bars near 100 → POC ~100, va band around it; high >= low.
        for _ in 0..5 {
            assert!(poc.on_bar(&bar(100.0, 1.0)).is_some());
            assert!(hi.on_bar(&bar(100.0, 1.0)).is_some());
            assert!(lo.on_bar(&bar(100.0, 1.0)).is_some());
        }
        let (p, h, l) = (
            poc.on_bar(&bar(100.0, 1.0)).unwrap(),
            hi.on_bar(&bar(100.0, 1.0)).unwrap(),
            lo.on_bar(&bar(100.0, 1.0)).unwrap(),
        );
        assert!(l <= p && p <= h, "poc inside va band: {l} <= {p} <= {h}");
        assert_eq!(poc.id(), "swing.value_area.poc.5");
        assert_eq!(hi.id(), "swing.value_area.high.5");
        assert_eq!(lo.id(), "swing.value_area.low.5");
    }

    #[test]
    fn swg_2_bar_feature_rolling_vwap_emits_window_vwap() {
        let mut f = SwingRollingVwap::new(3);
        assert!(!f.warm());
        for c in [100.0_f64, 110.0, 120.0] {
            f.on_bar(&bar(c, 1.0));
        }
        assert!(f.warm());
        let v = f.on_bar(&bar(130.0, 1.0)).unwrap();
        assert!(
            (v - (110.0 + 120.0 + 130.0) / 3.0).abs() < 1e-9,
            "oldest drops"
        );
        assert_eq!(f.id(), "swing.rolling_vwap.3");
    }

    // ---- spec 036 (SLQ) -----------------------------------------------------

    /// Full OHLC bar helper for SLQ tests.
    fn hbar(open: f64, high: f64, low: f64, close: f64, vol: f64) -> Bar {
        Bar {
            open,
            high,
            low,
            close,
            vol,
            buy_vol: vol / 2.0,
            sell_vol: vol / 2.0,
            vwap: close,
            n_trades: 1,
            first_ts_ns: 0,
            last_ts_ns: 0,
            close_ts_ns: 0,
        }
    }

    #[test]
    fn slq_atr_matches_hand_computation_and_fails_closed() {
        // Two bars, no prev close on the first: TRs are (10), then
        // max(4, |105-95|, |95-99|) = 10 → ATR = 10.
        let bars = [
            hbar(90., 100., 90., 95., 1.),
            hbar(95., 105., 99., 101., 1.),
        ];
        let a = atr(&bars, 2).unwrap();
        assert!((a - 10.0).abs() < 1e-9);
        assert!(atr(&bars, 3).is_none(), "needs >= n bars");
        assert!(atr(&bars, 0).is_none());
        // Flat series → zero ATR → fail-closed.
        let flat = [hbar(100., 100., 100., 100., 1.); 5];
        assert!(atr(&flat, 5).is_none());
    }

    #[test]
    fn slq_volume_levels_hvn_lvn_thresholds() {
        // Buckets of width 10 around closes 100 (vol 10), 120 (vol 8),
        // 140 (vol 9) — POC=100 (vol 10): HVN cut 7 (0.7×10): bucket 140
        // (vol 9 > 7, local max vs 8) is HVN; 120 (8 > 7 but neighbor 9/10
        // bigger... it's a local MIN vs both? prev=10, next=9 → not min).
        // LVN cut 3 (0.3×10): nothing below 3 unless we add a dip bucket.
        let mut bars = Vec::new();
        for _ in 0..2 {
            bars.push(hbar(100., 101., 99., 100., 5.)); // bucket idx(close/10)=10 → vol 10
        }
        bars.push(hbar(120., 121., 119., 120., 8.)); // idx 12 → vol 8
        bars.push(hbar(140., 141., 139., 140., 9.)); // idx 14 → vol 9
        bars.push(hbar(160., 161., 159., 160., 1.)); // idx 16 → vol 1 (LVN edge)
        let vl = volume_levels(&bars, 10.0, 0.7, 0.3).unwrap();
        // POC itself is a high-volume node: edge-local max above cut → HVN.
        assert_eq!(vl.hvn, vec![105.0, 145.0]);
        assert!(vl.lvn.contains(&165.0), "edge bucket 16 (vol 1 < 3)");
        assert!(
            vl.va_low <= vl.poc_price && vl.poc_price <= vl.va_high,
            "va band brackets poc"
        );
        assert!(volume_levels(&bars, 0.0, 0.7, 0.3).is_none(), "bad bucket");
    }

    fn compressed_window() -> Vec<Bar> {
        // 25 wide bars (TR ~ 24) then 20 tight bars around 100 (TR ~ 2):
        // baseline ATR ≈ 24 over the pre-window slice, compression holds
        // (2 << 0.6×24).
        let mut bars = Vec::new();
        for i in 0..25 {
            let p = 200.0 + (i % 5) as f64;
            bars.push(hbar(p - 12., p + 12., p - 12., p + 1., 10.));
        }
        for i in 0..20 {
            let p = 100.0 + (i % 3) as f64;
            bars.push(hbar(p - 1., p + 1., p - 1., p, 10.));
        }
        bars
    }

    #[test]
    fn slq_compressed_range_gates_on_mean_tr_vs_baseline() {
        let bars = compressed_window();
        let split = bars.len() - 20;
        let base = atr(&bars[..split], 20).unwrap();
        let win = &bars[split..];
        let (rh, rl) = compressed_range(win, 20, base, 0.6).unwrap();
        assert!(rh >= 102.0 && rl <= 99.0, "bounds bracket the window");
        // Same window against a TINY baseline must refuse (not compressed).
        assert!(compressed_range(win, 20, 1.0, 0.6).is_none());
        assert!(
            compressed_range(win, 30, base, 0.6).is_none(),
            "short window"
        );
    }

    #[test]
    fn slq_sweep_of_requires_wick_beyond_and_volume() {
        let bars = compressed_window();
        let split = bars.len() - 20;
        let base = atr(&bars[..split], 20).unwrap();
        let win = &bars[split..];
        let (rh, rl) = compressed_range(win, 20, base, 0.6).unwrap();
        let avg_vol = 10.0;
        // Wick below range_low by 0.1×ATR+ with 2× volume → low sweep.
        let sweep_bar = hbar(rl, 100., rl - 0.2 * base - 1., rl, 25.);
        let ev = sweep_of(&sweep_bar, rh, rl, base, 0.1, avg_vol, 1.5).unwrap();
        assert_eq!(ev.side, SweepSide::Low);
        assert_eq!(ev.extreme, sweep_bar.low);
        assert!((ev.volume_ratio - 2.5).abs() < 1e-9);
        // Same wick at LOW volume → filtered out.
        let quiet = hbar(rl, 100., rl - 0.2 * base - 1., rl, 10.);
        assert!(sweep_of(&quiet, rh, rl, base, 0.1, avg_vol, 1.5).is_none());
        // Big volume but wick INSIDE the range → not a sweep.
        let inside = hbar(rh, rh, rl + 1., rl + 1., 25.);
        assert!(sweep_of(&inside, rh, rl, base, 0.1, avg_vol, 1.5).is_none());
        // High-side mirror fires with extreme = high.
        let hi_sweep = hbar(rh, rh + 0.2 * base + 1., rl, rl + 1., 25.);
        let ev2 = sweep_of(&hi_sweep, rh, rl, base, 0.1, avg_vol, 1.5).unwrap();
        assert_eq!(ev2.side, SweepSide::High);
        assert_eq!(ev2.extreme, hi_sweep.high);
    }

    const DET_RANGE_N: usize = 5;
    const DET_ATR_N: usize = 5;

    fn detector() -> SweepDetector {
        SweepDetector::new(DET_RANGE_N, 0.6, DET_ATR_N, 0.1, 2, 1.5, 0.5)
    }

    /// Wide bars (baseline ATR ~ 24) then tight bars (range ~ 2).
    fn feed_baseline(d: &mut SweepDetector) {
        for i in 0..DET_ATR_N {
            let p = 200.0 + i as f64;
            d.push(&hbar(p - 12., p + 12., p - 12., p, 10.));
        }
        for i in 0..DET_RANGE_N {
            let p = 100.0 + (i % 3) as f64;
            d.push(&hbar(p - 1., p + 1., p - 1., p, 10.));
        }
    }

    #[test]
    fn slq_detector_confirms_reclaim_within_z_bars() {
        let mut d = detector();
        feed_baseline(&mut d);
        // Range roughly [98, 103]; baseline ATR ~ 24 → need wick below
        // ~95.x and volume ≥ 15.
        assert!(d.push(&hbar(100., 101., 99., 100., 10.)).is_none());
        let sweep = hbar(100., 101., 92., 96., 30.);
        assert!(
            d.push(&sweep).is_none(),
            "no same-bar confirm: close outside"
        );
        // Next close back inside → confirmed with stop = extreme − 0.5×ATR
        // (ATR measured at confirmation time; the sliding window mixes
        // transition bars, so the ATR is larger than the pure-wide 24).
        let (ev, stop) = d
            .push(&hbar(97., 101., 96., 100., 10.))
            .expect("reclaim confirms");
        assert_eq!(ev.side, SweepSide::Low);
        assert!((ev.extreme - 92.).abs() < 1e-9);
        assert!(
            stop < ev.extreme && stop > ev.extreme - 20.0,
            "low-sweep stop sits below the extreme: {stop}"
        );
    }

    #[test]
    #[test]
    fn slq_detector_offset_zero_confirm_and_expiry_after_z() {
        let mut d = detector();
        feed_baseline(&mut d);
        // Rejection wick: sweeps low AND closes inside → instant confirm.
        let (ev, _) = d
            .push(&hbar(100., 101., 93., 100., 30.))
            .expect("offset-0 confirm");
        assert_eq!(ev.side, SweepSide::Low);

        // Expiry: a pending sweep dies once MORE than z=2 SUBSEQUENT closes
        // land back outside without confirming.
        let mut d2 = detector();
        feed_baseline(&mut d2);
        assert!(d2.push(&hbar(100., 101., 92., 96., 30.)).is_none()); // pending set
        assert!(d2.push(&hbar(97., 101., 96., 96.5, 10.)).is_none()); // outside #1
        assert!(d2.push(&hbar(97., 101., 96., 96.5, 10.)).is_none()); // outside #2
        assert!(d2.push(&hbar(97., 101., 96., 96.5, 10.)).is_none()); // outside #3 → expired
        let late = d2.push(&hbar(97., 101., 96., 100., 10.));
        assert!(late.is_none(), "pending expired after > z outside closes");
    }

    #[test]
    fn slq_detector_new_sweep_replaces_pending() {
        let mut d = detector();
        feed_baseline(&mut d);
        assert!(d.push(&hbar(100., 101., 94., 96., 30.)).is_none()); // pending #1
                                                                     // A DEEPER low sweep before confirmation replaces the pending one.
                                                                     // The first wick is part of the rolling window now, so the effective
                                                                     // boundary is ~94: the new wick must clear it by >= Y*ATR AND close
                                                                     // outside the updated range (else it would offset-0-confirm).
        assert!(d.push(&hbar(98., 101., 89., 93., 30.)).is_none());
        // Reclaim confirms against the REPLACED extreme (89, not 94).
        let (ev, _) = d.push(&hbar(97., 101., 96., 100., 10.)).expect("confirm");
        assert!((ev.extreme - 89.).abs() < 1e-9, "replaced extreme");
    }

    #[test]
    fn slq_sweep_adapter_ids_side_filter_and_emit_only_on_confirm() {
        let mut low_ext = SwingSweep::new(
            DET_RANGE_N,
            0.6,
            DET_ATR_N,
            0.1,
            2,
            1.5,
            0.5,
            Some(SweepSide::Low),
            SweepField::Extreme,
        );
        let mut high_ext = SwingSweep::new(
            DET_RANGE_N,
            0.6,
            DET_ATR_N,
            0.1,
            2,
            1.5,
            0.5,
            Some(SweepSide::High),
            SweepField::Extreme,
        );
        assert_eq!(low_ext.id(), "swing.sweep.low.5");
        assert_eq!(high_ext.id(), "swing.sweep.high.5");
        let mut stop = SwingSweep::new(
            DET_RANGE_N,
            0.6,
            DET_ATR_N,
            0.1,
            2,
            1.5,
            0.5,
            Some(SweepSide::Low),
            SweepField::Stop,
        );
        assert_eq!(stop.id(), "swing.sweep.low.stop.5");

        for i in 0..(DET_RANGE_N + DET_ATR_N) {
            let b = if i < DET_ATR_N {
                let p = 200.0 + i as f64;
                hbar(p - 12., p + 12., p - 12., p, 10.)
            } else {
                hbar(99., 101., 99., 100., 10.)
            };
            assert!(low_ext.on_bar(&b).is_none(), "cold until warm");
            assert!(high_ext.on_bar(&b).is_none());
            assert!(stop.on_bar(&b).is_none());
        }
        // Low sweep + offset-0 confirm → low adapters fire, high stays silent.
        let sweep = hbar(100., 101., 93., 100., 30.);
        let ext = low_ext.on_bar(&sweep).unwrap();
        assert!((ext - 93.).abs() < 1e-9);
        let st = stop.on_bar(&sweep).unwrap();
        // Low-sweep invalidation sits BELOW the wick extreme; here the
        // baseline is exactly the wide ATR 24 → stop = 93 − 12 = 81.
        assert!((st - (ext - 0.5 * 24.0)).abs() < 1e-6, "stop {st}");
        assert!(high_ext.on_bar(&sweep).is_none(), "side filter");
        // A normal quiet bar emits nothing anywhere.
        let calm = hbar(100., 101., 99., 100., 10.);
        assert!(low_ext.on_bar(&calm).is_none());
        assert!(stop.on_bar(&calm).is_none());
    }

    #[test]
    fn slq_range_adapter_emits_only_when_compressed() {
        let mut rhi = SwingRange::new(DET_RANGE_N, 0.6, DET_ATR_N, RangeField::High);
        let mut rlo = SwingRange::new(DET_RANGE_N, 0.6, DET_ATR_N, RangeField::Low);
        assert_eq!(rhi.id(), "swing.range.high.5");
        assert_eq!(rlo.id(), "swing.range.low.5");
        // Wide bars: NOT compressed → silent (both adapters get the same
        // baseline history).
        for i in 0..DET_ATR_N {
            let p = 200.0 + i as f64;
            let b = hbar(p - 12., p + 12., p - 12., p, 10.);
            assert!(rhi.on_bar(&b).is_none());
            assert!(rlo.on_bar(&b).is_none());
        }
        // Tight window over the same baseline → bounds appear.
        for i in 0..DET_RANGE_N {
            let p = 100.0 + (i % 3) as f64;
            let hi = rhi.on_bar(&hbar(p - 1., p + 1., p - 1., p, 10.));
            let lo = rlo.on_bar(&hbar(p - 1., p + 1., p - 1., p, 10.));
            if i == DET_RANGE_N - 1 {
                assert!(hi.is_some() && lo.is_some(), "warm + compressed");
            } else {
                assert!(hi.is_none() && lo.is_none(), "needs full window");
            }
        }
    }

    #[test]
    fn slq_nearest_level_picks_closest_above_below() {
        let mut above = SwingNearestLevel::new(6, 10.0, 0.7, 0.3, LevelKind::Lvn, LevelSide::Above);
        let mut below = SwingNearestLevel::new(6, 10.0, 0.7, 0.3, LevelKind::Lvn, LevelSide::Below);
        assert_eq!(above.id(), "swing.profile.lvn_above.6");
        assert_eq!(below.id(), "swing.profile.lvn_below.6");
        // Volume ladder (bucket width 10): closes 100 (vol 10 total),
        // 110 (vol 2), 120 (vol 8), 130 (vol 1); POC = bucket 100.
        // LVN cut = 0.3 × 10 = 3 → buckets 110 and 130 are LVNs
        // (mids 115 and 135).
        let hist = [
            hbar(100., 101., 99., 100., 5.),
            hbar(100., 101., 99., 100., 5.),
            hbar(110., 111., 109., 110., 2.),
            hbar(120., 121., 119., 120., 8.),
            hbar(130., 131., 129., 130., 1.),
        ];
        for b in &hist {
            assert!(above.on_bar(b).is_none(), "cold until window full");
            assert!(below.on_bar(b).is_none());
        }
        // Window fills with a close back AT the POC price (adds vol 3 there;
        // POC stays bucket 100): nearest LVN strictly above 100 is 115, and
        // nothing sits strictly below → None.
        let close_at_poc = hbar(99., 101., 98., 100., 3.);
        let a = above.on_bar(&close_at_poc).unwrap();
        assert!((a - 115.).abs() < 1e-9);
        assert!(below.on_bar(&close_at_poc).is_none());

        // Fresh window whose final close is ABOVE every LVN: the closest
        // strictly-below level must be the higher one (135).
        let mut below2 =
            SwingNearestLevel::new(6, 10.0, 0.7, 0.3, LevelKind::Lvn, LevelSide::Below);
        for b in &hist {
            assert!(below2.on_bar(b).is_none());
        }
        let close_high = hbar(138., 141., 137., 140., 3.);
        // Adds bucket 140 (vol 3 < 3.9) BUT its only occupied neighbor is
        // lower (vol 1), so it is NOT a local minimum → stays excluded.
        let b = below2.on_bar(&close_high).unwrap();
        assert!((b - 135.).abs() < 1e-9);
    }

    #[test]
    fn slq_atr_adapter_warmup_and_id() {
        let mut f = SwingAtr::new(3);
        assert_eq!(f.id(), "swing.atr.3");
        assert!(!f.warm());
        assert!(f.on_bar(&hbar(99., 101., 99., 100., 1.)).is_none());
        assert!(f.on_bar(&hbar(99., 101., 99., 100., 1.)).is_none());
        // TRs: 2 (no prev close), 2, then max(5, |104−100|, |99−100|) = 5.
        let v = f.on_bar(&hbar(99., 104., 99., 100., 1.)).unwrap();
        assert!((v - 3.0).abs() < 1e-9, "(2+2+5)/3 = 3, got {v}");
        assert!(f.warm());
    }
}
