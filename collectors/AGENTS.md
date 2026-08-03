# collectors

## Purpose

Real-time market data ingestion from cryptocurrency exchange WebSocket streams and REST APIs. Normalizes raw exchange data into canonical `MarketEvent` types and writes them to daily event log files under `data/raw/`.

## Ownership

- `src/collector.rs` — generic `Collector` driver; `DriveOutcome` enum
- `src/normalize.rs` — `Normalizer` trait for exchange-specific normalization
- `src/binance.rs` — Binance normalizer, REST depth seeding, OI polling
- `src/bybit.rs`, `src/coinbase.rs`, `src/hyperliquid.rs`, `src/kraken.rs`, `src/okx.rs` — exchange normalizers
- `src/transport.rs`, `src/ws.rs` — WebSocket transport with backpressure
- `src/backoff.rs`, `src/backpressure.rs`, `src/rate.rs` — reconnection, backpressure, rate limiting
- `src/book_sync.rs` — book synchronization logic
- `src/json.rs` — JSON parsing utilities
- `src/rng.rs`, `src/staleness.rs` — helpers
- `src/bin/mp-collector.rs` — 24/7 live collector binary (entry point)
- `src/bin/collect.rs` — offline collect/replay utility
- `src/bin/inspect_data.rs` — data inspection tool
- `src/bin/test_binance.rs` — Binance integration test binary
- `tests/collector_chaos.rs` — chaos/fault-injection tests
- `tests/bybit_fixtures.rs`, `tests/venue_fixtures.rs` — test fixtures

## Local Contracts

- One `mp-collector` process owns one `(venue, symbol)` via exclusive lock files (`.lock_{venue}_{symbol}`)
- Data written as daily event logs: `{yyyymmdd}_{venue}_{symbol}.log` in `data/raw/`
- Heartbeat file written every 15s: `mp-collector-{venue}-{symbol}.heartbeat`
- Binance book seeded from REST snapshot immediately after WS connect (before processing depth deltas)
- Frame loss triggers book reset and `Status::BackpressureDrop` event emission
- Backoff: 250ms base, 30s cap, full-jitter (COL-1)

## Work Guidance

- Build with `cargo build -p mp-collectors --features live-ws,live-http --bin mp-collector --release`
- Run: `cargo run -p mp-collectors --features live-ws --bin mp-collector -- --symbol BTCUSDT`
- Use `--config path/to/config.toml` for full configuration

## Verification

- `cargo test -p mp-collectors`
- `cargo test -p mp-collectors --test collector_chaos`

## Child DOX Index

None.
