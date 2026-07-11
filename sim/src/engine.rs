//! Event-replay backtester (SIM-1, SIM-5). Drives a [`SimClock`] from event
//! timestamps only (no wall time), running the PRODUCTION feature engine,
//! strategy, and sizing crates unmodified — only the clock and fill model
//! differ from live.
//!
//! Fill-model ladder (SIM-2): `L0BarFill` fills at the next bar's open
//! (daily/hourly strategies); `L1TopOfBook` fills market intents against the
//! opposing best price (capped by displayed top-of-book qty × participation,
//! remainder walks forward) and limit intents only when a trade prints through
//! the price (trade-print rule — touching is not filling, SIM-12 tags these
//! `optimistic_maker`); `L2DepthWalk` walks the reconstructed book for market
//! intents (paying impact) and caps limit fills by traded volume ×
//! `queue_share`. Without book data (no `BookSnapshot`/`BookDelta` in the
//! stream), L1/L2 market orders fall back to filling fully at the next trade
//! print — the same behavior as before this ladder existed, so fixtures built
//! from trade-only feeds are unaffected.

use crate::account::Accountant;
use crate::decision_log::DecisionLog;
use crate::error::SimError;
use crate::metrics::Metrics;
use mp_core::{
    BookMirror, Clock, EventEnvelope, Fill, Liquidity, MarketEvent, OrderIntent, OrderKind, Side,
    SimClock, SizeUnit, SymbolId,
};
use mp_features::{BarBuilder, FeatureEngine};
use mp_risk::{size, SizingInputs, SizingParams};
use mp_strategies::strategy::{Ctx, TimerId};
use mp_strategies::Strategy;
use std::collections::BTreeMap;

/// Fill-model selection (SIM-2 ladder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillModel {
    /// Bar-close model for daily/hourly strategies: fills at the next bar's
    /// open ± a tick/bps haircut, always complete (no partials).
    L0BarFill,
    /// Top-of-book model: market orders capped by displayed qty × participation
    /// with partial-fill walk-forward; limit orders fill on trade-print-through.
    #[default]
    L1TopOfBook,
    /// Depth-walk model: market orders walk the reconstructed book (impact
    /// paid); limit orders capped by traded volume × queue_share per print.
    L2DepthWalk,
}

