//! Read-time analytics transforms over the corpus (spec 003 §Analytics) —
//! the Cryexc/OpenMarket compute-on-read pattern: raw points are stored once
//! (raw logs → cold Parquet), and research views (footprint bars, block-
//! bucketed order flow, OI-weighted funding) are DERIVED on demand instead of
//! pre-baked. Pure functions of `EventEnvelope` slices: no I/O, no wall clock,
//! deterministic (PD-3).
//!
//! Alignment discipline matches OpenMarket: every time bucketing uses
//! floor division `ts.div_euclid(interval) * interval` so candle-based views
//! line up across queries.

use mp_core::book::BookMirror;
use mp_core::{EventEnvelope, MarketEvent, Side, Venue};
use serde::Serialize;
use std::collections::BTreeMap;

/// One interval bar with the order-flow split (footprint totals).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FootprintBar {
    /// Interval floor (`ts.div_euclid(interval) * interval`).
    pub bucket_ts_ns: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Σ(price·qty)/Σqty over the bar; falls back to `close` on an empty bar.
    pub vwap: f64,
    /// Aggressor-buy base volume (Σqty).
    pub buy_vol: f64,
    /// Aggressor-sell base volume (Σqty).
    pub sell_vol: f64,
    pub n_trades: u64,
}

#[derive(Default)]
struct BarAcc {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    pv: f64,
    qty: f64,
    buy: f64,
    sell: f64,
    n: u64,
}

/// Aggregate trades into fixed `interval_ns` bars (floor-division bucketing).
/// Only `Trade` events participate; non-trade events are ignored. Input need
/// not be sorted (bars are keyed by bucket), but a sorted input yields bars in
/// time order with no re-sorting cost.
pub fn footprint_bars(events: &[EventEnvelope], interval_ns: i64) -> Vec<FootprintBar> {
    let interval = interval_ns.max(1);
    let mut bars: BTreeMap<i64, BarAcc> = BTreeMap::new();
    for ev in events {
        let Some((price, qty, side, _, _)) = ev.body.trade_view() else {
            continue;
        };
        let bucket = ev.recv_ts_ns.div_euclid(interval) * interval;
        let acc = bars.entry(bucket).or_default();
        if acc.n == 0 {
            acc.open = price;
            acc.high = price;
            acc.low = price;
        }
        acc.high = acc.high.max(price);
        acc.low = acc.low.min(price);
        acc.close = price;
        acc.pv += price * qty;
        acc.qty += qty;
        match side {
            Side::Buy => acc.buy += qty,
            Side::Sell => acc.sell += qty,
        }
        acc.n += 1;
    }
    bars.into_iter()
        .map(|(bucket, a)| FootprintBar {
            bucket_ts_ns: bucket,
            open: a.open,
            high: a.high,
            low: a.low,
            close: a.close,
            vwap: if a.qty > 0.0 { a.pv / a.qty } else { a.close },
            buy_vol: a.buy,
            sell_vol: a.sell,
            n_trades: a.n,
        })
        .collect()
}

/// One (interval × price-bucket) order-flow row — the footprint grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FootprintBucketRow {
    pub bucket_ts_ns: i64,
    /// Block-bucket price: `floor(price / bucket_usd) * bucket_usd`.
    pub price: f64,
    pub buy_vol: f64,
    pub sell_vol: f64,
    pub n_trades: u64,
}

/// Block-bucket trades into `(interval × price-bucket)` rows (OpenMarket's
/// blockSize/maxDepth heatmap aggregation, Cryexc's footprint per level).
/// `bucket_usd` is the price-block size in quote units; rows are sorted by
/// `(bucket_ts_ns, price)`.
pub fn footprint_buckets(
    events: &[EventEnvelope],
    interval_ns: i64,
    bucket_usd: f64,
) -> Vec<FootprintBucketRow> {
    let interval = interval_ns.max(1);
    let block = bucket_usd.max(f64::MIN_POSITIVE);
    let mut rows: BTreeMap<(i64, i64), (f64, f64, u64)> = BTreeMap::new();
    for ev in events {
        let Some((price, qty, side, _, _)) = ev.body.trade_view() else {
            continue;
        };
        let bucket = ev.recv_ts_ns.div_euclid(interval) * interval;
        let px_bucket = (price / block).floor() as i64;
        let row = rows.entry((bucket, px_bucket)).or_insert((0.0, 0.0, 0));
        match side {
            Side::Buy => row.0 += qty,
            Side::Sell => row.1 += qty,
        }
        row.2 += 1;
    }
    rows.into_iter()
        .map(|((bucket, px), (buy, sell, n))| FootprintBucketRow {
            bucket_ts_ns: bucket,
            price: px as f64 * block,
            buy_vol: buy,
            sell_vol: sell,
            n_trades: n,
        })
        .collect()
}

/// One sampled order-book ladder — the DOM view (terminal Slice 1). `bids` and
/// `asks` are the top `levels` resting entries, best first (highest bid, lowest
/// ask). `stale` mirrors [`BookMirror::is_stale`]: the mirror refuses to serve
/// a book with a sequence hole (EVT-9) — a ladder labeled `stale: true` must
/// render as "no trusted book", never as a silent stale picture.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DomSnapshot {
    /// Nominal sample time: the bucket boundary the book state was captured at.
    pub ts_ns: i64,
    pub stale: bool,
    /// `(price, qty)` per level, best first; empty when `stale`.
    pub bids: Vec<(f64, f64)>,
    /// `(price, qty)` per level, best first; empty when `stale`.
    pub asks: Vec<(f64, f64)>,
}

