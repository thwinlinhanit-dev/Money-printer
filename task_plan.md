# Task Plan: Zero-Cost Mode Implementation

## Goal

Adapt the Money-Printer system to run usefully and indefinitely under $0
budget constraints (free-tier VPS + personal PC) while preserving the core
philosophy. Drop full L2 book requirement, enforce strict retention, and
lower promotion criteria.

## Next Step

Phase 3: Add ZSTD compression settings and bar-aggregate preference for older
hot-tier data in the storage crate.

## Current Phase

Phase 3 — Storage & Retention Policy (partially complete)

## Phases

### Phase 1: Documentation & Spec Updates ✅

- [x] Create `docs/ZERO_COST_MODE.md`
- [x] Amend `ROADMAP.md` with Zero-Cost section
- [x] Amend `specs/024-market-data-integrity.md` (Zero-Cost scoring)
- [x] Amend `specs/025-signal-catalog.md` (zero_cost_compatible flag)
- [x] Amend `specs/045-accumulation-detector.md` (graceful degradation)
- [x] Update `ops/core_symbols.txt` comment block
- **Status:** done

### Phase 2: Collector Configuration ✅

- [x] Create `collectors/zero_cost/` directory with BTC + ETH configs
- [x] Verify swing_only behavior covers Zero-Cost streams (drops L2 book)
- [x] Add reconnect/keepalive tuning for free-tier (`backoff_base_ms`, `backoff_cap_ms`)
- [x] Tune zero_cost configs: 500ms base, 60s cap for shared-bandwidth VPS
- **Status:** done

### Phase 3: Storage & Retention Policy ⚠️

- [x] Create `docs/RETENTION_POLICY.md`
- [x] Add retention enforcement in `daily_maintenance.sh` (delete raw logs > 14 days)
- [ ] Add compression settings (ZSTD >= 6) to storage crate
- [ ] Add bar-aggregate preference for older data in hot tier
- **Status:** in_progress (retention enforced via shell; crate-level compression pending)

### Phase 4: Feature & Signal Adjustments ✅

- [x] Add `zero_cost_compatible` flag to signal catalog (default: true)
- [x] Verify footprint signals work without full book (swing_only verified)
- [x] Verify accumulation detector degrades gracefully (spec 045 amended)
- **Status:** done

### Phase 5: Integrity, Scoring & Promotion Gate ✅

- [x] Add Zero-Cost scoring mode to mp-ops (`--zero-cost` flag)
- [x] Lower required streams to trades + funding + OI
- [x] Make stale bursts warnings only in Zero-Cost mode
- [x] Wire `--zero-cost` flag in `daily_maintenance.sh` (scorecard + promote)
- [x] `is_clean_zero_cost()` gate (0.95 coverage, stale as warning)
- [x] `promotion_verdict_zero_cost()` gate (14-day streak)
- **Status:** done

### Phase 6: Ops & Runtime ✅

- [x] Create `ops/runbooks/zero-cost-mode.md`
- [x] Update `swing_collectors.ps1` docs
- [x] Add disk monitoring guidance (in runbook)
- **Status:** done

### Phase 7: Testing & Validation ✅

- [x] Unit tests for Zero-Cost config (5 tests in `collectors/tests/zero_cost_configs.rs`)
- [x] Unit tests for `is_clean_zero_cost` (7 tests in `storage/src/audit.rs`)
- [x] Unit tests for `promotion_verdict_zero_cost` (5 tests in `storage/src/promotion.rs`)
- [x] Verify cargo test passes (59 total: 54 storage + 5 collectors)
- **Status:** done

## Decisions Made

| Decision | Rationale |
|---|---|
| Reuse swing_only as Zero-Cost base | swing_only already drops L2 book; Zero-Cost = swing_only + gate relaxation |
| Hyperliquid only for Phase-0 | Permissionless API, no geo-blocks, no KYC |
| BTC + ETH only | Storage bounded; no multi-symbol expansion |
| Full L2 book deferred indefinitely | Storage + compute constraints on free-tier |
| Live trading remains forbidden | PD-1 absolute |
| ZERO_COST defaults to 1 in daily_maintenance.sh | Free-tier VPS is the primary target; full-mode requires explicit opt-in |
| Free-tier backoff: 500ms base / 60s cap | Rides out transient blips on shared bandwidth without hammering reconnect |

## Errors Encountered

| Error | Attempt | Resolution |
|---|---|---|
| `daily_maintenance.sh` never passed `--zero-cost` | 1 | Added ZERO_COST env var; branch RECORDINGS/STREAMS/FLAGS |
| Collector had no config-driven backoff | 1 | Added `backoff_base_ms`/`backoff_cap_ms` to FileConfig, wired to StreamOpts |
| Audit tests used wrong `clean_audit()` | 1 | Moved helper into `promotion::tests` scope, re-exported via `use super::*` |
