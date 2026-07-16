//! Live public market data collector binary.
//! Connects to Bybit's public linear streams, normalizes incoming trade events,
//! and writes them to a local event log file.
//!
//! Run with:
//!   cargo run --package mp-collectors --features live-ws --bin collect -- --symbol BTCUSDT

#[cfg(feature = "live-ws")]
fn run_collector(symbol: &str, output_path: &str) {
    use mp_collectors::ws::{endpoints, WsEndpoint, WsTransport};
    use mp_collectors::{Collector, CollectorConfig, BybitNormalizer, DriveOutcome};
    use mp_core::log::EventLogWriter;
    use std::path::Path;
    use std::time::Duration;

    println!("Starting data collection for Bybit {}...", symbol);
    println!("Output path: {}", output_path);

    // Install default crypto provider for rustls (required for wss:// connections)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // 1. Prepare Bybit public websocket subscription frame
    let subscribe_frame = format!(
        "{{\"op\":\"subscribe\",\"args\":[\"publicTrade.{}\"]}}",
        symbol
    );
    let endpoint = WsEndpoint {
        url: endpoints::BYBIT_LINEAR.to_string(),
        subscribe: vec![subscribe_frame],
    };

    // 2. Open local event log writer
    let (mut log_writer, _) = EventLogWriter::open(Path::new(output_path)).expect("open event log");

    // 3. Initialize Bybit normalizer and collector driver
    let normalizer = BybitNormalizer::new();
    let mut collector = Collector::new(normalizer, CollectorConfig::default());

    let mut connection_count = 0;

    loop {
        connection_count += 1;
        println!("Connecting (attempt {})...", connection_count);

        let mut transport = match WsTransport::connect(endpoint.clone(), 1024) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Failed to connect: {}. Retrying in 5s...", e);
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        println!("Connected and subscribed! Streaming data...");

        let mut event_buffer = Vec::new();
        loop {
            event_buffer.clear();
            let outcome = collector.drive(&mut transport, &mut event_buffer);

            // Write all normalized events to the log file
            for ev in &event_buffer {
                log_writer.append(ev).expect("write event to log");
                println!(
                    "Collected: {} trade @ {}",
                    if let mp_core::MarketEvent::Trade { qty, .. } = ev.body { qty } else { 0.0 },
                    if let mp_core::MarketEvent::Trade { price, .. } = ev.body { price } else { 0.0 }
                );
            }

            if !event_buffer.is_empty() {
                log_writer.flush().expect("flush log");
            }

            match outcome {
                DriveOutcome::Exhausted => {
                    // Non-blocking poll returned nothing; sleep briefly and continue
                    std::thread::sleep(Duration::from_millis(50));
                }
                DriveOutcome::Disconnected => {
                    eprintln!("Transport disconnected. Reconnecting...");
                    break;
                }
                DriveOutcome::ParseFailureLimit => {
                    eprintln!("Too many parse failures. Reconnecting...");
                    break;
                }
            }
        }

        std::thread::sleep(Duration::from_secs(1));
    }
}

fn main() {
    #[cfg(not(feature = "live-ws"))]
    {
        eprintln!("Error: The 'live-ws' feature must be enabled to run the collector.");
        eprintln!("Please run with: cargo run --package mp-collectors --features live-ws --bin collect");
        std::process::exit(1);
    }

    #[cfg(feature = "live-ws")]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let symbol = args.first().map(|s| s.as_str()).unwrap_or("BTCUSDT");
        let output_path = args.get(1).map(|s| s.as_str()).unwrap_or("collected.eventlog");
        run_collector(symbol, output_path);
    }
}
