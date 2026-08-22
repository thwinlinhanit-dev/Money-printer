//! Event-replay backtester (SIM-1, SIM-5). Thin orchestration: event clock →
//! fills → features → strategy → size/gate → pending. Fill models live in
//! [`crate::fills`]; this module only glues production crates.

use crate::account::Accountant;
use crate::decision_log::DecisionLog;
use crate::error::SimError;
use crate::fills::{FillModel, FillParams, Pending, PendingBook, PendingKind, ProducedFill};
use crate::metrics::{bars_per_year, Metrics};
use mp_core::{
    BookMirror, Clock, EventEnvelope, Fill, IntentId, MarketEvent, OrderIntent, OrderKind, Side,
    SimClock, SizeUnit, SplitMix64, SymbolId, Venue,
};
use mp_features::FeatureEngine;
use mp_risk::{
    evaluate, size, trip_on_breach, GateInput, KillSwitches, Mode, RiskLimits, Scope, SizingInputs,
    SizingParams, TripRequest, Verdict,
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
    /// Fee on maker-priced fills (resting limits, L0 bar fills). Spec 005 cost
    /// model: `fee = notional × fee_rate(venue, maker|taker)` — maker fills
    /// were previously charged the taker fee (audit fix-all 2026-08-17).
    pub maker_fee: f64,
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
            maker_fee: 0.0002,
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
                // SWG-6 (spec 035): simulator-loose breadth/correlation caps,
                // same spirit as the INFINITE loss budgets above — the backtest
                // measures the strategy's edge, not the portfolio thermostat.
                // Live/paper sets finite values in `risk.toml`.
                max_concurrent_positions: 1_000,
                max_corr_adjusted_portfolio: 10_000_000.0,
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
    /// (fire_at_ns, timer_id, owning strategy idx) — attribution lets the
    /// engine defer a bar-close strategy's timer to the next bar boundary
    /// (SWG-7) instead of silently dropping it.
    queued_timers: Vec<(i64, TimerId, usize)>,
    next_timer: u64,
    logs: Vec<String>,
    strat_idx: usize,
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
        self.queued_timers
            .push((self.now + after_ns, id, self.strat_idx));
        id
    }
    fn log(&mut self, msg: &str) {
        self.logs.push(msg.to_string());
    }
}

/// The backtester (orchestration only).
pub struct Backtester {
    fe: FeatureEngine,
    /// Strategies in registration order (deterministic dispatch, PD-3).
    strats: Vec<Box<dyn Strategy>>,
    /// Per-strategy subscription sets, captured at construction (audit C1).
    strat_subs: Vec<Vec<String>>,
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
    /// Per-strategy deterministic RNG (index-aligned with `strats`). Index 0 is
    /// seeded verbatim so a single-strategy run is byte-identical to the
    /// pre-multi-strategy engine; the rest are a deterministic mix.
    strat_rngs: Vec<SplitMix64>,
    seq: u64,
    next_intent: u128,
    pending_timers: Vec<(i64, TimerId, usize)>,
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
    /// SWG-5: last observed bar epoch (`recv_ts_ns / bar_tf_ns`). Equity is
    /// sampled for bar-return Sharpe once per bar boundary (the first event
    /// seeds the baseline without contributing a return).
    last_bar_epoch: i64,
}

impl Backtester {
    /// Single-strategy backtest (backward-compatible: a one-element multi run).
    pub fn new(fe: FeatureEngine, strat: Box<dyn Strategy>, cfg: SimConfig, seed: u64) -> Self {
        Self::from_strategies(fe, vec![strat], cfg, seed)
    }

