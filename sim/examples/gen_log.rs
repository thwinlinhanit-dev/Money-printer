//! Generate a synthetic event log for driving the `sim` CLI end-to-end.
//!
//!   cargo run --release --example gen_log -- <out.log> [n_trades]
//!
//! Writes `n_trades` (default 2000) synthetic BTCUSDT trades in the core event
//! log format — the exact bytes `sim backtest|mc|replay-live` read. This is a
//! demo/smoke helper: real logs come from the live collectors (spec 002).

use mp_core::log::EventLogWriter;
use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};

const MS: i64 = 1_000_000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().cloned().unwrap_or_else(|| {
        eprintln!("usage: gen_log <out.log> [n_trades]");
        std::process::exit(2);
    });
    let n: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2000);

    let (mut w, _fresh) = EventLogWriter::open(std::path::Path::new(&path))
        .unwrap_or_else(|e| panic!("open {path}: {e}"));

    // A gently oscillating price with alternating aggressor — enough structure
    // for the CVD feature to fire and the CoinFlip strategy to trade.
    for i in 0..n {
        let price = 50_000.0 + ((i % 20) as f64 - 10.0) * 5.0;
        let ev = EventEnvelope::new(
            Venue::Bybit,
            SymbolId(0),
            i * 100 * MS,
            i * 100 * MS,
            i as u64,
            MarketEvent::Trade {
                price,
                qty: 1.0 + (i % 3) as f64,
                side: if i % 2 == 0 { Side::Buy } else { Side::Sell },
                trade_id: i as u64,
            },
        );
        w.append(&ev).unwrap_or_else(|e| panic!("append: {e}"));
    }
    w.sync().unwrap_or_else(|e| panic!("sync: {e}"));
    println!("wrote {n} trades to {path}");
}
