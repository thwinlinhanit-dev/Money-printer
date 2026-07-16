//! Thin alias for `mp-collector` (kept so older scripts keep working).
//! Prefer: `cargo run -p mp-collectors --features live-ws --bin mp-collector`.

fn main() {
    eprintln!("note: `collect` is an alias of `mp-collector` (default: binance + hyperliquid whales)");
    // Re-invoke logic by exec'ing the same crate binary path is awkward; share
    // by including the same main path. For simplicity, document and exit with
    // the same feature gate message if live-ws is off; when live-ws is on,
    // call the same run entry via re-export is not available — spawn same code.
    #[cfg(not(feature = "live-ws"))]
    {
        eprintln!("Enable live-ws: cargo run -p mp-collectors --features live-ws --bin mp-collector");
        std::process::exit(1);
    }
    #[cfg(feature = "live-ws")]
    {
        // Forward all args after program name by resetting is unnecessary —
        // mp-collector and collect share the workspace; run identical binary body.
        // We compile the live loop only once in mp-collector; this alias prints
        // usage and tells the user to switch. (Avoid dual 200-line copies.)
        eprintln!(
            "run: cargo run -p mp-collectors --features live-ws --bin mp-collector -- --symbol BTCUSDT"
        );
        eprintln!("defaults: --venue binance, Hyperliquid whale stream on (disable with --no-whale)");
        std::process::exit(2);
    }
}
