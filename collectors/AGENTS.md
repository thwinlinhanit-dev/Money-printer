# collectors

## Purpose

Real-time market data ingestion from cryptocurrency exchange WebSocket streams and REST APIs. Normalizes raw exchange data into canonical `MarketEvent` types and writes them to daily event log files under `data/raw/`.

## Ownership

- `src/collector.rs` — generic `Collector` driver; `DriveOutcome` enum
- `src/normalize.rs` — `Normalizer` trait for exchange-specific normalization
- `src/binance.rs` — Binance normalizer, REST depth seeding, OI polling, REST aggTrades trade ingestion (COL-25..27)
- `src/bybit.rs`, `src/coinbase.rs`, `src/hyperliquid.rs`, `src/kraken.rs`, `src/okx.rs` — exchange normalizers
- `src/transport.rs`, `src/ws.rs` — WebSocket transport with backpressure
- `src/backoff.rs`, `src/backpressure.rs`, `src/rate.rs` — reconnection, backpressure, rate limiting
- `src/book_sync.rs` — book synchronization logic
- `src/json.rs` — JSON parsing utilities
- `src/rng.rs`, `src/staleness.rs` — helpers
- `src/binutil.rs` — shared helper utilities (lock/heartbeat/pid files, CLI flags) + the `--trace-file` subscriber builder (`trace_subscriber`, ANSI force-disabled — LOG-1: trace files are machine input, and the 2026-08-15 outage traces were polluted with ESC[..m codes that broke timestamp parsing; enforced by `binutil::tests::trace_subscriber_emits_ansi_free_timestamped_lines` + `ops/ci/check_log_hygiene.sh`)
- `src/bin/mp-collector.rs` — 24/7 live collector binary (entry point; `--trace-file` sink uses the shared plain-text builder). `swing_only = true` (config) or `--swing-only` drops the L2 book stream (`l2Book` / `orderbook.50`) while keeping trades + funding/mark/OI (+ bybit `allLiquidation`) — spec 035 SWG-1 / spec 036 SLQ-D: swing features are bar-only, never L2/tick inputs. Unit-tested (`swing_only_*`)
- `src/ibit.rs` — CBOE IBIT options-chain normalizer (spec 040): OCC symbol parsing, chain-snapshot → OptionTicker/OptionTrade (direction-by-kind convention for session volume), contract multiplier 100 (IBI-2), parse-canary (`parse_canary` — non-empty quotes yielding 0 contracts = schema drift, never silence)
- `src/bin/mp-ibit.rs` — IBIT poller binary (spec 040 IBI-1/IBI-9): own process, CBOE free delayed REST (15-min lag), COL-9 raw capture to `raw/cboe/{date}/`, staleness watchdog, lock/pid/heartbeat via binutil; config `ibit.toml.example` (deny_unknown_fields, IBI-7)
- `swing/*.toml` — the swing-set collector configs (bybit ×3 + standalone HL ×2 + whale census + FRED macro), deployed on the VPS via `ops/scripts/deploy_swing.sh`; topology + co-existence contract in `docs/SWING_DATA_PLAN.md`
- `src/bin/mp-whale.rs` — Hyperliquid whale-position poller (spec 028)
- `src/bin/mp-macro.rs` — FRED macro poller (spec 030; `FRED_API_KEY`)
- `src/bin/mp-netflow.rs` — Ethereum exchange-reserve balance poller (spec 034; `MP_ETHERSCAN_KEY`, Etherscan **V2** REST `api.etherscan.io/v2/api?chainid=1` — V1 deprecated venue-side 2025-08 and answers `status=0`; pinned by `nfl_url_uses_v2_route_with_mainnet_chainid`). LIVE since 2026-08-24: watchlist `collectors/netflow.toml` (5 exchange USDT hot wallets @ 300s); deploy pattern for local daemons: copy the release exe OUT of `target/` (e.g. `bin_local/`) or rebuilds fail on the Windows file lock while the process runs
- `src/etherscan.rs` — Etherscan normalizer (spec 034, NFL-1..8)
- `src/netflow.rs` — netflow config + Etherscan route selection (spec 034, NFL-2/NFL-7; unit-testable without `live-http`)
- `src/bin/collect.rs` — alias for `mp-collector` (identical entry point via `include!`, kept for older scripts)
- `src/bin/inspect_data.rs` — data inspection tool
- `src/bin/test_binance.rs` — Binance integration test binary
- `tests/collector_chaos.rs` — chaos/fault-injection tests
- `tests/bybit_fixtures.rs`, `tests/venue_fixtures.rs` — test fixtures

## Local Contracts

- One `mp-collector` process owns one `(venue, symbol)` via exclusive lock files (`.lock_{venue}_{symbol}`)
- Data written as daily event logs: `{yyyymmdd}_{venue}_{symbol}.log` in `data/raw/`
- Heartbeat file written every 15s: `mp-collector-{venue}-{symbol}.heartbeat`
- Binance book seeded from REST snapshot immediately after WS connect (before processing depth deltas)
- Binance trade source `trade_source = "ws" | "rest"` (config or `--trade-source`, default `ws`): REST mode polls `fapi/v1/aggTrades` on the shared rate budget with a fromId watermark, reports skipped id ranges as `Status::GapDetected`, and suppresses WS aggTrade frames (COL-25..27; required while fstream drops the trade stream — spec 024 incident 08-04)
- Egress proxy for the WS transport: `proxy = "http://host:port"` (HTTP CONNECT) or `"socks5://host:port"` in config, `MP_WS_PROXY` env overrides both the config and flag paths; TLS terminates against the venue, never the proxy (spec 024 incident 08-04). Verify a fix with `node ops/scripts/ws_probe.mjs` + the audit `streams` map (runbook `ops/runbooks/ws-egress-filter.md`)
- Frame loss triggers book reset and `Status::BackpressureDrop` event emission
- Backoff: 250ms base, 30s cap, full-jitter (COL-1)
- Depth reseed retries are rate-budgeted AND time-bounded: a failed Binance reseed defers its next attempt ~2s (audit 08-04; no per-iteration busy-spin)

## Work Guidance

- Build with `cargo build -p mp-collectors --features live-ws,live-http --bin mp-collector --release`
- Run: `cargo run -p mp-collectors --features live-ws --bin mp-collector -- --symbol BTCUSDT`
- Use `--config path/to/config.toml` for full configuration

## Verification

- `cargo test -p mp-collectors`
- `cargo test -p mp-collectors --test collector_chaos`

## Child DOX Index

None.