/// Reconstruct the book and sample the top-N ladder at fixed intervals —
/// [`analytics`]'s third view. Book events BEFORE `from_ts_ns` are applied so
/// the book is warm at the first sample (the ladder the viewer wants is *as of*
/// the scrubbed time, not a book that starts cold there). Sampling semantics:
/// a boundary strictly before an event's time settles with the PRE-apply state
/// (the ladder as of the last event at or before the boundary), the boundary
/// exactly at an event's time settles with that event applied, and trailing
/// boundaries after the last event repeat the final state. Capped at
/// `max_samples` (returned early). Pure, deterministic, no wall clock (PD-3).
pub fn dom_series(
    events: &[EventEnvelope],
    from_ts_ns: i64,
    to_ts_ns: i64,
    every_ns: i64,
    levels: usize,
    max_samples: usize,
) -> Vec<DomSnapshot> {
    let every = every_ns.max(1_000_000); // 1 ms floor
    let levels = levels.max(1).min(50);
    let max_samples = max_samples.max(1);
    if from_ts_ns > to_ts_ns {
        return Vec::new();
    }
    let mut book = BookMirror::new();
    let mut out: Vec<DomSnapshot> = Vec::new();
    let mut next = from_ts_ns;
    for ev in events {
        let t = ev.recv_ts_ns;
        if t >= from_ts_ns {
            while next < t && next <= to_ts_ns {
                push_dom_sample(&mut out, &book, next, levels);
                next += every;
                if out.len() >= max_samples {
                    return out;
                }
            }
        }
        let _ = book.apply(&ev.body);
        if t >= from_ts_ns && next == t && next <= to_ts_ns {
            push_dom_sample(&mut out, &book, t, levels);
            next += every;
            if out.len() >= max_samples {
                return out;
            }
        }
    }
    // Trailing region: no events left — repeat the final book on each
    // remaining boundary (doc promise: "same state, capped at max_samples").
    while next <= to_ts_ns {
        push_dom_sample(&mut out, &book, next, levels);
        next += every;
        if out.len() >= max_samples {
            return out;
        }
    }
    out
}

/// Push one ladder sample; `stale` books emit empty sides (EVT-9: a hole
/// renders as "no trusted book", never a silent stale picture).
fn push_dom_sample(out: &mut Vec<DomSnapshot>, book: &BookMirror, ts: i64, levels: usize) {
    if book.is_stale() {
        out.push(DomSnapshot {
            ts_ns: ts,
            stale: true,
            bids: Vec::new(),
            asks: Vec::new(),
        });
    } else {
        out.push(DomSnapshot {
            ts_ns: ts,
            stale: false,
            bids: book.bids().take(levels).collect(),
            asks: book.asks().take(levels).collect(),
        });
    }
}

/// One venue's contribution to an OI-weighted funding point.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OiwaMember {
    pub venue: String,
    /// Funding rate in the interval (the LAST rate seen in it).
    pub rate: f64,
    /// OI weight as-of the interval end — USD notional when the venue reports
    /// it, else contract count (`oi_unit` says which).
    pub oi: f64,
    /// Weight unit: `"usd"` (notional) or `"contracts"` (count).
    pub oi_unit: String,
}

/// One OI-weighted funding point (OpenMarket
/// `GROUP_BY_TYPE_OPEN_INTEREST_WEIGHTED_AVG`): `rate = Σ(rateᵢ·oiᵢ)/Σoiᵢ`.
/// Weights are NEVER mixed across units: the OIWA is computed over the unit
/// class with the larger aggregate OI (USD preferred on a tie), so a venue
/// reporting only contracts (e.g. Hyperliquid, `oi_notional = NaN`) is not
/// silently combined with USD-notional venues.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OiwaSeries {
    pub interval_ts_ns: i64,
    pub rate: f64,
    /// Aggregate OI weight over the members that entered the OIWA.
    pub total_oi: f64,
    /// Unit of the weights used: `"usd"` or `"contracts"`.
    pub oi_unit: String,
    pub members: Vec<OiwaMember>,
}

