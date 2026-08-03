//! Event-replay backtester (SIM-1, SIM-5). Thin orchestration: event clock →
//! fills → features → strategy → size/gate → pending. Fill models live in
//! [`crate::fills`]; this module only glues production crates.

use crate::account::Accountant;
use crate::decision_log::DecisionLog;
use crate::error::SimError;
use crate::fills::{FillModel, FillParams, Pending, PendingBook, PendingKind, ProducedFill};
use crate::metrics::Metrics;
use mp_core::{
    BookMirror, Clock, EventEnvelope, Fill, MarketEvent, OrderIntent, OrderKind, Side, SimClock,
    SizeUnit, SplitMix64, SymbolId, Venue,
};
use mp_features::FeatureEngine;
use mp_risk::{
    evaluate, size, trip_on_breach, GateInput, KillSwitches, Mode, RiskLimits, Scope,
    SizingInputs, SizingParams, TripRequest, Verdict,
};
use mp_strategies::strategy::{Ctx, TimerId};
use mp_strategies::Strategy;
use std::collections::BTreeMap;

/// Backtester configuration.
#[derive(Debug, Clone, Copy)]
pub struct SimConfig {
    pub fill_model: FillModel,
    pub latency_ns: i64,
    pub taker_fee: f64,
    pub slip_frac: f64,
    pub participation: f64,
    pub queue_share: f64,
    pub bar_tf_ns: i64,
    pub start_cash: f64,
    pub default_vol_frac: f64,
    pub per_trade_risk_pct: f64,
    pub k_stop: f64,
    pub step_size: f64,
    pub min_notional: f64,
    pub min_coverage: f64,
    pub funding_check_interval_ns: i64,
    pub limits: RiskLimits,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            fill_model: FillModel::default(),
            latency_ns: 150_000_000,
            taker_fee: 0.00055,
            slip_frac: 0.0001,
            participation: 0.5,
            queue_share: 0.25,
            bar_tf_ns: 60_000_000_000,
            start_cash: 100_000.0,
            default_vol_frac: 0.02,
            per_trade_risk_pct: 0.005,
            k_stop: 1.5,
            step_size: 0.0001,
            min_notional: 5.0,
            min_coverage: 0.995,
            funding_check_interval_ns: 28_800_000_000_000,
            limits: RiskLimits {
                max_order_notional: 1_000_000.0,
                max_position_notional: 5_000_000.0,
                max_gross_portfolio: 10_000_000.0,
                max_px_dev_frac: 0.05,
                max_orders_per_min: 1_000,
                // INFINITE budgets are SIMULATOR-ONLY. They keep the RG-8/9
                // daily-loss gate inert so a backtest measures the strategy's edge,
                // not where it would have been halted. They are NOT a live/
                // executable default — live/paper configs must set finite budgets in
                // `risk.toml` (the gate's own `RiskLimits::default()` is finite).
                strategy_daily_loss_budget: f64::INFINITY,
                portfolio_daily_loss_budget: f64::INFINITY,
            },
        }
    }
}

/// Deterministic per-event context handed to the strategy.
struct SimCtx {
    now: i64,
    equity: f64,
    positions: BTreeMap<SymbolId, f64>,
    rng: SplitMix64,
    queued_timers: Vec<(i64, TimerId)>,
    next_timer: u64,
    logs: Vec<String>,
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
        self.rng.next_u64()
    }
    fn set_timer(&mut self, after_ns: i64) -> TimerId {
        self.next_timer += 1;
        let id = TimerId(self.next_timer);
        self.queued_timers.push((self.now + after_ns, id));
        id
    }
    fn log(&mut self, msg: &str) {
        self.logs.push(msg.to_string());
    }
}

