# specs

## Purpose

Design specification documents (000–031) defining the system architecture, data schemas, protocols, behaviors, and invariants. Every crate references its governing specs.

## Ownership

- `README.md` — spec index and reading guide
- `000-conventions.md` — repository conventions
- `001-event-schema.md` — event schema specification
- `002-collectors.md` — collector architecture
- `003-storage.md` — storage layer
- `004-feature-engine.md` — feature engine
- `005-backtester.md` — backtester design
- `006-strategy-api.md` — strategy API
- `007-execution.md` — execution layer
- `008-risk-sizing.md` — risk sizing
- `009-ops-alerting.md` — operations and alerting
- `010-research-llm.md` — research/LLM pipeline
- `011-terminal.md` — terminal UI
- `012-zero-copy-pipeline.md` — zero-copy pipeline
- `013-ws-backpressure.md` — WebSocket backpressure
- `014-event-log-fsync.md` — event log fsync guarantees
- `015-carry-v1.md` — carry strategy spec
- `016-feature-materialization.md` — feature materialization
- `017-screener-grading.md` — screener/grading
- `018-mode-switch.md` — mode switching
- `019-collector-binary.md` — collector binary spec
- `020-binance-snapshot.md` — Binance snapshot protocol
- `021-bot-journal.md` — bot journal
- `022-screener-cadence.md` — screener cadence
- `023-string-interning.md` — string interning
- `024-market-data-integrity.md` — market data integrity
- `025-signal-catalog.md` — signal catalog
- `026-cross-venue-gap-detector.md` — cross-venue gap detector
- `027-historical-bootstrap.md` — historical bootstrap from Binance public archive
- `028-hyperliquid-whale-positions.md` — Hyperliquid whale position collector
- `029-liquidation-features.md` — cross-venue liquidation aggregation & estimated liq bands
- `030-macro-collector.md` — macro data collector (HIP-3 + FRED)
- `031-deribit-options-recorder.md` — Deribit options market-data recorder

## Local Contracts

- Specs are authoritative; code must match spec behavior
- When code and spec diverge, either code or spec must be updated — not both
- New features require a spec before implementation

## Verification

- Cross-reference implementation against spec during review

## Child DOX Index

None.