/// Backtester configuration.
#[derive(Debug, Clone, Copy)]
pub struct SimConfig {
    pub fill_model: FillModel,
    /// Intent→fill latency (SIM-3); 0 only in unit tests.
    pub latency_ns: i64,
    pub taker_fee: f64,
    /// Slippage as a fraction of price (L0/L1 fallback path).
    pub slip_frac: f64,
    /// L1: displayed top-of-book qty fraction a market order may take.
    pub participation: f64,
    /// L2: traded-volume fraction a resting limit may claim per print.
    pub queue_share: f64,
    /// L0: bar timeframe for the bar-open fill reference.
    pub bar_tf_ns: i64,
    pub start_cash: f64,
    /// Fallback instrument vol fraction when no `vol.rv` feature is present yet.
    pub default_vol_frac: f64,
    pub per_trade_risk_pct: f64,
    pub k_stop: f64,
    pub step_size: f64,
    pub min_notional: f64,
    /// SIM-6: minimum manifest coverage a run will accept (see `run_checked`).
    pub min_coverage: f64,
    /// SIM-4: how long a perp position may be held with zero Funding events
    /// seen before the run refuses to report (funding cost would be a silent
    /// zero otherwise).
    pub funding_check_interval_ns: i64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            fill_model: FillModel::default(),
            latency_ns: 150_000_000, // 150ms
            taker_fee: 0.00055,
            slip_frac: 0.0001,
            participation: 0.5,
            queue_share: 0.25,
            bar_tf_ns: 60_000_000_000, // 1 minute
            start_cash: 100_000.0,
            default_vol_frac: 0.02,
            per_trade_risk_pct: 0.005,
            k_stop: 1.5,
            step_size: 0.0001,
            min_notional: 5.0,
            min_coverage: 0.995,
            funding_check_interval_ns: 28_800_000_000_000, // 8h
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PendingKind {
    Market,
    Limit(f64),
}

struct Pending {
    symbol: SymbolId,
    side: Side,
    qty: f64,
    kind: PendingKind,
    ready_ns: i64,
    intent_id: mp_core::IntentId,
}

/// Deterministic per-event context handed to the strategy.
struct SimCtx {
    now: i64,
    equity: f64,
    positions: BTreeMap<SymbolId, f64>,
    rng: u64,
    timers: u64,
}
impl Ctx for SimCtx {
    fn now_ns(&self) -> i64 {
        self.now
    }
    fn position(&self, symbol: SymbolId) -> f64 {
        self.positions.get(&symbol).copied().unwrap_or(0.0)
    }
    fn equity_allocated(&self) -> f64 {
        self.equity
    }
    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn set_timer(&mut self, _after_ns: i64) -> TimerId {
        self.timers += 1;
        TimerId(self.timers)
    }
    fn log(&mut self, _msg: &str) {}
}

/// The backtester.
pub struct Backtester {
    fe: FeatureEngine,
    strat: Box<dyn Strategy>,
    acct: Accountant,
    clock: SimClock,
    cfg: SimConfig,
    log: DecisionLog,
    metrics: Metrics,
    pending: Vec<Pending>,
    latest_vol: BTreeMap<SymbolId, f64>,
    latest_mark: BTreeMap<SymbolId, f64>,
    books: BTreeMap<SymbolId, BookMirror>,
    bars: BTreeMap<SymbolId, BarBuilder>,
    funding_seen: BTreeMap<SymbolId, i64>,
    run_start_ns: Option<i64>,
    rng: u64,
    seq: u64,
    next_intent: u128,
}

impl Backtester {
    pub fn new(fe: FeatureEngine, strat: Box<dyn Strategy>, cfg: SimConfig, seed: u64) -> Self {
        Self {
            fe,
            strat,
            acct: Accountant::new(cfg.start_cash),
            clock: SimClock::new(0),
            cfg,
            log: DecisionLog::new(),
            metrics: Metrics::new(),
            pending: Vec::new(),
            latest_vol: BTreeMap::new(),
            latest_mark: BTreeMap::new(),
            books: BTreeMap::new(),
            bars: BTreeMap::new(),
            funding_seen: BTreeMap::new(),
            run_start_ns: None,
            rng: seed,
            seq: 0,
            next_intent: 0,
        }
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }
    pub fn decision_log(&self) -> &DecisionLog {
        &self.log
    }
    pub fn equity(&self) -> f64 {
        self.acct.equity()
    }
    pub fn position(&self, symbol: SymbolId) -> f64 {
        self.acct.position(symbol)
    }
    pub fn avg_cost(&self, symbol: SymbolId) -> f64 {
        self.acct.avg_cost(symbol)
    }
    pub fn identity_residual(&self) -> f64 {
        self.acct.identity_residual()
    }
    pub fn now_ns(&self) -> i64 {
        self.clock.now_ns()
    }
    /// SIM-8: expectancy under the 2×-costs stress (always available in the
    /// report alongside the base expectancy).
    pub fn stress_expectancy_2x(&self) -> f64 {
        self.metrics.stress_expectancy(self.acct.fees(), 2.0)
    }

    /// Replay a full event sequence at full (1.0) assumed coverage. See
    /// [`Self::run_checked`] for the SIM-6 coverage gate.
    pub fn run(&mut self, events: &[EventEnvelope]) -> Result<(), SimError> {
        self.run_checked(events, 1.0)
    }

    /// Replay a full event sequence (must be in global recv order), refusing
    /// to run if `coverage` (the consumed stream's manifest coverage, spec 003)
    /// is below `cfg.min_coverage` (SIM-6), and refusing to report if a held
    /// perp position never saw a Funding event within the check interval
    /// (SIM-4). A gap the sim doesn't know about is a number that lies.
    pub fn run_checked(&mut self, events: &[EventEnvelope], coverage: f64) -> Result<(), SimError> {
        if coverage < self.cfg.min_coverage {
            return Err(SimError::LowCoverage {
                actual: coverage,
                required: self.cfg.min_coverage,
            });
        }
        for ev in events {
            self.run_start_ns.get_or_insert(ev.recv_ts_ns);
            self.clock.set(ev.recv_ts_ns);
            self.on_event(ev);
            self.metrics.sample_equity(self.acct.equity());
        }
        self.check_funding_coverage()
    }

