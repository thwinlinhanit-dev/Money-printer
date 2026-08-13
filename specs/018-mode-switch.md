# 018 — One Runtime, Four Modes (Paper/Shadow/Live Mode Switch)

## Purpose

Define the runtime mode system that gates progression backtest → paper →
shadow → live (PD-1: human gating, PD-2: config outside the repo) **and** kill
the blueprint's "cardinal engineering sin": the two-code-path split between
the backtester and the live loop. ONE event-core runtime runs in every mode;
only the *source of events* (recorded replay vs live feed) and the *fill
sink* (sim vs broker) differ. With one runtime, the daily determinism check
becomes possible and cheap: replay yesterday's live session through the same
core and require byte-identical decisions — the single highest-value test in
the system.

## Scope

In: the `TradingMode` enum (implemented, `core/src/mode.rs`), mode config
outside the repo, promotion gates, demotion rules, per-mode safety checks, a
single `mp run --mode …` CLI, and the daily determinism check (replay
yesterday's recorded session → byte-identical decision log). Out: the safety
check implementations themselves (dead-man, recon, kill switch — spec
007/009), strategy internals, and the collector/feature machinery (other
specs).

## Design

### One runtime, four modes

```
                ┌────────────────────────────────────────────┐
events ────────▶│  event-core runtime (ONE binary: mp run)   │
                │  feed-in: recorded replay | live WS         │
                │  decisions: same strategy + risk + oms code │
                │  fills:    sim fills | real broker          │
                │  output:   decision log (every mode)        │
                └────────────────────────────────────────────┘
```

`mp run --mode sleep|backtest|paper|shadow|live` is the ONLY way the decision
path runs. Mode selects the two seams:

| Mode | Event source | Fill sink | Order flow |
|---|---|---|---|
| sleep | none | — | none (maintenance) |
| backtest | recorded replay (Dataset, SIM-6) | sim fills | none external |
| paper | live WS | sim fills | none external |
| shadow | live WS | none (decisions logged only) | none external |
| live | live WS | real broker (EXE) | PD-1: human promotion only |

Backtest and live feed the *same* strategy/risk/oms core — the seam is the
input adapter (replay vs WS) and the fill adapter (sim vs broker), never the
decision code.

### TradingMode

```rust
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TradingMode {
    Sleep,      // idle, no processing (maintenance)
    Backtest,   // recorded data, sim fills
    Paper,      // live data, sim fills, logged decisions
    Shadow,     // live data, no fills, decisions logged for comparison
    Live,       // live data, real fills (PD-1: human promotion only)
}
```

Implemented in `core/src/mode.rs` with `from_config()` (env override
`MONEY_PRINTER_MODE`, else the file below) and `demote()`.

### Mode configuration

File at `/etc/money-printer/mode.toml` on Linux (outside repo, 0600 perms).
On Windows: `%PROGRAMDATA%/money-printer/mode.toml`. Overridable via
`MONEY_PRINTER_MODE` env var for development (`MONEY_PRINTER_MODE=paper`
takes precedence over the config file).

```toml
mode = "paper"
promotion_token = "..."  # required for human-confirmed promotions
```

### Promotion gates

| Transition | Requirement |
|---|---|
| Backtest → Paper | 2 weeks clean backtest, walk-forward passed |
| Paper → Shadow | 2 weeks paper, zero faults, paper P&L within 10% of sim P&L |
| Shadow → Live | 4 weeks shadow, human confirmation, trade-only API keys |

`funnel promote <mode>` checks gates and prompts human confirmation.

### Daily determinism check

The north-star test (blueprint: the daily determinism check). Every morning
the pipeline replays *yesterday's recorded session* through the same
event-core runtime in backtest mode and compares the produced decision log to
what the live loop logged the day before (paper/shadow/live all log every
decision — the decision log is mandatory in every mode, MOD-4). Byte-identical
decisions (same event order, same intents, same risk verdicts) prove the live
path and the replay path are the same machine. Any diff raises the
`determinism-diff` alert (spec 009, registry id exists) and blocks promotion —
a live/backtest divergence is the exact failure mode the two-code-path sin
creates, and the check makes it visible within 24h, not after a loss.

The scaffolding exists: `sim/decision_log.rs` (decision records) and
`sim/paper.rs` (paper fills). What remains is the input seam (replay
yesterday's raw/feature inputs into the same engine entry point the live loop
uses) and the byte-identity comparator. Determinism requirements: fixed seed
(`SplitMix64`), sorted/deterministic iteration (CONV-10), no wall-clock reads
in the decision path (PD-3).

### Demotion

Automatic on any fault (asymmetry: auto-off, manual-on). If a safety check
fails, mode drops to the previous level:
- Live → Shadow (on recon failure)
- Shadow → Paper (on unhandled error)
- Paper → Backtest (on data corruption or sim mismatch)

### Mode logging

- On every startup: `INFO Running in {mode} mode`.
- On every decision (OrderIntent): `DEBUG mode={mode} intent={...}` — and the
  decision LOG (the determinism-check substrate) in every mode.
- On mode transition: `WARN Mode transition: {old} → {new}, reason: {reason}`.

### Live-only safety checks

- Dead-man switch armed (must receive heartbeat within interval).
- Reconciliation loop active (compare expected vs actual fills).
- Kill switch armed (can be triggered via telegram or API).

## Requirements

- **MOD-1** `TradingMode` MUST be defined in `core/src/mode.rs` with the five
  variants above, `from_config()`, and `demote()`.
- **MOD-2** Mode MUST be set in a config file outside the repo
  (`/etc/money-printer/mode.toml` on Linux, `%PROGRAMDATA%/money-printer/mode.toml`
  on Windows) or via `MONEY_PRINTER_MODE` env var. The config file path MUST
  NOT be inside the repo (PD-2).
- **MOD-3** Mode transitions MUST require the gates specified above. `funnel
  promote` MUST check gates and require human confirmation.
- **MOD-4** The mode MUST be logged on every startup and every decision; the
  decision log MUST be written in every mode (paper/shadow/live) — it is the
  determinism-check substrate.
- **MOD-5** `Live` mode MUST trigger additional safety checks: dead-man
  enabled, reconciliation loop active, kill switch armed.
- **MOD-6** The decision path MUST run through ONE binary (`mp run --mode
  …`). The same event-core runtime MUST serve backtest and paper/shadow/live;
  the only permitted differences are the input adapter (replay vs live WS)
  and the fill adapter (sim vs broker). A second decision-path codebase MUST
  NOT exist (blueprint: the cardinal engineering sin).
- **MOD-7** `paper`/`shadow` MUST consume the live feed through the same
  event pipeline as `live` (identical event ordering, identical streams) so
  the decision log is directly comparable.
- **MOD-8** `shadow` MUST produce decisions with NO fill sink (orders are
  logged, never sent); `paper` uses sim fills through `sim/paper.rs`.
- **MOD-9** The daily determinism check MUST replay the previous UTC day's
  recorded session through the runtime in backtest mode and compare the
  resulting decision log byte-for-byte with the live day's log. A diff MUST
  raise `determinism-diff` (spec 009) and MUST block promotion (spec 024).
- **MOD-10** The decision path MUST be deterministic: fixed seed, no
  wall-clock reads (PD-3), deterministic iteration (CONV-10). The same
  recorded inputs MUST produce the same decision log.
- **MOD-11** The check MUST be offline (recorded data only; no network) and
  MUST run in the daily pipeline after the scorecard (spec 024) passes.
- **MOD-12** A mode transition from `shadow`→`live` MUST require human
  confirmation (PD-1) and MUST NOT be possible via env var alone.

## Acceptance criteria

- [x] `TradingMode` exists and is configurable — `core/src/mode.rs`
  (`from_config`, `demote`).
- [x] Test: `mod_1_mode_logged_on_startup` — verify log entry
  (`core/src/mode.rs` tests).
- [x] Test: `mod_2_backtest_to_paper_gate` — demotion chain verified
  (`mod_2_backtest_to_paper_gate`).
- [x] Test: `mod_3_paper_uses_sim_fills` — live feed, verify no real orders
  (`mod_3_paper_uses_sim_fills`).
- [x] Test: `mod_4_live_enables_safety_checks` — dead-man, recon, kill switch
  flags (`mod_4_live_enables_safety_checks`).
- [x] Test: `mod_5_mode_switch_requires_human_confirm` — parse/confirm path
  (`mod_5_mode_switch_requires_human_confirm`).
- [x] Guardrail: `ops/ci/guardrails.sh` and its PowerShell port
  `ops/ci/guardrails.ps1` reject `mode = "live"` in tracked config (PD-1;
  verified 2026-08-06 — both pass green).
- [ ] `mp run --mode backtest|paper|shadow|live` — ONE binary serving every
  mode (MOD-6). Test: `mod_6_one_runtime_four_modes`.
- [ ] The live loop and the replay path share the engine entry point. Test:
  `mod_7_backtest_and_live_share_event_core`.
- [ ] `shadow` logs decisions with no fill sink (MOD-8). Test:
  `mod_8_shadow_never_fills`.
- [ ] Daily determinism check: replay yesterday → byte-identical decision
  log; a planted diff raises `determinism-diff` and blocks promotion
  (MOD-9..11). Tests: `mod_9_daily_determinism_replay`, `mod_10_decision_path_deterministic`,
  `mod_11_determinism_diff_blocks_promotion`.
- [ ] `MONEY_PRINTER_MODE=live` alone must NOT promote (MOD-12). Test:
  `mod_12_live_requires_human_confirmation`.

## Decisions

- 2026-07-19: Mode file: `/etc/money-printer/mode.toml` (outside repo, 0600 perms).
- 2026-07-19: Promotion: requires `funnel promote live --i-am-human` (already
  in funnel CLI).
- 2026-07-19: Demotion: automatic on any fault (asymmetry: auto-off,
  manual-on).
- 2026-08-13 (upgrade draft → ready): The two-code-path split is the
  blueprint's cardinal sin; this spec now mandates ONE runtime in every mode
  (MOD-6) with the daily determinism check (MOD-9..11) as the proof. The
  scaffolding exists (`sim/decision_log.rs`, `sim/paper.rs`); what remains is
  the input seam (replay yesterday's session into the same entry point) and
  the byte-identity comparator. Sequencing note (2026-08-13): the daily
  pipeline order becomes scorecard (spec 024) → determinism check (MOD-9) →
  compaction — the check runs on the raw/feature inputs the live loop used,
  BEFORE cold writes, mirroring the INT-4 gate's "verify before write"
  discipline.
- 2026-08-13: `determinism-diff` is already a registered P2 alert
  (`ops/src/registry.rs`) with a runbook; the check raises it through the
  existing framework (dedupe + quiet hours, spec 009).

## Open questions

- Does the determinism check compare against the FULL raw replay (cold Parquet
  of the day) or the live-loop's in-memory stream? Full replay is the honest
  proof but heavier; a fast path replays from the feature store and checks the
  decision log only. Owner decision needed before implementation.
