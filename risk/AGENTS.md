# risk

## Purpose

Risk management framework: position sizing (Kelly), capital allocation, killswitch, exposure governor, and risk gates.

## Ownership

- `src/sizing.rs` — position sizing models
- `src/portfolio.rs` — SWG-6 portfolio-level math (spec 035): `correlation_adjusted_exposure` (`sqrt(wᵀρw)`, fail-closed on corrupt/ragged matrices), `cumulative_funding_cost` + `expected_return_net_of_funding` (funding drag over the expected holding period, long-pays/short-receives)
- `src/kelly.rs` — Kelly criterion computation
- `src/allocator.rs` — capital allocation across strategies
- `src/governor.rs` — exposure governor
- `src/killswitch.rs` — automated killswitch logic
- `src/gate.rs` — risk gates (RG-1..13: RG-12 max concurrent positions — new-slot orders only; RG-13 correlation-adjusted exposure cap, caller-computed value)
- `src/config.rs` — risk configuration
- `risk.toml.example` — example config

## Verification

- `cargo test -p mp-risk`
- `cargo test -p mp-risk --test risk`
- `cargo test -p mp-risk --test execution`
- `cargo test -p mp-risk --test portfolio` (SWG-6 acceptance: `swg_6_*`)

## Child DOX Index

None.