/// The backtester (orchestration only).
pub struct Backtester {
    fe: FeatureEngine,
    strat: Box<dyn Strategy>,
    acct: Accountant,
    clock: SimClock,
    cfg: SimConfig,
    log: DecisionLog,
    metrics: Metrics,
    pending: PendingBook,
    latest_vol: BTreeMap<SymbolId, f64>,
    latest_mark: BTreeMap<SymbolId, f64>,
    books: BTreeMap<SymbolId, BookMirror>,
    run_start_ns: Option<i64>,
    trade_pnls: Vec<(i64, f64)>,
    kills: KillSwitches,
    allowed: Vec<(Venue, SymbolId)>,
    intent_ts: Vec<i64>,
    day_start: Option<(i64, f64)>,
    rng: SplitMix64,
    seq: u64,
    next_intent: u128,
    pending_timers: Vec<(i64, TimerId)>,
    next_timer_id: u64,
    /// Venue contract multiplier ($ notional per contract) per symbol; 1.0 = spot/linear.
    contract_mult: BTreeMap<SymbolId, f64>,
    /// Funding events observed per symbol (counted per held interval, SIM-4).
    funding_count: BTreeMap<SymbolId, u32>,
    /// Earliest time each symbol became (nonzero) long/short during the run.
    /// Drives SIM-4 strictness: a required funding tick per funding boundary
    /// crossed while held, so a month-long hold demands ~90 ticks and a
    /// 6h→9h window (old code silently skipped) demands its boundary tick.
    hold_start: BTreeMap<SymbolId, i64>,
    /// Strategy that produced each pending intent (fill log attribution, audit H2).
    intent_strategy: BTreeMap<u128, String>,
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
            pending: PendingBook::default(),
            latest_vol: BTreeMap::new(),
            latest_mark: BTreeMap::new(),
            books: BTreeMap::new(),
            run_start_ns: None,
            trade_pnls: Vec::new(),
            kills: KillSwitches::new(),
            allowed: Vec::new(),
            intent_ts: Vec::new(),
            day_start: None,
            rng: SplitMix64::new(seed),
            seq: 0,
            next_intent: 0,
            pending_timers: Vec::new(),
            next_timer_id: 0,
            contract_mult: BTreeMap::new(),
            funding_count: BTreeMap::new(),
            hold_start: BTreeMap::new(),
            intent_strategy: BTreeMap::new(),
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
    pub fn stress_expectancy_2x(&self) -> f64 {
        self.metrics.stress_expectancy(self.acct.fees(), 2.0)
    }
    pub fn fees(&self) -> f64 {
        self.acct.fees()
    }
    pub fn trade_pnls(&self) -> &[(i64, f64)] {
        &self.trade_pnls
    }
    pub fn summary(&self) -> crate::harness::MetricsSummary {
        crate::harness::MetricsSummary {
            trades: self.metrics.trades,
            expectancy: self.metrics.expectancy(),
            stress_expectancy_2x: self.stress_expectancy_2x(),
            max_drawdown: self.metrics.max_drawdown,
        }
    }

