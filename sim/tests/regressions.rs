//! Regression tests from the 2026-07-11 honesty audit (PD-5). Each pins a bug
//! that made reported numbers rosier than reality.

use mp_core::{
    EventEnvelope, IntentId, MarketEvent, OrderIntent, OrderKind, Side, SizeUnit, StrategyId,
    SymbolId, TimeInForce, Venue,
};
use mp_features::catalog::Cvd;
use mp_features::{FeatureEngine, FeatureUpdate};
use mp_sim::{Backtester, FillModel, SimConfig};
use mp_strategies::{Ctx, RegimeMask, Strategy, Universe};

const MS: i64 = 1_000_000;
const SYM: SymbolId = SymbolId(0);

fn trade(recv: i64, price: f64, qty: f64, side: Side) -> EventEnvelope {
    EventEnvelope::new(
        Venue::Bybit,
        SYM,
        recv,
        recv,
        recv as u64,
        MarketEvent::Trade {
            price,
            qty,
            side,
            trade_id: recv as u64,
        },
    )
}

fn engine() -> FeatureEngine {
    let mut e = FeatureEngine::new(1_000_000_000);
    e.register_tick(|| Box::new(Cvd::new("bybit")));
    e
}

/// Emits a fixed script of intents, one per feature update.
struct Scripted {
    script: Vec<(OrderKind, Side, f64)>,
    next: usize,
}

impl Strategy for Scripted {
    fn id(&self) -> StrategyId {
        StrategyId::new("scripted")
    }
    fn universe(&self) -> Universe {
        Universe::default()
    }
    fn subscriptions(&self) -> Vec<String> {
        vec!["cvd.bybit".to_string()]
    }
    fn warmup_ns(&self) -> i64 {
        0
    }
    fn declared_regime(&self) -> RegimeMask {
        RegimeMask::any()
    }
    fn on_feature(&mut self, u: &FeatureUpdate, _ctx: &mut dyn Ctx) -> Vec<OrderIntent> {
        let Some(&(kind, side, qty)) = self.script.get(self.next) else {
            return Vec::new();
        };
        self.next += 1;
        vec![OrderIntent {
            intent_id: IntentId(self.next as u128),
            strategy: self.id(),
            venue: Venue::Bybit,
            symbol: u.symbol,
            side,
            kind,
            qty: SizeUnit::Contracts(qty),
            tif: TimeInForce::Ioc,
            reduce_only: false,
            tag: "scripted".into(),
        }]
    }
}

/// Audit bug 1: expectancy was computed GROSS of fees while the metrics doc
/// and spec claimed "after costs" — and the 2x-cost stress column therefore
/// only priced 1x. Round trip: buy 1 (fills @105, fee 1.05), sell 1
/// (fills @110, fee 1.10). Gross realized = 5.0; honest net = 5 − 2.15 = 2.85.
#[test]
fn regression_audit1_expectancy_is_net_of_entry_and_exit_fees() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(Scripted {
            script: vec![
                (OrderKind::Market, Side::Buy, 1.0),
                (OrderKind::Market, Side::Sell, 1.0),
            ],
            next: 0,
        }),
        SimConfig {
            fill_model: FillModel::L1TopOfBook,
            latency_ns: 0,
            slip_frac: 0.0,
            taker_fee: 0.01, // 1% for round numbers
            ..SimConfig::default()
        },
        1,
    );
    // t0: buy intent; t1 (105): buy fills, sell intent; t2 (110): sell fills.
    bt.run(&[
        trade(0, 100.0, 1.0, Side::Buy),
        trade(MS, 105.0, 1.0, Side::Buy),
        trade(2 * MS, 110.0, 1.0, Side::Buy),
    ])
    .unwrap();

    let m = bt.metrics();
    assert_eq!(m.trades, 1, "one closed round trip");
    let net = 5.0 - (105.0 * 0.01 + 110.0 * 0.01); // 2.85
    assert!(
        (m.expectancy() - net).abs() < 1e-9,
        "expectancy must be net of entry+exit fees: got {} want {net}",
        m.expectancy()
    );
    // SIM-8: the 2x column now stresses on top of a fee-inclusive base:
    // net − total_fees = 2.85 − 2.15 = 0.70.
    let stress = bt.stress_expectancy_2x();
    assert!(
        (stress - 0.70).abs() < 1e-9,
        "2x-cost expectancy: got {stress} want 0.70"
    );
    // Accounting identity is untouched by the metrics change.
    assert!(bt.identity_residual().abs() < 1e-9);
    // The Monte-Carlo input sequence carries the same net number.
    assert_eq!(bt.trade_pnls().len(), 1);
    assert!((bt.trade_pnls()[0].1 - net).abs() < 1e-9);
}

/// Audit bug 2: a resting limit filled on a print exactly AT its price. The
/// trade-print rule is "trades THROUGH the price — touching is not filling"
/// (SIM-2 normative). At-price prints must not fill; strictly-through must.
#[test]
fn regression_audit2_trade_print_at_price_does_not_fill() {
    let mut bt = Backtester::new(
        engine(),
        Box::new(Scripted {
            script: vec![(OrderKind::Limit { px: 99.0 }, Side::Buy, 1.0)],
            next: 0,
        }),
        SimConfig {
            fill_model: FillModel::L1TopOfBook,
            latency_ns: 0,
            slip_frac: 0.0,
            ..SimConfig::default()
        },
        1,
    );
    bt.run(&[
        trade(0, 100.0, 1.0, Side::Buy),  // rests the buy limit @99
        trade(MS, 99.0, 5.0, Side::Sell), // print exactly AT 99: a touch
    ])
    .unwrap();
    assert_eq!(
        bt.position(SYM),
        0.0,
        "a print AT the limit price is a touch, not a fill"
    );
    // A print strictly through the price fills.
    bt.run(&[trade(2 * MS, 98.99, 1.0, Side::Sell)]).unwrap();
    assert_eq!(bt.position(SYM), 1.0);
}