    /// Multi-strategy backtest (audit 08-04): run several strategies against one
    /// shared simulated account and market feed. Event handlers (feature/fill/
    /// timer) are dispatched to every strategy in registration order with an
    /// isolated, per-strategy Ctx (own RNG, timers, logs) so dispatch stays
    /// deterministic (PD-3). Intent ids are engine-namespaced (record_dispatch)
    /// so a fill is always attributed to the strategy that emitted it. NOTE:
    /// `Ctx::position`/`equity` are the shared simulated account — strategies
    /// in one run observe the same positions.
    pub fn from_strategies(
        fe: FeatureEngine,
        strats: Vec<Box<dyn Strategy>>,
        cfg: SimConfig,
        seed: u64,
    ) -> Self {
        let strat_subs = strats.iter().map(|s| s.subscriptions()).collect();
        let strat_rngs = (0..strats.len())
            .map(|i| Backtester::strategy_seed(seed, i))
            .collect();
        Self {
            fe,
            strats,
            strat_subs,
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
            strat_rngs,
            seq: 0,
            next_intent: 0,
            pending_timers: Vec::new(),
            next_timer_id: 0,
            contract_mult: BTreeMap::new(),
            funding_count: BTreeMap::new(),
            hold_start: BTreeMap::new(),
            intent_strategy: BTreeMap::new(),
            last_bar_epoch: i64::MIN,
        }
    }

