//! Event-replay backtester (SIM-1, SIM-5). Drives a [`SimClock`] from event
//! timestamps only (no wall time), running the PRODUCTION feature engine,
//! strategy, and sizing crates unmodified — only the clock and fill model
//! differ from live. Market intents fill against the next trade after a
//! configured latency (taker), with fees + slippage on every fill.

use crate::account::Accountant;
use crate::decision_log::DecisionLog;
use crate::metrics::Metrics;
use mp_core::{
    Clock, EventEnvelope, Fill, Liquidity, MarketEvent, OrderIntent, OrderKind, Side, SimClock,
    SizeUnit, SymbolId,
};
use mp_features::FeatureEngine;
use mp_risk::{size, SizingInputs, SizingParams};
use mp_strategies::strategy::{Ctx, TimerId};
use mp_strategies::Strategy;
use std::collections::BTreeMap;

/// Backtester configuration.
#[derive(Debug, Clone, Copy)]
pub struct SimConfig {
    /// Intent→fill latency (SIM-3); 0 only in unit tests.
    pub latency_ns: i64,
    pub taker_fee: f64,
    /// Slippage as a fraction of price.
    pub slip_frac: f64,
    pub start_cash: f64,
    /// Fallback instrument vol fraction when no `vol.rv` feature is present yet.
    pub default_vol_frac: f64,
    pub per_trade_risk_pct: f64,
    pub k_stop: f64,
    pub step_size: f64,
    pub min_notional: f64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            latency_ns: 150_000_000, // 150ms
            taker_fee: 0.00055,
            slip_frac: 0.0001,
            start_cash: 100_000.0,
            default_vol_frac: 0.02,
            per_trade_risk_pct: 0.005,
            k_stop: 1.5,
            step_size: 0.0001,
            min_notional: 5.0,
        }
    }
}

struct Pending {
    symbol: SymbolId,
    side: Side,
    qty: f64,
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
    pub fn identity_residual(&self) -> f64 {
        self.acct.identity_residual()
    }
    pub fn now_ns(&self) -> i64 {
        self.clock.now_ns()
    }

    /// Replay a full event sequence (must be in global recv order).
    pub fn run(&mut self, events: &[EventEnvelope]) {
        for ev in events {
            self.clock.set(ev.recv_ts_ns);
            self.on_event(ev);
            self.metrics.sample_equity(self.acct.equity());
        }
    }

    fn on_event(&mut self, ev: &EventEnvelope) {
        // Marks + funding + pending fills.
        match &ev.body {
            MarketEvent::Trade { price, .. } => {
                self.latest_mark.insert(ev.symbol, *price);
                self.acct.mark(ev.symbol, *price);
                self.fill_pending(ev.symbol, *price, ev.recv_ts_ns);
            }
            MarketEvent::MarkPrice { mark, .. } => {
                self.latest_mark.insert(ev.symbol, *mark);
                self.acct.mark(ev.symbol, *mark);
            }
            MarketEvent::Funding { rate, .. } => {
                self.acct.accrue_funding(ev.symbol, *rate);
            }
            _ => {}
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

    fn enqueue(&mut self, intent: &OrderIntent, now: i64) {
        // Only market intents are modeled in this slice.
        if !matches!(intent.kind, OrderKind::Market) {
            return;
        }
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
            ready_ns: now + self.cfg.latency_ns,
            intent_id: intent.intent_id,
        });
    }

    fn fill_pending(&mut self, symbol: SymbolId, trade_price: f64, now: i64) {
        let mut still = Vec::new();
        let ready: Vec<Pending> = std::mem::take(&mut self.pending);
        for p in ready {
            if p.symbol != symbol || p.ready_ns > now {
                still.push(p);
                continue;
            }
            let px = match p.side {
                Side::Buy => trade_price * (1.0 + self.cfg.slip_frac),
                Side::Sell => trade_price * (1.0 - self.cfg.slip_frac),
            };
            let notional = px * p.qty;
            let fee = notional * self.cfg.taker_fee;
            let signed = match p.side {
                Side::Buy => p.qty,
                Side::Sell => -p.qty,
            };
            let before = self.acct.realized();
            self.acct.apply_fill(symbol, signed, px, fee);
            let realized_delta = self.acct.realized() - before;
            self.metrics.record_trade(realized_delta);

            self.seq += 1;
            self.next_intent = self.next_intent.max(p.intent_id.0);
            self.log.record_fill(
                self.seq,
                &Fill {
                    intent_id: p.intent_id,
                    symbol,
                    side: p.side,
                    price: px,
                    qty: p.qty,
                    fee,
                    liquidity: Liquidity::Taker,
                    ts_ns: now,
                },
            );
        }
        self.pending = still;
    }
}