/// OI-weighted funding over a merged, time-sorted event stream (as produced by
/// `load_logs_merged`). For each `interval_ns` bucket, each (venue, symbol)
/// contributes its funding rate (last seen in the bucket) weighted by its open
/// interest as-of the bucket end. Members without a positive OI reading
/// contribute no weight. Intervals with no funded members are omitted.
/// `interval_ns` defaults to the 8h funding cycle (use 28_800s) so hourly
/// venues normalize to one point per cycle.
pub fn oiwa_series(events: &[EventEnvelope], interval_ns: i64) -> Vec<OiwaSeries> {
    let interval = interval_ns.max(1);
    #[derive(Default)]
    struct Member {
        funding: BTreeMap<i64, f64>,
        oi_usd: Vec<(i64, f64)>,
        oi_contracts: Vec<(i64, f64)>,
    }
    let mut members: BTreeMap<(Venue, u32), Member> = BTreeMap::new();
    for ev in events {
        match ev.body {
            MarketEvent::Funding { rate, .. } => {
                let bucket = ev.recv_ts_ns.div_euclid(interval) * interval;
                members
                    .entry((ev.venue, ev.symbol.0))
                    .or_default()
                    .funding
                    .insert(bucket, rate);
            }
            MarketEvent::OpenInterest {
                oi_contracts,
                oi_notional,
                ..
            } => {
                let m = members.entry((ev.venue, ev.symbol.0)).or_default();
                if oi_notional.is_finite() && oi_notional > 0.0 {
                    m.oi_usd.push((ev.recv_ts_ns, oi_notional));
                }
                if oi_contracts.is_finite() && oi_contracts > 0.0 {
                    m.oi_contracts.push((ev.recv_ts_ns, oi_contracts));
                }
            }
            _ => {}
        }
    }
    // Union of funding buckets across all members, ascending.
    let mut buckets: Vec<i64> = Vec::new();
    for m in members.values() {
        for &b in m.funding.keys() {
            if !buckets.contains(&b) {
                buckets.push(b);
            }
        }
    }
    buckets.sort_unstable();

    /// As-of OI lookup: last reading at or before `end`.
    fn as_of(series: &[(i64, f64)], end: i64) -> Option<f64> {
        let idx = series.partition_point(|&(ts, _)| ts <= end);
        series.get(idx.wrapping_sub(1)).map(|&(_, v)| v)
    }

    let mut out = Vec::new();
    for bucket in buckets {
        let end = bucket + interval;
        // Collect the funded members with their OI in each unit class.
        let mut by_unit: BTreeMap<&str, Vec<OiwaMember>> = BTreeMap::new();
        for ((venue, _sym), m) in &members {
            let Some(&rate) = m.funding.get(&bucket) else {
                continue;
            };
            if let Some(oi) = as_of(&m.oi_usd, end) {
                by_unit.entry("usd").or_default().push(OiwaMember {
                    venue: venue.slug().to_owned(),
                    rate,
                    oi,
                    oi_unit: "usd".into(),
                });
            }
            if let Some(oi) = as_of(&m.oi_contracts, end) {
                by_unit.entry("contracts").or_default().push(OiwaMember {
                    venue: venue.slug().to_owned(),
                    rate,
                    oi,
                    oi_unit: "contracts".into(),
                });
            }
            // A funded member with neither OI class contributes no weight.
        }
        if by_unit.is_empty() {
            continue;
        }
        // Pick the unit class with the larger aggregate OI ("usd" on a tie).
        let chosen = pick_oiwa_class(&by_unit);
        let (unit, members_chosen) = chosen.unwrap();
        let mut num = 0.0;
        let mut den = 0.0;
        for m in members_chosen {
            num += m.rate * m.oi;
            den += m.oi;
        }
        out.push(OiwaSeries {
            interval_ts_ns: bucket,
            rate: if den > 0.0 { num / den } else { 0.0 },
            total_oi: den,
            oi_unit: unit.to_owned(),
            members: members_chosen.clone(),
        });
    }
    out
}

/// Pick the OI unit class with the larger aggregate OI ("usd" on a tie —
/// max_by keeps the Greater side, so the tie-break prefers the SHORTER unit
/// name, i.e. "usd" over "contracts"). Shared by [`oiwa_series`] and
/// [`carry_series`] so unit selection is identical everywhere.
fn pick_oiwa_class<'a>(
    by_unit: &'a BTreeMap<&'a str, Vec<OiwaMember>>,
) -> Option<(&'a str, &'a Vec<OiwaMember>)> {
    by_unit
        .iter()
        .max_by(|(a_u, a), (b_u, b)| {
            let sa: f64 = a.iter().map(|m| m.oi).sum();
            let sb: f64 = b.iter().map(|m| m.oi).sum();
            sa.total_cmp(&sb).then_with(|| b_u.len().cmp(&a_u.len()))
        })
        .map(|(u, m)| (*u, m))
}

/// One carry point per (interval, venue, symbol): the OI-weighted funding
/// rate plus the mark-vs-oracle basis — the two legs of the funding-carry
/// trade (OpenMarket market-stats: `basis = mark − index`, `basisBps =
/// basis/index × 10⁴`). `mark`/`index` are the LAST readings in the interval;
/// `index` (and thus `basis_bps`) is `NaN` when the venue omits the oracle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarryPoint {
    pub interval_ts_ns: i64,
    /// Venue slug (e.g. "hyperliquid").
    pub venue: String,
    /// SymbolId.0 (numeric — resolve via the shared symbol table at the edge).
    pub symbol: u32,
    /// OI-weighted funding rate in the interval.
    pub funding_rate: f64,
    /// Aggregate OI weight used for the OIWA.
    pub total_oi: f64,
    pub oi_unit: String,
    pub mark: f64,
    pub index: f64,
    /// `(mark − index)/index × 10⁴`; NaN when either leg is missing.
    pub basis_bps: f64,
}

