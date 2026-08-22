# specs

## Purpose

Design specification documents (000–041) defining the system architecture, data schemas, protocols, behaviors, and invariants. Every crate references its governing specs.

## Ownership

- `README.md` — spec index and reading guide
- `000-conventions.md` — repository conventions
- `001-event-schema.md` — event schema specification (incl. the 2026-08-14 codec amendment, BDC requirements)
- `002-collectors.md` — collector architecture
- `003-storage.md` — storage layer
- `004-feature-engine.md` — feature engine
- `005-backtester.md` — backtester design
- `006-strategy-api.md` — strategy API
- `007-execution.md` — execution layer
- `008-risk-sizing.md` — risk sizing
- `009-ops-alerting.md` — operations and alerting
- `010-research-llm.md` — research/LLM pipeline
- `011-terminal.md` — WASM terminal UI
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
- `032-multi-symbol-collectors.md` — multi-symbol-per-process collector fan-out (MSC requirements)
- `033-wallet-identity.md` — wallet identity in trades (TradeWithAddr, WAL requirements)
- `034-exchange-netflow.md` — exchange netflow indexer (Ethereum reserve balances, NFL requirements)
- `035-swing-focus.md` — swing trading focus (multi-day/multi-week horizon spec, SWG requirements)
- `036-volume-profile-liquidity.md` — volume-profile liquidity features + swing-range-reclaim-v1 (SLQ requirements)
- `037-options-greeks-computation.md` — options Greeks computation engine: GEX profiles, higher Greeks, max pain, implied probability (GRE requirements)
- `038-iv-surface-builder.md` — IV surface builder & volatility analytics: term structure, skew, VRP, vol regime, DVOL index (IVS requirements)
- `039-options-flow-aggregator.md` — cross-exchange options flow aggregator: block trades, net premium, delta-adjusted flow, whale detection (OFI requirements)
- `040-ibit-etf-integration.md` — IBIT ETF options integration: CBOE data collection, cross-market analysis with Deribit (IBI requirements)
- `041-analytics-terminal.md` — real-time analytics terminal: web dashboard, WebSocket streaming, charts, per-asset routing (TER requirements)

## Local Contracts

- Specs are authoritative; code must match spec behavior
- When code and spec diverge, either code or spec must be updated — not both
- New features require a spec before implementation

## Verification

- Cross-reference implementation against spec during review

## Child DOX Index

None.
