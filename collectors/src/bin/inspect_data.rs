use mp_core::event::MarketEvent;
use mp_core::log::LogReader;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw_dir = Path::new("data/raw");
    if !raw_dir.exists() {
        println!("No data/raw directory found.");
        return Ok(());
    }

    let mut entries: Vec<_> = fs::read_dir(raw_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "log"))
        .collect();

    entries.sort();

    println!(
        "{:<32} {:>9} {:>10} {:>19} {:>19} {:>12}",
        "Filename", "Size(MB)", "Events", "First Event UTC", "Last Event UTC", "Venues"
    );
    println!("{}", "-".repeat(110));

    let mut total_bytes: u64 = 0;
    let mut total_events: u64 = 0;
    let mut global_venues = BTreeSet::new();
    let mut global_event_types = BTreeMap::new();

    for path in entries {
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        total_bytes += metadata.len();

        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        // Only dotfiles are hidden artifacts; nothing else is silently dropped —
        // every *.log under data/raw is inspected and reported.
        if filename.starts_with('.') {
            continue;
        }

        let reader = match LogReader::open(&path) {
            Ok(r) => r,
            Err(e) => {
                println!("{:<32} {:>9.2} ERROR opening: {}", filename, size_mb, e);
                continue;
            }
        };

        let mut count = 0u64;
        let mut first_ts: Option<i64> = None;
        let mut last_ts: Option<i64> = None;
        let mut venues = BTreeSet::new();
        let mut event_types = BTreeMap::new();

        for ev_res in reader {
            match ev_res {
                Ok(ev) => {
                    count += 1;
                    if first_ts.is_none() {
                        first_ts = Some(ev.recv_ts_ns);
                    }
                    last_ts = Some(ev.recv_ts_ns);

                    let v_str = format!("{:?}", ev.venue);
                    venues.insert(v_str.clone());
                    global_venues.insert(v_str);

                    let event_kind = match &ev.body {
                        MarketEvent::Trade { .. } => "Trade",
                        MarketEvent::TradeWithAddr { .. } => "TradeWithAddr",
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
                        MarketEvent::NetflowSnapshot { .. } => "NetflowSnapshot",
                    };
                    *event_types.entry(event_kind).or_insert(0u64) += 1;
                    *global_event_types.entry(event_kind).or_insert(0u64) += 1;
                }
                Err(e) => {
                    println!(
                        "\n  [WARN] {} parse error at event {}: {}",
                        filename, count, e
                    );
                    break;
                }
            }
        }

        total_events += count;

        let first_str = first_ts.map(fmt_ts).unwrap_or_else(|| "N/A".to_string());
        let last_str = last_ts.map(fmt_ts).unwrap_or_else(|| "N/A".to_string());
        let v_summary = venues.into_iter().collect::<Vec<_>>().join(",");

        println!(
            "{:<32} {:>9.2} {:>10} {:>19} {:>19} {:>12}",
            filename, size_mb, count, first_str, last_str, v_summary
        );
        if !event_types.is_empty() {
            print!("   └─ Breakdown: ");
            let ets: Vec<String> = event_types
                .iter()
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect();
            println!("{}", ets.join(" | "));
        }
    }

    println!("{}", "-".repeat(110));
    println!(
        "TOTAL: {:.2} MB, {} events across all data files",
        total_bytes as f64 / (1024.0 * 1024.0),
        total_events
    );
    println!("Global event breakdown: {:?}", global_event_types);
    println!("Global venues: {:?}", global_venues);

    Ok(())
}

fn fmt_ts(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let millis = (ns % 1_000_000_000) / 1_000_000;
    // approximate ISO timestamp format
    let days = secs / 86400;
    let time_in_day = secs % 86400;
    let hours = time_in_day / 3600;
    let mins = (time_in_day % 3600) / 60;
    let s = time_in_day % 60;

    // approximate YYYY-MM-DD from days since 1970
    let mut y = 1970i64;
    let mut rem = days;
    loop {
        let days_yr = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
            366
        } else {
            365
        };
        if rem < days_yr {
            break;
        }
        rem -= days_yr;
        y += 1;
    }
    let months = [
        31,
        if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
            29
        } else {
            28
        },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while m < 12 && rem >= months[m] {
        rem -= months[m];
        m += 1;
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        y,
        m + 1,
        rem + 1,
        hours,
        mins,
        s,
        millis
    )
}
