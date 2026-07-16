//! Generates a synthetic event log for demo/testing purposes.
//! Produces 200 trade events across 2 hours of simulated market activity.

use mp_core::log::EventLogWriter;
use mp_core::{EventEnvelope, MarketEvent, Side, SymbolId, Venue};
use std::process::ExitCode;

const MS: i64 = 1_000_000;
const SEC: i64 = 1_000 * MS;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().map(|s| s.as_str()).unwrap_or("demo.eventlog");

    let (mut w, _) = match EventLogWriter::open(std::path::Path::new(path)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("gen_fixture: {e}");
            return ExitCode::from(2);
        }
    };

    let base_price = 65_000.0_f64;
    let mut rng_state: u64 = 42;

    for i in 0..200 {
        rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let noise = ((rng_state >> 33) as f64 / u32::MAX as f64 - 0.5) * 50.0;
        let price = (base_price + noise).max(60_000.0);

        let side = if rng_state % 2 == 0 { Side::Buy } else { Side::Sell };
        let qty = 0.001 + (rng_state % 100) as f64 * 0.001;
        let ts = i as i64 * 36 * SEC; // ~36s apart → 200 events over 2 hours

        let ev = EventEnvelope::new(
            Venue::Bybit,
            SymbolId(0),
            ts,
            ts,
            i as u64,
            MarketEvent::Trade {
                price,
                qty,
                side,
                trade_id: i as u64,
            },
        );
        if let Err(e) = w.append(&ev) {
            eprintln!("gen_fixture: write error: {e}");
            return ExitCode::from(2);
        }
    }

    if let Err(e) = w.sync() {
        eprintln!("gen_fixture: sync error: {e}");
        return ExitCode::from(2);
    }

    println!("wrote 200 synthetic trades to {path}");
    ExitCode::SUCCESS
}
