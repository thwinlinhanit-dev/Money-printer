//! 24/7 live collector binary (SPEC-011). Records trades, tickers, and
//! liquidations to daily-rotated raw logs. Only compiles with `live-ws`.
//!
//! Run:
//!   cargo run --package mp-collectors --features live-ws --bin mp-collector -- --venue bybit --symbol BTCUSDT

#[cfg(feature = "live-ws")]
mod inner {
    use mp_collectors::ws::{endpoints, WsEndpoint, WsTransport};
    use mp_collectors::{Collector, CollectorConfig, DriveOutcome};
    use mp_core::log::EventLogWriter;
    use mp_core::Venue;
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn flag(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    }

    fn utc_date_str() -> String {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let days = d / 86400;
        let mut y = 1970i64;
        let mut rem = days as i64;
        loop {
            let days_yr = if is_leap(y) { 366 } else { 365 };
            if rem < days_yr {
                break;
            }
            rem -= days_yr;
            y += 1;
        }
        let months = [31, if is_leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let mut m = 0usize;
        while m < 12 && rem >= months[m] {
            rem -= months[m];
            m += 1;
        }
        format!("{:04}{:02}{:02}", y, m + 1, rem + 1)
    }

    fn is_leap(y: i64) -> bool {
        (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
    }

    fn subscribe_for(venue: &str, symbol: &str) -> Vec<String> {
        match venue {
            "bybit" => vec![format!(
                r#"{{"op":"subscribe","args":["publicTrade.{symbol}","tickers.{symbol}","liquidation.{symbol}"]}}"#,
            )],
            "binance" => {
                let stream = format!("{}@aggTrade/{}@markPrice@1s/{}@forceOrder", symbol.to_lowercase(), symbol.to_lowercase(), symbol.to_lowercase());
                vec![format!(r#"{{"method":"SUBSCRIBE","params":["{stream}"],"id":1}}"#)]
            }
            _ => vec![format!(r#"{{"op":"subscribe","args":["publicTrade.{symbol}"]}}"#)],
        }
    }

    fn endpoint_for(venue: &str) -> &'static str {
        match venue {
            "bybit" => endpoints::BYBIT_LINEAR,
            "binance" => endpoints::BINANCE_FUTURES,
            "okx" => endpoints::OKX_PUBLIC,
            "hyperliquid" => endpoints::HYPERLIQUID,
            _ => endpoints::BYBIT_LINEAR,
        }
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args: Vec<String> = std::env::args().collect();
        let venue = flag(&args, "--venue").unwrap_or_else(|| "bybit".to_string());
        let symbol = flag(&args, "--symbol").unwrap_or_else(|| "BTCUSDT".to_string());

        let _ = rustls::crypto::ring::default_provider().install_default();

        let venue_enum: Venue = match venue.as_str() {
            "bybit" => Venue::Bybit,
            "binance" => Venue::BinanceFutures,
            "okx" => Venue::Okx,
            "hyperliquid" => Venue::Hyperliquid,
            _ => return Err(format!("unsupported venue: {venue}").into()),
        };

        let subscribe_frames = subscribe_for(&venue, &symbol);
        let ws_url = endpoint_for(&venue);
        let endpoint = WsEndpoint {
            url: ws_url.to_string(),
            subscribe: subscribe_frames,
        };

        let normalizer = mp_collectors::normalizer_for(venue_enum);
        let mut collector = Collector::new(normalizer, CollectorConfig::default());

        let raw_dir = Path::new("data").join("raw");
        std::fs::create_dir_all(&raw_dir)?;

        let mut current_date = String::new();
        let mut log_writer: Option<EventLogWriter> = None;
        let mut last_symbol_count: usize = 0;

        let mut connection_count = 0u64;

        loop {
            connection_count += 1;
            tracing::info!(venue = %venue, symbol = %symbol, attempt = connection_count, "connecting");

            let mut transport = match WsTransport::connect(endpoint.clone(), 1024) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, "connect failed; retrying in 5s");
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };

            tracing::info!(venue = %venue, symbol = %symbol, "connected and subscribed");

            let mut event_buffer = Vec::new();
            loop {
                event_buffer.clear();
                let outcome = collector.drive(&mut transport, &mut event_buffer);

                if !event_buffer.is_empty() {
                    let date = utc_date_str();
                    if date != current_date {
                        if let Some(ref mut w) = log_writer {
                            w.flush().ok();
                        }
                        let log_path = raw_dir.join(format!("{date}_{venue}_{symbol}.log"));
                        tracing::info!(path = %log_path.display(), "rotating log file");
                        let (w, truncated) =
                            EventLogWriter::open(&log_path).expect("open event log");
                        if truncated {
                            tracing::warn!(path = %log_path.display(), "recovered torn tail");
                        }
                        log_writer = Some(w);
                        current_date = date;
                        last_symbol_count = 0;
                    }

                    if let Some(ref mut w) = log_writer {
                        let syms = collector.normalizer().symbols();
                        if syms.len() != last_symbol_count {
                            w.write_symbols(syms.metas()).expect("write symbol frame");
                            last_symbol_count = syms.len();
                        }

                        for ev in &event_buffer {
                            w.append(ev).expect("write event");
                        }
                        w.flush().ok();
                    }

                    if tracing::enabled!(tracing::Level::DEBUG) {
                        for ev in &event_buffer {
                            match &ev.body {
                                mp_core::MarketEvent::Trade { price, qty, .. } => {
                                    tracing::debug!(price, qty, "trade");
                                }
                                mp_core::MarketEvent::Funding { rate, .. } => {
                                    tracing::debug!(rate, "funding");
                                }
                                mp_core::MarketEvent::MarkPrice { mark, .. } => {
                                    tracing::debug!(mark, "mark_price");
                                }
                                mp_core::MarketEvent::OpenInterest { oi_notional, .. } => {
                                    tracing::debug!(oi = oi_notional, "open_interest");
                                }
                                mp_core::MarketEvent::Liquidation { price, qty, .. } => {
                                    tracing::warn!(price, qty, "liquidation");
                                }
                                _ => {}
                            }
                        }
                    }
                }

                match outcome {
                    DriveOutcome::Exhausted => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    DriveOutcome::Disconnected => {
                        tracing::warn!("transport disconnected; reconnecting");
                        break;
                    }
                    DriveOutcome::ParseFailureLimit => {
                        tracing::error!("parse failure limit reached; reconnecting");
                        break;
                    }
                }
            }

            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn main() {
    #[cfg(not(feature = "live-ws"))]
    {
        eprintln!("Error: 'live-ws' feature required.");
        eprintln!("  cargo run --package mp-collectors --features live-ws --bin mp-collector -- --venue bybit --symbol BTCUSDT");
        std::process::exit(1);
    }

    #[cfg(feature = "live-ws")]
    {
        tracing_subscriber::fmt::init();
        if let Err(e) = inner::run() {
            tracing::error!(error = %e, "collector failed");
            std::process::exit(1);
        }
    }
}