    /// Deterministic per-strategy seed. Index 0 gets the run seed verbatim, so a
    /// single-strategy run consumes randomness exactly as the legacy engine did;
    /// additional strategies derive a fixed, index-dependent mix.
    fn strategy_seed(seed: u64, idx: usize) -> SplitMix64 {
        if idx == 0 {
            SplitMix64::new(seed)
        } else {
            SplitMix64::new(seed.wrapping_add((idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)))
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
            sharpe: self.metrics.sharpe(bars_per_year(self.cfg.bar_tf_ns)),
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
        self.stream(events.iter().cloned());
        self.check_funding_coverage()
    }

    /// Streaming event loop (paper mode, SIM-15/EXE-8). Feed events exactly as
    /// they arrive from a live/recorded feed; identical input sequences
    /// produce identical decision logs whether fed as one slice (replay) or in
    /// many batches (paper tail) — that identity IS the G3 paper-vs-sim
    /// comparison primitive. The run-end guards (SIM-4 funding, SIM-6
    /// coverage) are the caller's `check_funding` responsibility for open
    /// sessions; a closed replay should use `run_checked`.
    pub fn stream(&mut self, events: impl IntoIterator<Item = EventEnvelope>) {
        for ev in events {
            self.run_start_ns.get_or_insert(ev.recv_ts_ns);
            self.clock.set(ev.recv_ts_ns);
            self.on_event(&ev);
            self.metrics.sample_equity(self.acct.equity());
            // SWG-5: sample equity once per bar boundary for bar-return Sharpe.
            // Uses the same bar timeframe as the fill model so a daily/4h bar
            // replay reports a daily/4h Sharpe, not an event-count one.
            let bar_tf = self.cfg.bar_tf_ns.max(1);
            let epoch = ev.recv_ts_ns.div_euclid(bar_tf);
            if epoch != self.last_bar_epoch {
                self.last_bar_epoch = epoch;
                self.metrics.record_bar_return(self.acct.equity());
            }
        }
    }

    /// SIM-4 run-end guard, callable by paper sessions on close: refuses to
    /// certify a session whose held perp positions crossed a funding boundary
    /// without a recorded Funding event.
    pub fn check_funding(&self) -> Result<(), SimError> {
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
            MarketEvent::Trade {
                price, qty, side, ..
            }
            | MarketEvent::TradeWithAddr {
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
            MarketEvent::Funding {
                rate, interval_s, ..
            } => {
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

        if !matches!(
            ev.body,
            MarketEvent::Trade { .. } | MarketEvent::TradeWithAddr { .. }
        ) && self.cfg.fill_model != FillModel::L0BarFill
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
        // SWG-7: bar-boundary signal, same bar timeframe as the SWG-5 equity
        // sampling (`last_bar_epoch` still holds the PREVIOUS event's epoch
        // here — `stream()` updates it after `on_event`). Strategies with a
        // bar-close cadence (`Daily`/`FourHour`) may act ONLY on bar close.
        let at_bar_boundary = now.div_euclid(self.cfg.bar_tf_ns.max(1)) != self.last_bar_epoch;
        self.fire_timers(now, at_bar_boundary);

        let ups = self.fe.on_event(ev);
        for u in ups {
            self.seq += 1;
            self.log.record_feature(self.seq, &u);
            let feat_name = self.fe.resolve_name(u.feature).to_owned();
            if feat_name.starts_with("vol.rv") {
                self.latest_vol.insert(u.symbol, u.value);
            }
            // audit C1: dispatch each feature update only to the strategies whose
            // subscription set matches it (each strategy also self-checks its
            // feature name). Iteration order is registration order — deterministic
            // under an injected clock (PD-3).
            for i in 0..self.strats.len() {
                if self.strats[i].rebalance_cadence().is_bar_close() && !at_bar_boundary {
                    // SWG-7: a swing strategy evaluates on bar close ONLY.
                    continue;
                }
                if !self.subscribed(i, &feat_name) {
                    continue;
                }
                let (mut intents, logs) = self.dispatch_one(now, i, |s, ctx| s.on_feature(&u, ctx));
                self.record_dispatch(now, &mut intents, &logs);
            }
        }
    }

    /// Validate + record a dispatch result, threading strategy rationale lines
    /// into the decision log ahead of the intents they explain.
    fn record_dispatch(&mut self, now: i64, intents: &mut [OrderIntent], logs: &[String]) {
        for msg in logs {
            self.seq += 1;
            self.log.record_log(self.seq, msg);
        }
        for intent in intents.iter_mut() {
            // audit 08-04 / H2: the engine owns the intent-id space. Re-stamp
            // every emitted intent with a globally-unique id so two strategies
            // minting the same local id (both start at 1,2,..) can never
            // collide or misattribute a fill. Deterministic: ids come from the
            // same injected dispatch order every run.
            intent.intent_id = IntentId(self.next_intent);
            self.next_intent += 1;
            // Attribute the (now-unique) id back to its strategy up front, so a
            // fill can always be traced even if the order is later rejected.
            self.intent_strategy
                .insert(intent.intent_id.0, intent.strategy.0.clone());
            self.seq += 1;
            self.log.record_intent(self.seq, intent);
            self.enqueue(intent, now);
        }
    }

    fn fire_timers(&mut self, now: i64, at_bar_boundary: bool) {
        self.pending_timers.sort_by_key(|&(t, _, _)| t);
        let mut still = Vec::new();
        let mut fired = Vec::new();
        for (t, id, owner) in self.pending_timers.drain(..) {
            // At-or-after semantics: a timer scheduled for `now` fires at this
            // event, not the next one (audit fix-all 2026-08-17: `t < now` was
            // an off-by-one that delayed every exact-time timer by one event).
            if t <= now {
                fired.push((t, id, owner));
            } else {
                still.push((t, id, owner));
            }
        }
        self.pending_timers = still;
        let mut deferred = Vec::new();
        for (_, id, owner) in fired {
            if self.strats[owner].rebalance_cadence().is_bar_close() && !at_bar_boundary {
                // SWG-7: a bar-close-cadence strategy's timer fired mid-bar —
                // an execution decision is a bar decision, so defer it to the
                // next bar boundary. NEVER silently dropped.
                let bar_tf = self.cfg.bar_tf_ns.max(1);
                let next_boundary = now.div_euclid(bar_tf) * bar_tf + bar_tf;
                deferred.push((next_boundary, id, owner));
                continue;
            }
            // Dispatch the fired timer to every strategy; each only reacts to
            // its own opaque TimerIds, so there is no cross-strategy coupling.
            let (mut intents, logs) = self.dispatch_all(now, |s, ctx| s.on_timer(id, ctx));
            self.record_dispatch(now, &mut intents, &logs);
        }
        self.pending_timers.extend(deferred);
    }

    /// Deterministic, per-strategy dispatch of one event handler. Builds an
    /// isolated `Ctx` (own RNG, timers, log lines), runs `f` on strategy `idx`,
    /// and folds that strategy's RNG/timers/logs back into the engine.
    fn dispatch_one(
        &mut self,
        now: i64,
        idx: usize,
        f: impl Fn(&mut dyn Strategy, &mut dyn Ctx) -> Vec<OrderIntent>,
    ) -> (Vec<OrderIntent>, Vec<String>) {
        let mut ctx = SimCtx {
            now,
            equity: self.acct.equity(),
            positions: self.acct.positions(),
            rng: self.strat_rngs[idx].clone(),
            queued_timers: Vec::new(),
            next_timer: self.next_timer_id,
            logs: Vec::new(),
            strat_idx: idx,
        };
        let intents = f(self.strats[idx].as_mut(), &mut ctx);
        self.strat_rngs[idx] = SplitMix64::from_state(ctx.rng.state());
        self.next_timer_id = ctx.next_timer;
        self.pending_timers.extend(ctx.queued_timers);
        (intents, ctx.logs)
    }

    /// Dispatch an event handler to every strategy, in registration order,
    /// folding their intents + log lines deterministically (multi-strategy
    /// support, audit 08-04). Strategies that don't care return no intents;
    /// emitted intents are namespaced by `record_dispatch`, so each fill stays
    /// attributed to the strategy that produced it.
    fn dispatch_all(
        &mut self,
        now: i64,
        f: impl Fn(&mut dyn Strategy, &mut dyn Ctx) -> Vec<OrderIntent>,
    ) -> (Vec<OrderIntent>, Vec<String>) {
        let mut all = Vec::new();
        let mut logs = Vec::new();
        for i in 0..self.strats.len() {
            let (ints, ls) = self.dispatch_one(now, i, &f);
            all.extend(ints);
            logs.extend(ls);
        }
        (all, logs)
    }

    /// Whether strategy `idx` subscribes to `name` (its subscription set includes
    /// `"*"` or a prefix of `name`, audit C1).
    fn subscribed(&self, idx: usize, name: &str) -> bool {
        self.strat_subs[idx]
            .iter()
            .any(|s| s == "*" || name.starts_with(s.as_str()))
    }

    fn on_trade(&mut self, symbol: SymbolId, price: f64, qty: f64, side: Side, now: i64) {
        let params = self.fill_params();
        let fills = match self.cfg.fill_model {
            FillModel::L0BarFill => self
                .pending
                .on_trade_l0(params, symbol, price, qty, side, now),
            FillModel::L1TopOfBook | FillModel::L2DepthWalk => {
                let mut out = self
                    .pending
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
            self.log
                .record_invalid_intent(self.seq, intent.intent_id, &e.to_string());
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
        // SWG-6 RG-12: distinct symbols currently holding a position (breadth
        // slots). The gate counts only orders that OPEN a new slot.
        let open_positions = self
            .acct
            .positions()
            .values()
            .filter(|q| q.abs() > 0.0)
            .count() as u32;
        // SWG-6 RG-13: correlation-adjusted exposure AFTER this order fills.
        // The sim has no cross-asset correlation model (fixtures are
        // single-asset), so it assumes the worst case — perfect correlation —
        // under which the correlation-adjusted value equals the resulting gross
        // notional (the same number RG-5 already guards). Live/paper callers
        // with a correlation matrix use `mp_risk::correlation_adjusted_exposure`.
        let mut resulting: BTreeMap<SymbolId, f64> = self.acct.positions().clone();
        let signed = match intent.side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };
        *resulting.entry(intent.symbol).or_insert(0.0) += signed;
        let corr_adjusted_exposure_notional: f64 = resulting
            .iter()
            .map(|(s, q)| {
                let m = self.contract_mult.get(s).copied().unwrap_or(1.0);
                q.abs() * self.latest_mark.get(s).copied().unwrap_or(0.0) * m
            })
            .sum();
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
            open_positions,
            corr_adjusted_exposure_notional,
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
    }

    fn apply_produced_fill(&mut self, p: ProducedFill, now: i64) {
        let notional = p.price * p.qty;
        // Spec 005 cost model: fee = notional × fee_rate(maker|taker) — maker
        // fills are no longer charged the taker fee (audit fix-all 2026-08-17).
        let fee = notional
            * match p.liquidity {
                mp_core::Liquidity::Maker => self.cfg.maker_fee,
                mp_core::Liquidity::Taker => self.cfg.taker_fee,
            };
        let signed = match p.side {
            Side::Buy => p.qty,
            Side::Sell => -p.qty,
        };
        let outcome = self
            .acct
            .apply_fill(p.symbol, signed, p.price, fee, p.optimism);
        let pos = self.acct.position(p.symbol);
        if pos != 0.0 {
            self.hold_start
                .entry(p.symbol)
                .or_insert(self.clock.now_ns());
        } else {
            self.hold_start.remove(&p.symbol);
        }
        // Per-trade NET P&L = gross − attributed fees − attributed funding
        // (audit fix-all 2026-08-17: funding was invisible to expectancy —
        // a carry edge paid out entirely in funding scored as flat).
        let net = outcome.realized_gross - outcome.attributed_fees - outcome.attributed_funding;
        if outcome.closed_qty > 0.0 {
            // Trade-level tag = worst-case across the position's legs, so a
            // maker-optimistic entry is never re-tagged by a conservative
            // exit (B4: the G1 gate must see entry-leg optimism).
            self.metrics
                .record_trade_with_optimism(net, outcome.optimism);
            self.trade_pnls.push((self.clock.now_ns(), net));
        }

        self.seq += 1;
        let strategy = self
            .intent_strategy
            .get(&p.intent_id.0)
            .map(String::as_str)
            .unwrap_or("");
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
        self.log
            .record_fill_tagged(self.seq, &fill, p.optimism, strategy, p.venue);

        let (mut follow, logs) = self.dispatch_all(now, |s, ctx| s.on_fill(&fill, ctx));
        self.record_dispatch(now, &mut follow, &logs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_core::StrategyId;
    use mp_strategies::strategy::RegimeMask;
    use mp_strategies::Universe;

    /// A strategy that never emits anything — only needed to construct a Backtester.
    struct Noop;
    impl Strategy for Noop {
        fn id(&self) -> StrategyId {
            StrategyId::new("noop")
        }
        fn universe(&self) -> Universe {
            Universe::default()
        }
        fn subscriptions(&self) -> Vec<String> {
            Vec::new()
        }
        fn warmup_ns(&self) -> i64 {
            0
        }
        fn declared_regime(&self) -> RegimeMask {
            RegimeMask::any()
        }
        fn on_feature(
            &mut self,
            _: &mp_features::FeatureUpdate,
            _: &mut dyn Ctx,
        ) -> Vec<OrderIntent> {
            Vec::new()
        }
        fn with_params(&self, _: &std::collections::BTreeMap<String, f64>) -> Box<dyn Strategy> {
            Box::new(Noop)
        }
    }

    // audit 08-04 / H2: the engine owns the intent-id space. Two strategies that
    // both mint local id 7 (strategies all start their counters at 1,2,..) must
    // receive distinct global ids, and each id must trace back to the strategy
    // that emitted it — so a multi-strategy run can never misattribute a fill.
    #[test]
    fn audit_h2_engine_namespaces_intent_ids_across_strategies() {
        let mut bt = Backtester::new(
            FeatureEngine::new(1),
            Box::new(Noop),
            SimConfig::default(),
            1,
        );
        let intent = |strategy: &str, side: Side| OrderIntent {
            intent_id: IntentId(7), // same local id from both strategies
            strategy: StrategyId::new(strategy),
            venue: Venue::Bybit,
            symbol: SymbolId(0),
            side,
            kind: OrderKind::Market,
            qty: SizeUnit::Contracts(1.0),
            tif: mp_core::TimeInForce::Ioc,
            reduce_only: false,
            tag: "namespaced".into(),
        };
        let mut intents = vec![
            intent("carry-v1", Side::Buy),
            intent("liq-fade", Side::Sell),
        ];
        bt.record_dispatch(0, &mut intents, &[]);

        // Both local id-7 intents were re-stamped with unique global ids.
        assert_ne!(intents[0].intent_id, intents[1].intent_id);
        assert_eq!(intents[0].intent_id, IntentId(0));
        assert_eq!(intents[1].intent_id, IntentId(1));
        assert_eq!(bt.next_intent, 2);

        // Attribution maps each global id to exactly its own strategy.
        assert_eq!(bt.intent_strategy[&intents[0].intent_id.0], "carry-v1");
        assert_eq!(bt.intent_strategy[&intents[1].intent_id.0], "liq-fade");
    }
}
