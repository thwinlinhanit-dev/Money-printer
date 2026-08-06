use mp_core::event::MarketEvent;
use mp_core::log::LogReader;
use std::collections::BTreeMap;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path_str = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("data/collected.eventlog");
    let path = Path::new(path_str);

    if !path.exists() {
        println!("File does not exist: {}", path.display());
        return Ok(());
    }

    println!("Inspecting event log: {}", path.display());
    let reader = LogReader::open(path)?;

    let mut event_count = 0;
    let mut type_counts = BTreeMap::new();
    let mut symbol_counts = BTreeMap::new();
    let mut venue_counts = BTreeMap::new();
    let mut earliest_ts = i64::MAX;
    let mut latest_ts = i64::MIN;

    for ev_res in reader {
        let ev = ev_res?;
        event_count += 1;

        let type_name = match &ev.body {
            MarketEvent::Trade { .. } => "Trade",
            MarketEvent::BookDelta { .. } => "BookDelta",
            MarketEvent::BookSnapshot { .. } => "BookSnapshot",
            MarketEvent::Funding { .. } => "Funding",
            MarketEvent::MarkPrice { .. } => "MarkPrice",
            MarketEvent::OpenInterest { .. } => "OpenInterest",
            MarketEvent::Liquidation { .. } => "Liquidation",
            MarketEvent::IndexPrice { .. } => "IndexPrice",
            MarketEvent::Status { .. } => "Status",
            MarketEvent::WhalePosition { .. } => "WhalePosition",
            MarketEvent::MacroPoint { .. } => "MacroPoint",
            MarketEvent::OptionTrade { .. } => "OptionTrade",
            MarketEvent::OptionBook { .. } => "OptionBook",
            MarketEvent::OptionTicker { .. } => "OptionTicker",
        };

        *type_counts.entry(type_name.to_string()).or_insert(0) += 1;
        *venue_counts.entry(format!("{:?}", ev.venue)).or_insert(0) += 1;
        *symbol_counts.entry(ev.symbol.0).or_insert(0) += 1;

        if ev.recv_ts_ns > 0 {
            earliest_ts = earliest_ts.min(ev.recv_ts_ns);
            latest_ts = latest_ts.max(ev.recv_ts_ns);
        }
    }

    println!("\nSummary:");
    println!("Total Events: {}", event_count);

    println!("\nEvent Types:");
    for (k, v) in &type_counts {
        println!("  {:.<15} {}", k, v);
    }

    println!("\nVenues:");
    for (k, v) in &venue_counts {
        println!("  {:.<15} {}", k, v);
    }

    println!("\nInterned Symbol IDs:");
    for (k, v) in &symbol_counts {
        println!("  SymbolId({}): {}", k, v);
    }

    if earliest_ts != i64::MAX {
        println!("\nTime Range (local recv timestamp):");
        println!("  Earliest: {} ns", earliest_ts);
        println!("  Latest:   {} ns", latest_ts);
        let duration_ms = (latest_ts - earliest_ts) as f64 / 1_000_000.0;
        println!(
            "  Duration: {:.2} ms ({:.2} seconds)",
            duration_ms,
            duration_ms / 1000.0
        );
    }

    Ok(())
}