    fn check_funding_coverage(&self) -> Result<(), SimError> {
        let Some(start) = self.run_start_ns else {
            return Ok(());
        };
        let now = self.clock.now_ns();
        if now - start < self.cfg.funding_check_interval_ns {
            return Ok(()); // run too short to expect a funding event yet
        }
        for (symbol, qty) in self.acct.positions() {
            if qty != 0.0 && !self.funding_seen.contains_key(&symbol) {
                return Err(SimError::MissingFunding(symbol));
            }
        }
        Ok(())
    }

    fn on_event(&mut self, ev: &EventEnvelope) {
        self.books.entry(ev.symbol).or_default().apply(&ev.body);

        match &ev.body {
            MarketEvent::Trade {
                price, qty, side, ..
            } => {
                self.latest_mark.insert(ev.symbol, *price);
                self.acct.mark(ev.symbol, *price);
                self.on_trade(ev.symbol, *price, *qty, *side, ev.recv_ts_ns);
            }
            MarketEvent::MarkPrice { mark, .. } => {
                self.latest_mark.insert(ev.symbol, *mark);
                self.acct.mark(ev.symbol, *mark);
            }
            MarketEvent::Funding { rate, .. } => {
                self.funding_seen.insert(ev.symbol, ev.recv_ts_ns);
                self.acct.accrue_funding(ev.symbol, *rate);
            }
            _ => {}
        }

        // Market-order fills (L1/L2) may also progress on non-trade events
        // (e.g. a book delta moves the touch) — SIM-2 "first event with
        // recv_ts_ns >= ready".
        if !matches!(ev.body, MarketEvent::Trade { .. })
            && self.cfg.fill_model != FillModel::L0BarFill
        {
            self.try_fill_market(ev.symbol, ev.recv_ts_ns);
        }

        // Features → strategy → sized intents.
        let ups = self.fe.on_event(ev);
        for u in ups {
            if u.feature.starts_with("vol.rv") {
                self.latest_vol.insert(u.symbol, u.value);
            }
            let mut ctx = SimCtx {
                now: ev.recv_ts_ns,
                equity: self.acct.equity(),
                positions: self.acct.positions(),
                rng: self.rng,
                timers: 0,
            };
            let intents = self.strat.on_feature(&u, &mut ctx);
            self.rng = ctx.rng;
            for intent in intents {
                self.seq += 1;
                self.log.record_intent(self.seq, &intent);
                self.enqueue(&intent, ev.recv_ts_ns);
            }
        }
    }

    /// Dispatch trade-driven fill logic for the configured model.
    fn on_trade(&mut self, symbol: SymbolId, price: f64, qty: f64, side: Side, now: i64) {
        match self.cfg.fill_model {
            FillModel::L0BarFill => self.on_trade_l0(symbol, price, qty, side, now),
            FillModel::L1TopOfBook | FillModel::L2DepthWalk => {
                self.try_fill_limit_trade_print(symbol, price, qty, side, now);
                self.try_fill_market(symbol, now);
            }
        }
    }

    fn enqueue(&mut self, intent: &OrderIntent, now: i64) {
        let kind = match intent.kind {
            OrderKind::Market => PendingKind::Market,
            OrderKind::Limit { px } => PendingKind::Limit(px),
            OrderKind::Cancel { .. } => return, // not modeled in this slice
        };
        let mark = self.latest_mark.get(&intent.symbol).copied().unwrap_or(0.0);
        let qty = match intent.qty {
            SizeUnit::Contracts(c) => c.max(0.0),
            SizeUnit::RiskUnits(u) => {
                let vol = self
                    .latest_vol
                    .get(&intent.symbol)
                    .copied()
                    .unwrap_or(self.cfg.default_vol_frac);
                let sized = size(
                    &SizingParams {
                        per_trade_risk_pct: self.cfg.per_trade_risk_pct,
                    },
                    &SizingInputs {
                        risk_units: u,
                        equity: self.acct.equity(),
                        alloc_weight: 1.0,
                        instrument_vol_frac: vol,
                        mark_price: mark,
                        k_stop: self.cfg.k_stop,
                        step_size: self.cfg.step_size,
                        min_notional: self.cfg.min_notional,
                    },
                );
                sized.qty_contracts
            }
        };
        if qty <= 0.0 {
            return;
        }
        self.pending.push(Pending {
            symbol: intent.symbol,
            side: intent.side,
            qty,
            kind,
            ready_ns: now + self.cfg.latency_ns,
            intent_id: intent.intent_id,
        });
    }

