# Task Plan: Zero-Cost Mode Implementation

## Goal

Adapt the Money-Printer system to run usefully and indefinitely under $0
budget constraints (free-tier VPS + personal PC) while preserving the core
philosophy. Drop full L2 book requirement, enforce strict retention, and
lower promotion criteria.

## Next Step

Phase 1: Documentation & Spec Updates — create ZERO_COST_MODE.md, amend
ROADMAP.md, amend specs 024/025/045, update core_symbols.txt comments.

## Current Phase

Phase 1 — Documentation & Spec Updates

## Phases

### Phase 1: Documentation & Spec Updates

- [ ] Create `docs/ZERO_COST_MODE.md`
- [ ] Amend `ROADMAP.md` with Zero-Cost section
- [ ] Amend `specs/024-market-data-integrity.md` (Zero-Cost scoring)
- [ ] Amend `specs/025-signal-catalog.md` (zero_cost_compatible flag)
- [ ] Amend `specs/045-accumulation-detector.md` (graceful degradation)
- [ ] Update `ops/core_symbols.txt` comment block
- **Status:** in_progress

### Phase 2: Collector Configuration

- [ ] Create `collectors/zero_cost/` directory with BTC + ETH configs
- [ ] Verify swing_only behavior covers Zero-Cost streams
- [ ] Add reconnect/keepalive tuning for free-tier
- **Status:** pending

### Phase 3: Storage & Retention Policy

- [ ] Create `docs/RETENTION_POLICY.md`
- [ ] Add retention config to storage crate
- [ ] Add compression settings (ZSTD >= 6)
- [ ] Add bar-aggregate preference for older data
- **Status:** pending

### Phase 4: Feature & Signal Adjustments

- [ ] Add `zero_cost_compatible` flag to signal catalog
- [ ] Verify footprint signals work without full book
- [ ] Verify accumulation detector degrades gracefully
- **Status:** pending

### Phase 5: Integrity, Scoring & Promotion Gate

- [ ] Add Zero-Cost scoring mode to mp-ops
- [ ] Lower required streams to trades + funding + OI
- [ ] Make stale bursts warnings only in Zero-Cost mode
- **Status:** pending

### Phase 6: Ops & Runtime

- [ ] Create `ops/runbooks/zero-cost-mode.md`
- [ ] Update swing_collectors.ps1 docs
- [ ] Add disk monitoring guidance
- **Status:** pending

### Phase 7: Testing & Validation

- [ ] Unit tests for Zero-Cost config
- [ ] Integration tests for retention policy
- [ ] Verify cargo test passes
- **Status:** pending

## Decisions Made

| Decision | Rationale |
|---|---|
| Reuse swing_only as Zero-Cost base | swing_only already drops L2 book; Zero-Cost = swing_only + gate relaxation |
| Hyperliquid only for Phase-0 | Permissionless API, no geo-blocks, no KYC |
| BTC + ETH only | Storage bounded; no multi-symbol expansion |
| Full L2 book deferred indefinitely | Storage + compute constraints on free-tier |
| Live trading remains forbidden | PD-1 absolute |

## Errors Encountered

| Error | Attempt | Resolution |
|---|---|---|
| None yet | 1 | n/a |
