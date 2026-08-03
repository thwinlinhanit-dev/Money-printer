# Phase 0 Integrity Milestone: Status Audit & Implementation Plan

Your stated milestone: **repair collector provenance and run a seven-day, two-symbol Binance recording audit.** Below is a detailed assessment of every idea you listed, mapped against the actual codebase, followed by an actionable plan.

---

## Current State Summary

You already have **substantial recordings**: 11 days of `binance/BTCUSDT` and 9 days of `binance/ETHUSDT` raw logs in [data/raw/](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/data/raw). The collector binary, event schema, provenance struct, audit module, daily scorecard, compactor, and manifest are all coded. But as you suspected, many pieces are **library-tested** without integration-level proof.

---

## Idea-by-Idea Status Assessment

### 1. Single ingestion contract — one collector per (venue, symbol)

| Sub-idea | Status | Evidence |
|---|---|---|
| One process per (venue, symbol) | ✅ **library-tested** | [mp-collector.rs L1-3](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/collectors/src/bin/mp-collector.rs#L1-L3) enforces this via `InstanceLock` |
| Never mix venue data in one log | ✅ **library-tested** | Collector hardcodes a single venue. [audit.rs L161](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/storage/src/audit.rs#L161) catches venue mismatches |
| Symbol ID process-wide registry | ✅ **library-tested** | `SymbolTable` in [symbol.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/core/src/symbol.rs), written as FRAME_SYMBOLS before events |
| Explicit provenance on every event | ✅ **library-tested** | `EventProvenance` struct in [event.rs L99-110](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/core/src/event.rs#L99-L110) with stream/subscription/connection_id/snapshot_source |
| Bad/mixed legacy logs quarantined | ✅ **library-tested** | [audit.rs L155-188](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/storage/src/audit.rs#L155-L188) quarantines venue mismatch, missing provenance, bad symbol table |

> [!IMPORTANT]
> All of these are **library-tested only**. The actual raw logs in `data/raw/` have never been audited to prove provenance is present on every event. The older logs (July 18-19) may predate the provenance stamping code.

---

### 2. Data quality visible and enforceable

| Sub-idea | Status | Evidence |
|---|---|---|
| Data audit CLI | ❌ **Not implemented** | `audit_raw_log()` exists as a library function but no CLI binary calls it |
| Compaction refuses contaminated logs | ⚠️ **Library code exists, not wired** | `audit.is_clean()` exists ([audit.rs L60](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/storage/src/audit.rs#L60)) but [compactor.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/storage/src/compactor.rs) does not call it (INT-4 gap) |
| Daily recording scorecard | ✅ **library-tested** | [audit.rs L212-239](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/storage/src/audit.rs#L212-L239) — but no runner generates one |
| 7-day promotion gate | ❌ **Not implemented** | No code tracks consecutive clean days |

---

### 3. Backpressure as risk-control

| Sub-idea | Status | Evidence |
|---|---|---|
| Four distinct policies | ✅ **library-tested** | [backpressure.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/collectors/src/backpressure.rs): Block, DropOldest, DropNewest, Unbounded |
| Default preserves newest + emits loss events | ✅ **integration-tested** | Default is `DropOldest`; [ws.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/collectors/src/ws.rs) tests confirm |
| Track dropped frames, queue HWM | ✅ **library-tested** | `TransportMetrics` in [ws.rs L36-39](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/collectors/src/ws.rs#L36-L39) |
| Processing latency, reconnects, resync time tracking | ⚠️ **Partial** | `HealthCounters` tracks reconnects/gaps/resyncs but not latency or resync time |
| Book-data drop invalidates book until REST snapshot | ✅ **integration-tested** | [mp-collector.rs L388-417](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/collectors/src/bin/mp-collector.rs#L388-L417) calls `reset_books()` on any frame loss |

---

### 4. Collector operationally boring

| Sub-idea | Status | Evidence |
|---|---|---|
| One deployment mode | ⚠️ **Two incomplete paths** | Windows watchdog ([watchdog_collectors.ps1](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/ops/watchdog_collectors.ps1)) AND 7 systemd units in [ops/systemd/](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/ops/systemd) |
| Reproducible command with config file | ✅ **library-tested** | [binance-btcusdt.example.toml](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/collectors/binance-btcusdt.example.toml) + `--config` flag |
| Bounded soak harness | ❌ **Not implemented** | No harness simulates disconnects, malformed frames, queue saturation, REST failure |
| Daily compact→manifest→audit pipeline | ❌ **Not implemented** | All pieces exist individually but no orchestrated pipeline |

---

### 5. Status language (maturity labels)

| Sub-idea | Status |
|---|---|
| Label system (library-tested → capital-approved) | ❌ **Not implemented** |

The [specs/README.md](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/specs/README.md) uses `draft/ready/implementing/implemented/superseded`. Your proposed 5-level language is more honest. **Several specs marked "✅ implemented" are arguably only library-tested** (e.g., 005, 006, 008, 010, 024).

---

### 6. Feature pipeline before indicators

| Sub-idea | Status | Evidence |
|---|---|---|
| Feature engine exists | ✅ **library-tested** | [features/src/engine.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/features/src/engine.rs) — 10.7KB |
| Feed recorded events through engine in a real runner | ❌ **Not implemented** | No binary or script materializes features from recorded logs |
| Config hash + source-manifest hash on materialized features | ❌ **Not implemented** | |
| Replay-vs-live golden comparisons | ❌ **Not implemented** | |

---

### 7. Replace carry-v1 with falsifiable strategy

| Sub-idea | Status | Evidence |
|---|---|---|
| carry-v1 exists | ✅ **library-tested** | [carry_v1.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/strategies/src/carry_v1.rs) — 10.5KB |
| Venue/symbol isolation | ⚠️ **Not enforced** | carry-v1 does not validate symbol isolation |
| Real funding accrual, position lifecycle | ❌ **Not implemented** | |
| Wrong-symbol, partial-fill, stale-funding tests | ❌ **Not implemented** | |
| Research report that can kill the idea | ❌ **Not implemented** | |

---

### 8. Simulation harder to fool

| Sub-idea | Status | Evidence |
|---|---|---|
| Fee and fill-model protections | ✅ **library-tested** | [fills.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/sim/src/fills.rs) + [gates.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/sim/src/gates.rs) |
| Data-quality penalties | ❌ **Not implemented** | |
| Multiple fill assumptions, worst credible outcome | ❌ **Not implemented** | |
| Strategy graveyard | ❌ **Not implemented** | |

---

### 9. Paper/shadow mode before OMS

| Sub-idea | Status | Evidence |
|---|---|---|
| Mode enum | ✅ **library-tested** | [mode.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/core/src/mode.rs) — 4.1KB |
| Shadow: record decisions only | ❌ **Not implemented** | |
| Paper: simulate fills from live feeds | ❌ **Not implemented** | |
| Daily shadow-vs-replay divergence check | ❌ **Not implemented** | |

---

### 10. Rebuild ops around one real daemon

| Sub-idea | Status | Evidence |
|---|---|---|
| opsd core exists | ✅ **library-tested** | [daemon.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/ops/src/daemon.rs) — heartbeat ingestion, status snapshot |
| opsd binary | ❌ **Not implemented** | No `main()` in [ops/src/bin/](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/ops/src/bin) runs the daemon |
| Health checks, alert routing, kill-latch | ⚠️ **Library-tested only** | `DeadMan`, `AlertRouter`, `KillLatch` exist but not wired into a running process |
| /status useful before Telegram | ⚠️ **Library-tested** | `StatusSnapshot` is serializable but no HTTP server exposes it |
| Test the actual process | ❌ **Not implemented** | Tests are unit-level on pure helpers |

---

### 11. Research jobs executable

| Sub-idea | Status | Evidence |
|---|---|---|
| run_brief.py exists | ✅ **Exists** | [run_brief.py](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/research/run_brief.py) — 2.2KB |
| run_grading.py exists | ✅ **Exists** | [run_grading.py](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/research/run_grading.py) — 1.8KB |
| Brief archives inputs/prompt/model/output/validation | ⚠️ **Partial** | [brief.py](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/research/brief.py) has archiving but needs integration testing |

---

### 12. Simplify deployment

| Sub-idea | Status | Evidence |
|---|---|---|
| Docker/Compose | ⚠️ **Exists but not real** | [compose.yaml](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/ops/compose.yaml) exists; no Dockerfile. Should be removed until real |
| Single config schema | ❌ **Not implemented** | Collector has its own TOML, nothing else uses it |
| Clean-host smoke test | ❌ **Not implemented** | |
| Pinned environments | ⚠️ **Partial** | [rust-toolchain.toml](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/rust-toolchain.toml) exists; Python not pinned |

---

### 13. Repository credibility

| Sub-idea | Status | Evidence |
|---|---|---|
| CI workflow | ✅ **Exists** | [.github/workflows/](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/.github/workflows) |
| cargo fmt/clippy/test as gates | ⚠️ **Unknown** | Need to inspect workflow file |
| Generated binaries in repo | ❌ **Problem** | `scratch_binance.exe`, `scratch_binance.pdb`, `scratch_funding.exe`, etc. are checked in at repo root |
| Demo artifacts in repo | ⚠️ **Problem** | `demo.eventlog`, `demo2.eventlog`, `collector_stdout.log`, `stderr.txt` at repo root |
| Scratch scripts in data/raw | ⚠️ **Problem** | ~20 `.ps1` test scripts, `.log`, `.bat` files in `data/raw/` |

---

## Proposed Concrete Milestone

> **Goal: Repair collector provenance and run a seven-day, two-symbol Binance recording audit.**

### Phase A — Audit CLI + Provenance Verification (Days 1-2)

#### [NEW] `storage/src/bin/mp-audit.rs`
A CLI binary that:
1. Takes `--data-dir`, `--venue`, `--symbol`, `--date` (or `--date-range`)
2. Calls `audit_raw_log()` on each matching raw log file
3. Prints a structured scorecard (JSON + human-readable summary)
4. Exit code 0 = all clean, 1 = findings

#### [MODIFY] [compactor.rs](file:///c:/Users/thwin/Downloads/Money-printer-claude-trading-research-intelligence-4tzim4/Money-printer-claude-trading-research-intelligence-4tzim4/storage/src/compactor.rs)
Wire `audit_raw_log()` as a pre-check: refuse compaction when `!audit.is_clean()` (INT-4).

---

### Phase B — Run the Audit on Existing Data (Days 2-3)

Run `mp-audit` on the existing ~11 days of BTCUSDT + ETHUSDT logs. This will likely expose:
- Early logs (July 18-19) with synthetic provenance (quarantine them)
- Possible gaps in multi-day recordings
- Missing stream coverage on some days

Produce a scorecard report documenting which days are clean.

---

### Phase C — 7-Day Promotion Gate (Day 3)

#### [NEW] `storage/src/promotion.rs`
Small module that:
1. Takes a list of daily scorecards
2. Requires 7 consecutive clean days across all required symbols
3. Returns a `PromotionVerdict` with pass/fail and the first failing date

---

### Phase D — Daily Pipeline Script (Day 4)

#### [NEW] `ops/scripts/daily_pipeline.ps1` (Windows-first)
Orchestrates: compact → manifest → audit → scorecard for yesterday's data. Same date/venue conventions throughout. Can be called from the Windows watchdog or a scheduled task.

---

### Phase E — Clean Up Repository (Day 4-5)

1. Add `scratch_*.exe`, `scratch_*.pdb`, `demo*.eventlog`, `*.log` at root to `.gitignore`
2. Remove `ops/compose.yaml` (no Dockerfile exists)
3. Move test `.ps1` scripts from `data/raw/` to a `data/raw/.scratch/` or remove from VCS

---

## Open Questions

> [!IMPORTANT]
> **Q1: Which deployment mode should be primary?** You run on Windows now. The systemd units reference Linux paths and binaries that may not exist. Should I remove the systemd units and formalize the Windows watchdog as the single path? Or do you plan to deploy on Linux soon?

> [!IMPORTANT]
> **Q2: Quarantine vs delete for pre-provenance logs?** The July 18-19 logs and Bybit/Hyperliquid logs likely lack live provenance. Should they be moved to a `data/quarantine/` directory, or just flagged by the audit as "legacy-unattributable" and left in place?

> [!IMPORTANT]
> **Q3: Spec status table accuracy.** Several specs are marked "✅ implemented" that are, by your language, only library-tested (005 Backtester, 006 Strategy API, 008 Risk & Sizing, 010 Research LLM, 024 Market Data Integrity). Should I update them to a new status like `🧪 library-tested` now, or wait until you approve the full status language?

## Verification Plan

### Automated Tests
```bash
# Existing tests prove library correctness
cargo test -p mp-storage -- audit
cargo test -p mp-storage -- compactor

# New acceptance test for INT-4
cargo test -p mp-storage -- int_4_compaction_refuses_quarantined_log

# New CLI integration test
cargo run -p mp-storage --bin mp-audit -- --data-dir data --venue binance --symbol BTCUSDT --date-range 20260722-20260729
```

### Manual Verification
- Run audit CLI on all existing raw logs, produce a scorecard artifact
- Verify clean days count ≥ 7 for both BTCUSDT and ETHUSDT (the actual milestone gate)
- Verify compactor refuses a known-bad log (empty provenance fixture)