    pub fn run(&mut self, events: &[EventEnvelope]) -> Result<(), SimError> {
        self.run_checked(events, 1.0)
    }

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
        let _ = start;
        let now = self.clock.now_ns();
        let interval = self.cfg.funding_check_interval_ns.max(1);
        // SIM-4 strictness (audit H2): the guard counts funding boundaries
        // crossed while a perp was held, per symbol — not run duration and not
        // "one tick ever". A month-long hold demands ~90 ticks; a run that
        // starts mid-window and holds past a boundary (e.g. 6h→9h) demands its
        // boundary tick even though it is shorter than one interval. Runs whose
        // holds cross no boundary (e.g. 4s micro-tests) owe no funding yet and
        // legitimately require no ticks — real 8h tick data cannot appear in a
        // sub-interval window.
        for (symbol, open) in &self.hold_start {
            let required = (now.div_euclid(interval) - open.div_euclid(interval)) as u32;
            let count = self.funding_count.get(symbol).copied().unwrap_or(0);
            if count < required {
                return Err(SimError::MissingFunding(*symbol));
            }
        }
        Ok(())
    }

    pub fn kill_switches_mut(&mut self) -> &mut KillSwitches {
        &mut self.kills
    }

    fn fill_params(&self) -> FillParams {
        FillParams {
            model: self.cfg.fill_model,
            slip_frac: self.cfg.slip_frac,
            participation: self.cfg.participation,
            queue_share: self.cfg.queue_share,
            bar_tf_ns: self.cfg.bar_tf_ns,
        }
    }

    fn on_event(&mut self, ev: &EventEnvelope) {
        if !self.allowed.contains(&(ev.venue, ev.symbol)) {
            self.allowed.push((ev.venue, ev.symbol));
        }
        let day = ev.recv_ts_ns.div_euclid(86_400_000_000_000);
        match self.day_start {
            Some((d, _)) if d == day => {}
            _ => self.day_start = Some((day, self.acct.equity())),
        }

        self.books.entry(ev.symbol).or_default().apply(&ev.body);

        match &ev.body {
            MarketEvent::Trade { price, qty, side, .. } => {
                self.latest_mark.insert(ev.symbol, *price);
                self.acct.mark(ev.symbol, *price);
                self.on_trade(ev.symbol, *price, *qty, *side, ev.recv_ts_ns);
            }
            MarketEvent::MarkPrice { mark, .. } => {
                self.latest_mark.insert(ev.symbol, *mark);
                self.acct.mark(ev.symbol, *mark);
            }
            MarketEvent::Funding { rate, interval_s, .. } => {
                *self.funding_count.entry(ev.symbol).or_insert(0) += 1;
                self.acct.accrue_funding(ev.symbol, *rate);
                let _ = interval_s;
            }
            MarketEvent::OpenInterest { .. } => {}
            _ => {}
        }
        // Mark = book mid when one is trustworthy (audit H2: a raw trade print,
        // especially a stop-hunt wick, previously marked all positions; the mid
        // is far less wick-sensitive). Falls back to the latest trade/mark event.
        if let Some(mid) = self.books.get(&ev.symbol).and_then(BookMirror::mid) {
            self.latest_mark.insert(ev.symbol, mid);
            self.acct.mark(ev.symbol, mid);
        }

        if !matches!(ev.body, MarketEvent::Trade { .. })
            && self.cfg.fill_model != FillModel::L0BarFill
        {
            let params = self.fill_params();
            let fills = self.pending.try_fill_market(
                params,
                &mut self.books,
                &self.latest_mark,
                ev.symbol,
                ev.recv_ts_ns,
            );
            for f in fills {
                self.apply_produced_fill(f, ev.recv_ts_ns);
            }
        }

        let now = ev.recv_ts_ns;
        self.fire_timers(now);

        let subs = self.strat.subscriptions();
        let subscribed = |name: &str| subs.iter().any(|s| s == "*" || name.starts_with(s.as_str()));

        let ups = self.fe.on_event(ev);
        for u in ups {
            self.seq += 1;
            self.log.record_feature(self.seq, &u);
            let feat_name = self.fe.resolve_name(u.feature);
            if feat_name.starts_with("vol.rv") {
                self.latest_vol.insert(u.symbol, u.value);
            }
            // Honor the strategy's subscriptions (audit C1: a funding-only
            // strategy acting on a CVD update as if it were funding is a
            // wrong-signal bug; the sim dispatches every feature update unless
            // filtered here, and the strategy can't resolve feature ids).
            if !subscribed(feat_name) {
                continue;
            }
            let (intents, logs) = self.dispatch_strategy(now, |s, ctx| s.on_feature(&u, ctx));
            self.record_dispatch(now, &intents, &logs);
        }
    }

    /// Validate + record a dispatch result, threading strategy rationale lines
    /// into the decision log ahead of the intents they explain.
    fn record_dispatch(&mut self, now: i64, intents: &[OrderIntent], logs: &[String]) {
        for msg in logs {
            self.seq += 1;
            self.log.record_log(self.seq, msg);
        }
        for intent in intents {
            self.seq += 1;
            self.log.record_intent(self.seq, intent);
            self.enqueue(intent, now);
        }
    }

    fn fire_timers(&mut self, now: i64) {
        self.pending_timers.sort_by_key(|&(t, _)| t);
        let mut still = Vec::new();
        let mut fired = Vec::new();
        for (t, id) in self.pending_timers.drain(..) {
            if t < now {
                fired.push(id);
            } else {
                still.push((t, id));
            }
        }
        self.pending_timers = still;
        for timer_id in fired {
            let (intents, logs) = self.dispatch_strategy(now, |s, ctx| s.on_timer(timer_id, ctx));
            self.record_dispatch(now, &intents, &logs);
        }
    }

    /// Single strategy dispatch path for on_feature / on_fill / on_timer.
    /// Returns (intents, strategy log lines). Log lines are routed through the
    /// decision log so a strategy's rationale is replayable (audit H3).
    fn dispatch_strategy(
        &mut self,
        now: i64,
        f: impl FnOnce(&mut dyn Strategy, &mut dyn Ctx) -> Vec<OrderIntent>,
    ) -> (Vec<OrderIntent>, Vec<String>) {
        let mut ctx = SimCtx {
            now,
            equity: self.acct.equity(),
            positions: self.acct.positions(),
            rng: SplitMix64::from_state(self.rng.state()),
            queued_timers: Vec::new(),
            next_timer: self.next_timer_id,
            logs: Vec::new(),
        };
        let intents = f(self.strat.as_mut(), &mut ctx);
        self.rng = SplitMix64::from_state(ctx.rng.state());
        self.next_timer_id = ctx.next_timer;
        self.pending_timers.extend(ctx.queued_timers);
        (intents, ctx.logs)
    }

    fn on_trade(&mut self, symbol: SymbolId, price: f64, qty: f64, side: Side, now: i64) {
        let params = self.fill_params();
        let fills = match self.cfg.fill_model {
            FillModel::L0BarFill => self
                .pending
                .on_trade_l0(params, symbol, price, qty, side, now),
            FillModel::L1TopOfBook | FillModel::L2DepthWalk => {
                let mut out =
                    self.pending
                        .try_fill_limit_trade_print(params, symbol, price, qty, side, now);
                out.extend(self.pending.try_fill_market(
                    params,
                    &mut self.books,
                    &self.latest_mark,
                    symbol,
                    now,
                ));
                out
            }
        };
        for f in fills {
            self.apply_produced_fill(f, now);
        }
    }

    fn enqueue(&mut self, intent: &OrderIntent, now: i64) {
        // CONV-8 / audit C2: reject structurally-invalid intents before they
        // can reach the gate or a fill model. NaN/negative sizes and non-finite
        // limit prices are strategy bugs and must not become executed orders.
        if let Err(e) = intent.validate() {
            self.seq += 1;
            self.log.record_invalid_intent(self.seq, intent.intent_id, &e.to_string());
            return;
        }
        let kind = match intent.kind {
            OrderKind::Market => PendingKind::Market,
            OrderKind::Limit { px } => PendingKind::Limit(px),
            OrderKind::Cancel { .. } => return,
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
                size(
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
                        contract_multiplier: self
                            .contract_mult
                            .get(&intent.symbol)
                            .copied()
                            .unwrap_or(1.0),
                    },
                )
                .qty_contracts
            }
        };
        if qty <= 0.0 {
            return;
        }

        self.intent_ts.retain(|&t| now - t < 60_000_000_000);
        let price = match kind {
            PendingKind::Limit(px) => px,
            PendingKind::Market => mark,
        };
        let day_start_equity = self
            .day_start
            .map(|(_, e)| e)
            .unwrap_or(self.cfg.start_cash);
        let daily_pnl = self.acct.equity() - day_start_equity;
        let gross: f64 = self
            .acct
            .positions()
            .iter()
            .map(|(s, q)| q.abs() * self.latest_mark.get(s).copied().unwrap_or(0.0))
            .sum();
        let mult = self
            .contract_mult
            .get(&intent.symbol)
            .copied()
            .unwrap_or(1.0);
        let gate_input = GateInput {
            mode: Mode::Paper,
            venue: intent.venue,
            symbol: intent.symbol,
            strategy: intent.strategy.clone(),
            side: intent.side,
            qty,
            price,
            mark,
            current_position_qty: self.acct.position(intent.symbol),
            gross_exposure_notional: gross,
            orders_last_min: self.intent_ts.len() as u32,
            strategy_daily_pnl: daily_pnl,
            portfolio_daily_pnl: daily_pnl,
            reconciler_clean: true,
            reduce_only: intent.reduce_only,
            contract_multiplier: mult,
            allowed: &self.allowed,
        };
        let verdict = evaluate(&self.cfg.limits, &self.kills, &gate_input);
        self.intent_ts.push(now);
        self.seq += 1;
        self.log.record_verdict(self.seq, intent.intent_id, verdict);
        // EXE-1/spec 007: an RG-8/9 daily-loss breach must additionally trip the
        // matching kill switch — fail-closed, one-way, until a human resets.
        if let Some(req) = trip_on_breach(verdict, &gate_input) {
            match req {
                TripRequest::Strategy(s) => self.kills.trip(Scope::Strategy(s)),
                TripRequest::Global => self.kills.trip(Scope::Global),
            }
        }
        if !matches!(verdict, Verdict::Pass) {
            return;
        }

        self.pending.push(Pending {
            symbol: intent.symbol,
            venue: intent.venue,
            side: intent.side,
            qty,
            kind,
            ready_ns: now + self.cfg.latency_ns,
            intent_id: intent.intent_id,
        });
        self.intent_strategy
            .insert(intent.intent_id.0, intent.strategy.0.clone());
    }

    fn apply_produced_fill(&mut self, p: ProducedFill, now: i64) {
        let notional = p.price * p.qty;
        let fee = notional * self.cfg.taker_fee;
        let signed = match p.side {
            Side::Buy => p.qty,
            Side::Sell => -p.qty,
        };
        let outcome = self.acct.apply_fill(p.symbol, signed, p.price, fee);
        let pos = self.acct.position(p.symbol);
        if pos != 0.0 {
            self.hold_start.entry(p.symbol).or_insert(self.clock.now_ns());
        } else {
            self.hold_start.remove(&p.symbol);
        }
        let net = outcome.realized_gross - outcome.attributed_fees;
        if outcome.closed_qty > 0.0 {
            self.metrics.record_trade_with_optimism(net, p.optimism);
            self.trade_pnls.push((self.clock.now_ns(), net));
        }

        self.seq += 1;
        self.next_intent = self.next_intent.max(p.intent_id.0);
        let strategy = self.intent_strategy.get(&p.intent_id.0).map(String::as_str).unwrap_or("");
        let fill = Fill {
            intent_id: p.intent_id,
            symbol: p.symbol,
            side: p.side,
            price: p.price,
            qty: p.qty,
            fee,
            liquidity: p.liquidity,
            ts_ns: self.clock.now_ns(),
        };
        self.log.record_fill_tagged(self.seq, &fill, p.optimism, strategy, p.venue);

        let (follow, logs) = self.dispatch_strategy(now, |s, ctx| s.on_fill(&fill, ctx));
        self.record_dispatch(now, &follow, &logs);
    }
}