/// Funding-carry series over a merged, time-sorted event stream: per
/// (interval, venue, symbol), the OI-weighted funding rate (same unit-class
/// rule as [`oiwa_series`]) paired with the mark-vs-oracle basis. Intervals
/// with a funded member are emitted even when the mark leg is absent
/// (`mark`/`basis_bps` NaN); intervals with NO funded member are omitted.
pub fn carry_series(events: &[EventEnvelope], interval_ns: i64) -> Vec<CarryPoint> {
    let interval = interval_ns.max(1);
    #[derive(Default)]
    struct Member {
        funding: BTreeMap<i64, f64>,
        oi_usd: Vec<(i64, f64)>,
        oi_contracts: Vec<(i64, f64)>,
        mark: BTreeMap<i64, f64>,
        index: BTreeMap<i64, f64>,
    }
    let mut members: BTreeMap<(Venue, u32), Member> = BTreeMap::new();
    for ev in events {
        let bucket = ev.recv_ts_ns.div_euclid(interval) * interval;
        match ev.body {
            MarketEvent::Funding { rate, .. } => {
                members
                    .entry((ev.venue, ev.symbol.0))
                    .or_default()
                    .funding
                    .insert(bucket, rate);
            }
            MarketEvent::OpenInterest {
                oi_contracts,
                oi_notional,
                ..
            } => {
                let m = members.entry((ev.venue, ev.symbol.0)).or_default();
                if oi_notional.is_finite() && oi_notional > 0.0 {
                    m.oi_usd.push((ev.recv_ts_ns, oi_notional));
                }
                if oi_contracts.is_finite() && oi_contracts > 0.0 {
                    m.oi_contracts.push((ev.recv_ts_ns, oi_contracts));
                }
            }
            MarketEvent::MarkPrice { mark, index } => {
                let m = members.entry((ev.venue, ev.symbol.0)).or_default();
                m.mark.insert(bucket, mark);
                if index.is_finite() {
                    m.index.insert(bucket, index);
                }
            }
            _ => {}
        }
    }
    let mut buckets: Vec<i64> = Vec::new();
    for m in members.values() {
        for &b in m.funding.keys() {
            if !buckets.contains(&b) {
                buckets.push(b);
            }
        }
    }
    buckets.sort_unstable();

    let mut out = Vec::new();
    for bucket in buckets {
        let end = bucket + interval;
        // Candidates for this bucket keyed by (venue slug, symbol) — several
        // symbols can share a venue, so the slug alone would collapse them.
        let mut cands: BTreeMap<(String, u32), Cand> = BTreeMap::new();
        for ((venue, sym), m) in &members {
            let Some(&rate) = m.funding.get(&bucket) else {
                continue;
            };
            cands.insert(
                (venue.slug().to_owned(), *sym),
                Cand {
                    rate,
                    oi_usd: oi_as_of(&m.oi_usd, end),
                    oi_contracts: oi_as_of(&m.oi_contracts, end),
                    mark: *m.mark.get(&bucket).unwrap_or(&f64::NAN),
                    index: *m.index.get(&bucket).unwrap_or(&f64::NAN),
                },
            );
        }
        if cands.is_empty() {
            continue;
        }
        // Unit classes over the candidates (same never-mix rule as OIWA).
        let mut by_unit: BTreeMap<&str, Vec<OiwaUnitEntry>> = BTreeMap::new();
        for (key, c) in &cands {
            if let Some(oi) = c.oi_usd {
                by_unit.entry("usd").or_default().push((key.clone(), oi));
            }
            if let Some(oi) = c.oi_contracts {
                by_unit
                    .entry("contracts")
                    .or_default()
                    .push((key.clone(), oi));
            }
        }
        if by_unit.is_empty() {
            continue;
        }
        let (unit, class) = pick_oiwa_class_simple(&by_unit);
        let mut den = 0.0;
        for (_, oi) in class {
            den += oi;
        }
        for ((slug, sym), _oi) in class {
            let c = &cands[&(slug.clone(), *sym)];
            let basis_bps = if c.mark.is_finite() && c.index.is_finite() && c.index != 0.0 {
                (c.mark - c.index) / c.index * 10_000.0
            } else {
                f64::NAN
            };
            out.push(CarryPoint {
                interval_ts_ns: bucket,
                venue: slug.clone(),
                symbol: *sym,
                funding_rate: c.rate,
                total_oi: den,
                oi_unit: unit.to_owned(),
                mark: c.mark,
                index: c.index,
                basis_bps,
            });
        }
    }
    out
}

/// One liquidation-stress interval (per venue, symbol): signed notional sums
/// by aggressor side — buy-side liquidations are the venue buying back
/// liquidated shorts (flow INTO the book), sell-side is longs being dumped.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiqPoint {
    pub interval_ts_ns: i64,
    pub venue: String,
    /// SymbolId.0 (numeric — resolve via the shared symbol table at the edge).
    pub symbol: u32,
    /// Σ(price·qty) for buy-side liquidations in the interval.
    pub buy_notional: f64,
    /// Σ(price·qty) for sell-side liquidations in the interval.
    pub sell_notional: f64,
    /// `buy_notional − sell_notional` (positive = short-squeeze pressure).
    pub net_notional: f64,
    pub n_buy: u64,
    pub n_sell: u64,
}