    /// L0: on the bar that follows the bar an intent was queued in, fill it
    /// completely at that new bar's open ± a tick/bps haircut (SIM-2).
    fn on_trade_l0(&mut self, symbol: SymbolId, price: f64, qty: f64, side: Side, now: i64) {
        let bar = self
            .bars
            .entry(symbol)
            .or_insert_with(|| BarBuilder::new(self.cfg.bar_tf_ns));
        let closed = bar.on_event(
            now,
            &MarketEvent::Trade {
                price,
                qty,
                side,
                trade_id: 0,
            },
        );
        let Some(closed) = closed else { return };
        let new_open = self.bars.get(&symbol).unwrap().current_open();

        let mut still = Vec::new();
        let ready: Vec<Pending> = std::mem::take(&mut self.pending);
        for p in ready {
            if p.symbol != symbol || p.ready_ns > closed.close_ts_ns {
                still.push(p);
                continue;
            }
            let px = match p.side {
                Side::Buy => new_open * (1.0 + self.cfg.slip_frac),
                Side::Sell => new_open * (1.0 - self.cfg.slip_frac),
            };
            self.execute_fill(
                symbol,
                p.side,
                px,
                p.qty,
                p.intent_id,
                Liquidity::Taker,
                false,
            );
        }
        self.pending = still;
    }

    /// L1/L2 market-order fills: opposing best price (L1, capped by displayed
    /// qty × participation) or a full book walk (L2, impact paid). Falls back
    /// to the latest trade price, filled in full, when no book is available —
    /// preserves the pre-ladder behavior for trade-only fixtures.
    fn try_fill_market(&mut self, symbol: SymbolId, now: i64) {
        let mut still = Vec::new();
        let ready: Vec<Pending> = std::mem::take(&mut self.pending);
        for p in ready {
            if p.symbol != symbol || p.ready_ns > now || p.kind != PendingKind::Market {
                still.push(p);
                continue;
            }
            let filled = match self.cfg.fill_model {
                FillModel::L1TopOfBook => self.fill_l1_market(symbol, p.side, p.qty),
                FillModel::L2DepthWalk => self.fill_l2_market(symbol, p.side, p.qty),
                FillModel::L0BarFill => None, // handled in on_trade_l0
            };
            match filled {
                Some((px, qty)) if qty > 0.0 => {
                    self.execute_fill(
                        symbol,
                        p.side,
                        px,
                        qty,
                        p.intent_id,
                        Liquidity::Taker,
                        false,
                    );
                    if qty < p.qty {
                        still.push(Pending {
                            qty: p.qty - qty,
                            ..p
                        }); // remainder walks forward
                    }
                }
                _ => still.push(p),
            }
        }
        self.pending = still;
    }

    /// L1: opposing best price capped by displayed qty × participation; falls
    /// back to the trade tape (uncapped, full fill) when the book is stale.
    fn fill_l1_market(&self, symbol: SymbolId, side: Side, qty: f64) -> Option<(f64, f64)> {
        let book = self.books.get(&symbol);
        let touch = match side {
            Side::Buy => book.and_then(|b| b.best_ask()),
            Side::Sell => book.and_then(|b| b.best_bid()),
        };
        if let Some((px, top_qty)) = touch {
            let cap = top_qty * self.cfg.participation;
            let fill_qty = qty.min(cap.max(0.0));
            if fill_qty <= 0.0 {
                return None;
            }
            let slipped = match side {
                Side::Buy => px * (1.0 + self.cfg.slip_frac),
                Side::Sell => px * (1.0 - self.cfg.slip_frac),
            };
            return Some((slipped, fill_qty));
        }
        self.fallback_trade_price(symbol, side, qty)
    }

