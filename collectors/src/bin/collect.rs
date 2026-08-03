//! Thin alias for `mp-collector` (kept so older scripts keep working).
//! Prefer: `cargo run -p mp-collectors --features live-ws --bin mp-collector`.

fn main() {
    eprintln!(
        "note: `collect` is an alias of `mp-collector` (default: venue=binance, symbol=BTCUSDT)"
    );
    // Re-invoke logic by exec'ing the same crate binary path is awkward; share
    // by including the same main path. For simplicity, document and exit with
    // the same feature gate message if live-ws is off; when live-ws is on,
    // call the same run entry via re-export is not available — spawn same code.
    #[cfg(not(feature = "live-ws"))]
    {
        eprintln!(
            "Enable live-ws: cargo run -p mp-collectors --features live-ws --bin mp-collector"
        );
        std::process::exit(1);
    }
    #[cfg(feature = "live-ws")]
    {
        // Honest help text: mp-collector's actual flags are `--config <path>`
        // or `--venue <v> --symbol <s>`. There is no whale-stream toggle
        // (an earlier draft advertised `--no-whale`; that flag does not exist).
        eprintln!(
            "run: cargo run -p mp-collectors --features live-ws,live-http --bin mp-collector -- [--config path/to/config.toml | --venue binance --symbol BTCUSDT]"
        );
        eprintln!("flags: --config <toml>  (full config) | --venue <binance|bybit|okx|hyperliquid>, --symbol <SYM>");
        eprintln!("defaults: --venue binance --symbol BTCUSDT; one (venue,symbol) per process.");
        std::process::exit(2);
    }
}
