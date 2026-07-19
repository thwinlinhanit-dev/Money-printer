# 019 — Collector Binary & Systemd Integration

## Purpose
Create a proper `main.rs` for the collector binary (currently none exists) with config file parsing, per-venue normalizer, event log writer, WS transport, reconnection logic, graceful shutdown, and a systemd unit template for production deployment.

## Scope
In: `collectors/src/main.rs`, `collectors.toml` config format, backoff config, systemd unit template, PID file, heartbeat file. Out: other binaries (oms, ops, funnel), collector business logic (normalizers, transport).

## Design

### Binary structure
```
mp-collector (binary)
├── --config <path> (flags only, no positional args)
├── Config (from collectors.toml)
├── Normalizer (venue-specific, e.g. binance::Normalizer)
├── WsTransport (venue-agnostic, from collectors/src/ws.rs)
├── EventLogWriter
├── Collector::drive() loop
├── Backoff (exponential with jitter)
├── SIGTERM handler
└── Heartbeat writer
```

One binary handles all venues — the venue is selected by the `venue` field in the config file (not separate binaries per venue). The normalizer is instantiated via a match on `Config::venue`.

### Config format (`collectors.toml`)
```toml
venue = "hyperliquid"
symbols = ["BTC", "ETH"]
data_dir = "/data/collect"
streams = ["trades", "l2Book", "activeAssetCtx"]

[backoff]
base_ms = 1000
cap_ms = 30000

[fsync]
every_n_events = 1000
every_sec = 10
```

### Collector::drive
```rust
pub async fn drive(&mut self) -> Result<()> {
    loop {
        match self.transport.connect().await {
            Ok(stream) => self.handle_stream(stream).await,
            Err(e) => { /* backoff, reconnect */ }
        }
    }
}
```
- On disconnect: emit `Status::Disconnected`, wait according to backoff schedule, reconnect.
- Backoff: exponential with jitter, base 1s, cap 30s, seed from config.

### Systemd unit (Linux only)
```ini
[Unit]
Description=Money Printer Collector (%i)
After=network.target

[Service]
Type=simple
User=printer
ExecStart=/opt/money-printer/bin/mp-collector --config /etc/money-printer/collectors/%i.toml
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

On Windows/development: run via `mp-collector --config path\to\config.toml` directly or as a scheduled task.

### PID & heartbeat
- PID file: `/run/mp-collector-{venue}.pid` (Linux) or `{data_dir}/mp-collector-{venue}.pid` (Windows).
- Heartbeat file: `/run/mp-collector-{venue}.heartbeat`, updated every 30s with Unix timestamp.
- Used by dead-man switch (spec 009) and monitoring.

### Graceful shutdown
- SIGTERM → stop WS loop → flush `EventLogWriter` → `sync_data()` → remove PID file → exit 0.
- SIGKILL: lose at most `fsync.every_n_events` events.

## Requirements
- **COL-15** `collectors/src/main.rs` MUST: parse config, build normalizer + transport, create `EventLogWriter`, run `Collector::drive`, handle SIGTERM.
- **COL-16** Config format MUST follow the `collectors.toml` spec above.
- **COL-17** A systemd unit template MUST be provided at `ops/systemd/mp-collector@.service`.
- **COL-18** The binary MUST write PID file and heartbeat file.
- **COL-19** Graceful shutdown on SIGTERM: flush event log, fsync, exit.

## Acceptance criteria
- [ ] `cargo build --release --features live-ws` produces `mp-collector` binary
- [ ] Test: `col_15_binary_parses_config` — verify config loading
- [ ] Test: `col_16_connects_via_ws_transport` — connects to the configured venue, verifies at least one event received within 30s (needs network, mark #[ignore])
- [ ] Test: `col_17_reconnects_on_disconnect` — mock transport, verify backoff sequence
- [ ] Test: `col_18_sigterm_flushes_log` — send SIGTERM, verify clean file
- [ ] Test: `col_19_systemd_unit_valid` — `systemd-analyze verify` passes
- [ ] Integration: run for 24 hours on VPS, verify no gaps in manifest

## Decisions
- 2026-07-19: One binary, multiple configs. Venue selected by `venue` field in `collectors.toml`. Simpler ops than separate binaries: one binary to build, one systemd template, one binary to update.
- 2026-07-19: Config per venue in `/etc/money-printer/collectors/{venue}.toml`. Systemd uses template instances: `mp-collector@hyperliquid`, `mp-collector@binance`, etc.
- 2026-07-19: Heartbeat: write timestamp to `/run/mp-collector-{venue}.heartbeat` every 30s.
- 2026-07-19: Backoff jitter uses `rand::thread_rng()` — no seed needed (jitter is for thundering herd avoidance, not deterministic replay).

## Open questions
- None.