    /// L2: walk the reconstructed book (impact paid, book recovers from later
    /// deltas); falls back to the trade tape when the book is stale.
    fn fill_l2_market(&mut self, symbol: SymbolId, side: Side, qty: f64) -> Option<(f64, f64)> {
        let walked = self.books.get_mut(&symbol).and_then(|b| match side {
            Side::Buy => b.walk_ask(qty),
            Side::Sell => b.walk_bid(qty),
        });
        if let Some((filled_qty, notional)) = walked {
            if filled_qty <= 0.0 {
                return self.fallback_trade_price(symbol, side, qty);
            }
            return Some((notional / filled_qty, filled_qty));
        }
        self.fallback_trade_price(symbol, side, qty)
    }

    fn fallback_trade_price(&self, symbol: SymbolId, side: Side, qty: f64) -> Option<(f64, f64)> {
        let mark = self.latest_mark.get(&symbol).copied()?;
        let px = match side {
            Side::Buy => mark * (1.0 + self.cfg.slip_frac),
            Side::Sell => mark * (1.0 - self.cfg.slip_frac),
        };
        Some((px, qty))
    }

    /// Trade-print rule (SIM-2): a resting limit fills only when a trade
    /// crosses through its price on the opposing aggressor side — a buy limit
    /// needs a sell-side print at/below its price, a sell limit a buy-side
    /// print at/above. Touching (a quote at the price with no print) never
    /// fills. L1 fills the full remaining qty per print; L2 caps each print's
    /// fill at `trade_qty × queue_share` (queue-position realism) — the
    /// remainder walks forward and may fill across several subsequent prints.
    fn try_fill_limit_trade_print(
        &mut self,
        symbol: SymbolId,
        trade_px: f64,
        trade_qty: f64,
        aggr: Side,
        now: i64,
    ) {
        let mut still = Vec::new();
        let ready: Vec<Pending> = std::mem::take(&mut self.pending);
        for p in ready {
            let PendingKind::Limit(limit_px) = p.kind else {
                still.push(p);
                continue;
            };
            let crosses = match (p.side, aggr) {
                (Side::Buy, Side::Sell) => trade_px <= limit_px,
                (Side::Sell, Side::Buy) => trade_px >= limit_px,
                _ => false,
            };
            if p.symbol != symbol || p.ready_ns > now || !crosses {
                still.push(p);
                continue;
            }
            let fill_qty = match self.cfg.fill_model {
                FillModel::L2DepthWalk => p.qty.min(trade_qty * self.cfg.queue_share),
                _ => p.qty, // L1: full remaining qty per print
            };
            if fill_qty <= 0.0 {
                still.push(p);
                continue;
            }
            self.execute_fill(
                symbol,
                p.side,
                limit_px,
                fill_qty,
                p.intent_id,
                Liquidity::Maker,
                true, // optimistic_maker (SIM-12)
            );
            if fill_qty < p.qty {
                still.push(Pending {
                    qty: p.qty - fill_qty,
                    ..p
                }); // remainder needs more prints to fully fill
            }
        }
        self.pending = still;
    }

    /// Apply a fill to accounting, metrics, and the decision log.
    // Each argument is a distinct fill attribute the three fill models produce
    // differently (price/qty/liquidity/optimistic-tag); bundling them into a
    // sub-struct would only move the list, not shorten it.
    #[allow(clippy::too_many_arguments)]
    fn execute_fill(
        &mut self,
        symbol: SymbolId,
        side: Side,
        price: f64,
        qty: f64,
        intent_id: mp_core::IntentId,
        liquidity: Liquidity,
        optimistic_maker: bool,
    ) {
        let notional = price * qty;
        let fee = notional * self.cfg.taker_fee; // "taker-priced maker fee" (SIM-12)
        let signed = match side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };
        let before = self.acct.realized();
        self.acct.apply_fill(symbol, signed, price, fee);
        let realized_delta = self.acct.realized() - before;
        if optimistic_maker {
            self.metrics.record_maker_trade(realized_delta);
        } else {
            self.metrics.record_trade(realized_delta);
        }

        self.seq += 1;
        self.next_intent = self.next_intent.max(intent_id.0);
        self.log.record_fill_tagged(
            self.seq,
            &Fill {
                intent_id,
                symbol,
                side,
                price,
                qty,
                fee,
                liquidity,
                ts_ns: self.clock.now_ns(),
            },
            optimistic_maker,
        );
    }
}