/// Liquidation-stress series over a merged, time-sorted event stream: per
/// (interval, venue, symbol), the signed notional sums of `Liquidation`
/// events. Intervals with no liquidation are omitted (the stress leg is
/// naturally sparse). Same bucket rule as [`carry_series`] — intervals line
/// up across queries for cross-leg join on `interval_ts_ns`.
pub fn liq_series(events: &[EventEnvelope], interval_ns: i64) -> Vec<LiqPoint> {
    let interval = interval_ns.max(1);
    #[derive(Default)]
    struct Acc {
        buy: f64,
        sell: f64,
        n_buy: u64,
        n_sell: u64,
    }
    let mut accs: BTreeMap<(Venue, u32, i64), Acc> = BTreeMap::new();
    for ev in events {
        if let MarketEvent::Liquidation { price, qty, side } = ev.body {
            if !price.is_finite() || !qty.is_finite() {
                continue; // CONV-8: never aggregate a non-finite sentinel
            }
            let bucket = ev.recv_ts_ns.div_euclid(interval) * interval;
            let a = accs.entry((ev.venue, ev.symbol.0, bucket)).or_default();
            let notional = price * qty;
            match side {
                Side::Buy => {
                    a.buy += notional;
                    a.n_buy += 1;
                }
                Side::Sell => {
                    a.sell += notional;
                    a.n_sell += 1;
                }
            }
        }
    }
    let mut out: Vec<LiqPoint> = accs
        .into_iter()
        .map(|((venue, symbol, bucket), a)| LiqPoint {
            interval_ts_ns: bucket,
            venue: venue.slug().to_owned(),
            symbol,
            buy_notional: a.buy,
            sell_notional: a.sell,
            net_notional: a.buy - a.sell,
            n_buy: a.n_buy,
            n_sell: a.n_sell,
        })
        .collect();
    out.sort_by_key(|p| (p.interval_ts_ns, p.venue.clone(), p.symbol));
    out
}

/// One carry candidate per funded (venue, symbol) in an interval.
struct Cand {
    rate: f64,
    oi_usd: Option<f64>,
    oi_contracts: Option<f64>,
    mark: f64,
    index: f64,
}

/// A (venue-slug, symbol) identity with its OI weight in one unit class.
type OiwaUnitEntry = ((String, u32), f64);

/// Unit-class selection over `(key, oi)` pairs — same rule as
/// [`pick_oiwa_class`] (larger aggregate OI, "usd" on a tie).
fn pick_oiwa_class_simple<'a>(
    by_unit: &'a BTreeMap<&'a str, Vec<OiwaUnitEntry>>,
) -> (&'a str, &'a Vec<OiwaUnitEntry>) {
    by_unit
        .iter()
        .max_by(|(a_u, a), (b_u, b)| {
            let sa: f64 = a.iter().map(|(_, o)| o).sum();
            let sb: f64 = b.iter().map(|(_, o)| o).sum();
            sa.total_cmp(&sb).then_with(|| b_u.len().cmp(&a_u.len()))
        })
        .map(|(u, m)| (*u, m))
        .expect("by_unit is non-empty by construction")
}

