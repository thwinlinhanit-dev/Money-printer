# risk

## Purpose

Risk management framework: position sizing (Kelly), capital allocation, killswitch, exposure governor, and risk gates.

## Ownership

- `src/sizing.rs` — position sizing models
- `src/kelly.rs` — Kelly criterion computation
- `src/allocator.rs` — capital allocation across strategies
- `src/governor.rs` — exposure governor
- `src/killswitch.rs` — automated killswitch logic
- `src/gate.rs` — risk gates
- `src/config.rs` — risk configuration
- `risk.toml.example` — example config

## Verification

- `cargo test -p mp-risk`
- `cargo test -p mp-risk --test risk`
- `cargo test -p mp-risk --test execution`

## Child DOX Index

None.