/// As-of OI lookup: last reading at or before `end`.
fn oi_as_of(series: &[(i64, f64)], end: i64) -> Option<f64> {
    let idx = series.partition_point(|&(ts, _)| ts <= end);
    series.get(idx.wrapping_sub(1)).map(|&(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::{EventEnvelope, SymbolId};

    fn trade(
        venue: Venue,
        sym: u32,
        recv_ns: i64,
        price: f64,
        qty: f64,
        side: Side,
        id: u64,
    ) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            SymbolId(sym),
            recv_ns,
            recv_ns,
            id,
            MarketEvent::Trade {
                price,
                qty,
                side,
                trade_id: id,
            },
        )
    }

    fn funding(venue: Venue, sym: u32, recv_ns: i64, rate: f64) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            SymbolId(sym),
            recv_ns,
            recv_ns,
            1,
            MarketEvent::Funding {
                rate,
                interval_s: 3600,
                next_funding_ts_ns: recv_ns + 3_600_000_000_000,
            },
        )
    }

    fn oi(venue: Venue, sym: u32, recv_ns: i64, oi_notional: f64) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            SymbolId(sym),
            recv_ns,
            recv_ns,
            2,
            MarketEvent::OpenInterest {
                oi_contracts: oi_notional,
                oi_notional,
            },
        )
    }

    fn liq(
        venue: Venue,
        sym: u32,
        recv_ns: i64,
        price: f64,
        qty: f64,
        side: Side,
    ) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            SymbolId(sym),
            recv_ns,
            recv_ns,
            3,
            MarketEvent::Liquidation { price, qty, side },
        )
    }

    #[test]
    fn footprint_bars_aggregate_and_split_sides() {
        let t0 = 1_700_000_000_000_000_000i64; // some floor-aligned-ish time
        let evs = vec![
            trade(Venue::Hyperliquid, 1, t0, 100.0, 2.0, Side::Buy, 1),
            trade(Venue::Hyperliquid, 1, t0 + 1, 110.0, 3.0, Side::Sell, 2),
            trade(Venue::Hyperliquid, 1, t0 + 2, 90.0, 1.0, Side::Buy, 3),
            // Next bucket:
            trade(
                Venue::Hyperliquid,
                1,
                t0 + 60_000_000_000,
                95.0,
                5.0,
                Side::Sell,
                4,
            ),
        ];
        let bars = footprint_bars(&evs, 60_000_000_000);
        assert_eq!(bars.len(), 2);
        let b0 = bars[0];
        assert_eq!(b0.open, 100.0);
        assert_eq!(b0.high, 110.0);
        assert_eq!(b0.low, 90.0);
        assert_eq!(b0.close, 90.0);
        assert_eq!(b0.buy_vol, 3.0); // 2 + 1
        assert_eq!(b0.sell_vol, 3.0);
        assert_eq!(b0.n_trades, 3);
        // vwap = (100*2 + 110*3 + 90*1) / 6 = 620/6 ≈ 103.333
        assert!((b0.vwap - 103.3333333).abs() < 1e-6);
        let b1 = bars[1];
        assert_eq!(b1.sell_vol, 5.0);
        assert_eq!(b1.buy_vol, 0.0);
        assert_eq!(b1.open, 95.0);
        assert_eq!(b1.vwap, 95.0);
    }

    #[test]
    fn footprint_buckets_block_price() {
        let t0 = 1_700_000_000_000_000_000i64;
        let evs = vec![
            trade(Venue::Hyperliquid, 1, t0, 64_850.0, 1.0, Side::Buy, 1),
            trade(Venue::Hyperliquid, 1, t0 + 1, 64_920.0, 2.0, Side::Sell, 2),
            trade(Venue::Hyperliquid, 1, t0 + 2, 64_880.0, 1.0, Side::Buy, 3),
        ];
        let rows = footprint_buckets(&evs, 60_000_000_000, 100.0);
        assert_eq!(rows.len(), 2);
        // 64850 floors to 64850; 64920 floors to 64900; 64880 floors to 64800.
        assert_eq!(rows[0].price, 64_800.0);
        assert_eq!(rows[0].buy_vol, 2.0); // 64850 + 64880 both floor to the 64800 block
        assert_eq!(rows[0].sell_vol, 0.0);
        assert_eq!(rows[0].n_trades, 2);
        assert_eq!(rows[1].price, 64_900.0);
        assert_eq!(rows[1].sell_vol, 2.0);
        assert_eq!(rows[1].buy_vol, 0.0);
        assert_eq!(rows[1].n_trades, 1);
    }

    #[test]
    fn liq_series_sums_signed_notional_per_interval() {
        let t0 = 1_700_000_000_000_000_000i64;
        let evs = vec![
            // Hour bucket A (t0): one buy (short flushed) + two sells (longs).
            liq(Venue::Bybit, 1, t0, 100.0, 2.0, Side::Buy),
            liq(Venue::Bybit, 1, t0 + 1, 100.0, 1.0, Side::Sell),
            liq(Venue::Bybit, 1, t0 + 2, 110.0, 5.0, Side::Sell),
            // Hour bucket B (t0 + 1h): one sell on the same symbol.
            liq(
                Venue::Bybit,
                1,
                t0 + 3_600_000_000_000,
                100.0,
                3.0,
                Side::Sell,
            ),
            // A NaN-priced liquidation must never poison the sums (CONV-8).
            liq(Venue::Bybit, 1, t0 + 3, f64::NAN, 9.0, Side::Buy),
            // Another symbol in the same bucket stays separate.
            liq(Venue::Bybit, 2, t0, 50.0, 4.0, Side::Sell),
            // A different venue is never merged into bybit's row.
            liq(Venue::BinanceFutures, 1, t0, 100.0, 7.0, Side::Buy),
        ];
        let out = liq_series(&evs, 3_600_000_000_000);
        // Sorted by (interval, venue, symbol): "binance" < "bybit", and the
        // t0 bucket precedes the t0+1h bucket.
        assert_eq!(out.len(), 4);
        let binance = &out[0];
        assert_eq!(binance.venue, "binance");
        assert!((binance.buy_notional - 700.0).abs() < 1e-9);
        assert_eq!(binance.n_buy, 1);
        let bybit_b = &out[1];
        assert_eq!(bybit_b.venue, "bybit");
        assert_eq!(bybit_b.symbol, 1);
        assert_eq!(
            bybit_b.interval_ts_ns,
            t0.div_euclid(3_600_000_000_000) * 3_600_000_000_000
        );
        // buy = 100×2 = 200; sell = 100×1 + 110×5 = 650 → net −450.
        assert!((bybit_b.buy_notional - 200.0).abs() < 1e-9);
        assert!((bybit_b.sell_notional - 650.0).abs() < 1e-9);
        assert!((bybit_b.net_notional + 450.0).abs() < 1e-9);
        assert_eq!((bybit_b.n_buy, bybit_b.n_sell), (1, 2));
        // bybit symbol 2 (never touched by the NaN / other-venue rows).
        assert_eq!(out[2].symbol, 2);
        assert!((out[2].sell_notional - 200.0).abs() < 1e-9);
        assert_eq!(out[2].n_sell, 1);
        // The t0+1h bybit bucket: same symbol, separate row.
        assert_eq!(
            out[3].interval_ts_ns,
            bybit_b.interval_ts_ns + 3_600_000_000_000
        );
        assert!((out[3].sell_notional - 300.0).abs() < 1e-9);
        assert_eq!(out[3].n_sell, 1);
    }

    #[test]
    fn oiwa_weights_by_oi() {
        let t0 = 1_700_000_000_000_000_000i64;
        let mut evs = vec![
            oi(Venue::Hyperliquid, 1, t0 - 60, 400.0), // OI before funding
            funding(Venue::Hyperliquid, 1, t0, 0.0001),
            oi(Venue::BinanceFutures, 1, t0 - 60, 100.0),
            funding(Venue::BinanceFutures, 1, t0, 0.0003),
            // A third venue with funding but NO OI reading → excluded.
            funding(Venue::Bybit, 1, t0, 0.0002),
        ];
        // load_logs_merged order = time-sorted; keep it sorted here too.
        evs.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
        let out = oiwa_series(&evs, 28_800_000_000_000); // 8h
        assert_eq!(out.len(), 1);
        let s = &out[0];
        // num = 0.0001*400 + 0.0003*100 = 0.04 + 0.03 = 0.07; den = 500 → 0.00014
        assert!((s.rate - 0.00014).abs() < 1e-12);
        assert!((s.total_oi - 500.0).abs() < 1e-9);
        assert_eq!(s.oi_unit, "usd");
        assert_eq!(s.members.len(), 2);
        assert_eq!(s.members[0].venue, "binance");
        assert_eq!(s.members[1].venue, "hyperliquid");
        assert_eq!(s.members[0].oi_unit, "usd");
    }

    #[test]
    fn carry_pairs_funding_with_basis() {
        let t0 = 1_700_000_000_000_000_000i64;
        let mut evs = vec![
            oi(Venue::Hyperliquid, 1, t0 - 60, 200.0),
            funding(Venue::Hyperliquid, 1, t0, 0.0001),
            // Mark + oracle: mark 100.1 vs index 100.0 → +10 bps basis.
            EventEnvelope::new(
                Venue::Hyperliquid,
                SymbolId(1),
                t0,
                t0,
                3,
                MarketEvent::MarkPrice {
                    mark: 100.1,
                    index: 100.0,
                },
            ),
        ];
        evs.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
        let out = carry_series(&evs, 3_600_000_000_000); // 1h
        assert_eq!(out.len(), 1);
        let p = &out[0];
        assert_eq!(p.venue, "hyperliquid");
        assert_eq!(p.symbol, 1);
        assert!((p.funding_rate - 0.0001).abs() < 1e-15);
        assert!((p.total_oi - 200.0).abs() < 1e-9);
        // Both units present and equal → tie-break prefers "usd".
        assert_eq!(p.oi_unit, "usd");
        assert!((p.mark - 100.1).abs() < 1e-12);
        assert!((p.index - 100.0).abs() < 1e-12);
        // (100.1 − 100)/100 × 10⁴ = 10 bps
        assert!((p.basis_bps - 10.0).abs() < 1e-9);

        // A funded interval with NO mark leg still emits (mark/basis NaN).
        let mut evs2 = vec![
            oi(Venue::Hyperliquid, 1, t0 - 60, 200.0),
            funding(Venue::Hyperliquid, 1, t0, 0.0001),
        ];
        evs2.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
        let out2 = carry_series(&evs2, 3_600_000_000_000);
        assert_eq!(out2.len(), 1);
        assert!(out2[0].mark.is_nan());
        assert!(out2[0].basis_bps.is_nan());
    }

    #[test]
    fn carry_keeps_multiple_symbols_per_venue() {
        // Regression: candidates were keyed by venue slug only, so a second
        // symbol on the SAME venue silently overwrote the first in each
        // bucket (BTC vanished behind ETH on hyperliquid).
        let t0 = 1_700_000_000_000_000_000i64;
        let mut evs = vec![
            oi(Venue::Hyperliquid, 1, t0 - 60, 200.0),
            funding(Venue::Hyperliquid, 1, t0, 0.0001),
            oi(Venue::Hyperliquid, 2, t0 - 60, 300.0),
            funding(Venue::Hyperliquid, 2, t0, 0.0002),
        ];
        evs.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
        let out = carry_series(&evs, 3_600_000_000_000);
        assert_eq!(out.len(), 2, "both symbols must emit per interval");
        let mut by_sym: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
        for p in &out {
            by_sym.insert(p.symbol, p.funding_rate);
        }
        assert_eq!(by_sym.len(), 2);
        assert!((by_sym[&1] - 0.0001).abs() < 1e-15);
        assert!((by_sym[&2] - 0.0002).abs() < 1e-15);
    }

    #[test]
    fn oiwa_omits_unfunded_intervals() {
        let t0 = 1_700_000_000_000_000_000i64;
        let evs = vec![
            oi(Venue::Hyperliquid, 1, t0, 100.0),
            oi(Venue::Hyperliquid, 1, t0 + 28_800_000_000_000, 100.0),
        ];
        assert!(oiwa_series(&evs, 28_800_000_000_000).is_empty());
    }

    #[test]
    fn oiwa_falls_back_to_contracts_and_never_mixes_units() {
        let t0 = 1_700_000_000_000_000_000i64;
        // Hyperliquid: contracts only (oi_notional NaN, like the real
        // collector). Binance: USD notional. The OIWA must pick ONE unit
        // class, not silently combine contracts with notional.
        let hl = EventEnvelope::new(
            Venue::Hyperliquid,
            SymbolId(1),
            t0 - 60,
            t0 - 60,
            1,
            MarketEvent::OpenInterest {
                oi_contracts: 200.0,
                oi_notional: f64::NAN,
            },
        );
        let hl_f = funding(Venue::Hyperliquid, 1, t0, 0.0001);
        // Binance: USD notional only (no contract count) — must not pollute
        // the contracts class.
        let bn = EventEnvelope::new(
            Venue::BinanceFutures,
            SymbolId(1),
            t0 - 60,
            t0 - 60,
            2,
            MarketEvent::OpenInterest {
                oi_contracts: f64::NAN,
                oi_notional: 300.0,
            },
        );
        let bn_f = funding(Venue::BinanceFutures, 1, t0, 0.0003);
        let mut evs = vec![hl.clone(), hl_f.clone(), bn, bn_f];
        evs.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
        let out = oiwa_series(&evs, 28_800_000_000_000);
        assert_eq!(out.len(), 1);
        let s = &out[0];
        // USD class aggregate (300) > contracts (200) → usd wins, HL excluded.
        assert_eq!(s.oi_unit, "usd");
        assert_eq!(s.members.len(), 1);
        assert_eq!(s.members[0].venue, "binance");
        assert!((s.rate - 0.0003).abs() < 1e-15);

        // Now make contracts dominate: no USD members at all → contracts path.
        let mut evs2 = vec![hl.clone(), hl_f];
        evs2.sort_by_key(|e| (e.recv_ts_ns, e.stream_seq));
        let out2 = oiwa_series(&evs2, 28_800_000_000_000);
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].oi_unit, "contracts");
        assert_eq!(out2[0].members[0].venue, "hyperliquid");
        assert!((out2[0].total_oi - 200.0).abs() < 1e-9);
        assert!((out2[0].rate - 0.0001).abs() < 1e-15);
    }

    fn snapshot(
        venue: Venue,
        sym: u32,
        recv_ns: i64,
        seq: u64,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
    ) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            SymbolId(sym),
            recv_ns,
            recv_ns,
            seq as u64,
            MarketEvent::BookSnapshot {
                bids: bids.to_vec().into(),
                asks: asks.to_vec().into(),
                seq,
                depth: 10,
                reason: mp_core::SnapshotReason::Periodic,
            },
        )
    }

    fn delta(
        venue: Venue,
        sym: u32,
        recv_ns: i64,
        first_seq: u64,
        last_seq: u64,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
    ) -> EventEnvelope {
        EventEnvelope::new(
            venue,
            SymbolId(sym),
            recv_ns,
            recv_ns,
            last_seq as u64,
            MarketEvent::BookDelta {
                bids: bids.to_vec().into(),
                asks: asks.to_vec().into(),
                first_seq,
                last_seq,
            },
        )
    }

    #[test]
    fn dom_samples_warm_book_at_boundaries() {
        let t0 = 1_700_000_000_000_000_000i64;
        let evs = vec![
            snapshot(
                Venue::Hyperliquid,
                1,
                t0 - 10,
                10,
                &[(101.0, 5.0), (100.5, 3.0)],
                &[(102.0, 4.0)],
            ),
            // Warm book in place before the window opens.
            delta(
                Venue::Hyperliquid,
                1,
                t0 + 1,
                11,
                11,
                &[],
                &[(102.0, 0.0), (102.5, 1.5)],
            ),
        ];
        let out = dom_series(&evs, t0, t0 + 10_000_000_000, 5_000_000_000, 5, 100);
        // 3 samples at t0, t0+5s, t0+10s.
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].ts_ns, t0);
        assert!(!out[0].stale);
        // First sample carries the warm book (ask 102.0 before the t0+1 delta).
        assert_eq!(out[0].bids, vec![(101.0, 5.0), (100.5, 3.0)]);
        assert_eq!(out[0].asks, vec![(102.0, 4.0)]);
        // t0+1 delta moved the ask to 102.5; t0+5s sample sees it.
        assert_eq!(out[1].asks, vec![(102.5, 1.5)]);
        assert_eq!(out[1].bids, vec![(101.0, 5.0), (100.5, 3.0)]);
        // t0+10s: no new events, same book as the 5s sample.
        assert_eq!(out[2].asks, vec![(102.5, 1.5)]);
    }

    #[test]
    fn dom_levels_capped_and_stale_samples_empty() {
        let t0 = 1_700_000_000_000_000_000i64;
        // Snapshot seq 10, then a hole: next delta starts at 12 (gap → stale).
        let evs = vec![
            snapshot(
                Venue::Hyperliquid,
                1,
                t0 - 1,
                10,
                &[(100.0, 1.0)],
                &[(101.0, 1.0)],
            ),
            delta(Venue::Hyperliquid, 1, t0, 12, 12, &[], &[(101.5, 2.0)]),
            snapshot(
                Venue::Hyperliquid,
                1,
                t0 + 10_000_000_000,
                20,
                &[(99.0, 2.0)],
                &[(102.0, 3.0)],
            ),
        ];
        let out = dom_series(&evs, t0, t0 + 15_000_000_000, 5_000_000_000, 1, 100);
        assert_eq!(out.len(), 4);
        assert!(out[0].stale, "gap must mark the ladder stale");
        assert!(out[0].bids.is_empty());
        // Recovery at the new snapshot, levels capped to 1.
        assert!(!out[2].stale);
        assert_eq!(out[2].bids, vec![(99.0, 2.0)]);
        assert_eq!(out[2].asks, vec![(102.0, 3.0)]);
    }

    #[test]
    fn dom_max_samples_caps_and_window_is_bounded() {
        let t0 = 1_700_000_000_000_000_000i64;
        let evs = vec![
            snapshot(
                Venue::Hyperliquid,
                1,
                t0 - 1,
                10,
                &[(100.0, 1.0)],
                &[(101.0, 1.0)],
            ),
            snapshot(
                Venue::Hyperliquid,
                1,
                t0 + 60_000_000_000,
                11,
                &[(99.0, 1.0)],
                &[(102.0, 1.0)],
            ),
        ];
        // every = 100ms over a 60s window → 600 samples, capped at 50.
        let out = dom_series(&evs, t0, t0 + 60_000_000_000, 100_000_000, 5, 50);
        assert_eq!(out.len(), 50);
        assert_eq!(out[0].ts_ns, t0);
        assert_eq!(out[49].ts_ns, t0 + 49 * 100_000_000);
        // A reversed window yields nothing.
        assert!(dom_series(&evs, t0 + 10, t0, 100_000_000, 5, 50).is_empty());
    }
}
