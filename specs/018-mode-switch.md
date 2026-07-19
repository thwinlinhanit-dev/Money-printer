# 018 — Paper/Shadow/Live Mode Switch

## Purpose
Define a runtime mode system that gates progression from backtest → paper → shadow → live, with safety checks activated at each stage, satisfying PD-1 (human gating) and PD-2 (config outside repo).

## Scope
In: `TradingMode` enum, config file (outside repo), promotion gates, demotion rules, safety checks per mode, startup logging. Out: specific safety check implementations (dead-man, recon, kill switch — spec 007/009).

## Design

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

### Mode configuration
File at `/etc/money-printer/mode.toml` on Linux (outside repo, 0600 perms). On Windows: `%PROGRAMDATA%/money-printer/mode.toml`. Overridable via `MONEY_PRINTER_MODE` env var for development (`MONEY_PRINTER_MODE=paper` takes precedence over config file).
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

`funnel promote <mode>` CLI command checks gates and prompts human confirmation.

### Demotion
Automatic on any fault (asymmetry: auto-off, manual-on). If a safety check fails, mode drops to the previous level:
- Live → Shadow (on recon failure)
- Shadow → Paper (on unhandled error)
- Paper → Backtest (on data corruption or sim mismatch)

### Mode logging
- On every startup: `INFO Running in {mode} mode`.
- On every decision (OrderIntent): `DEBUG mode={mode} intent={...}`.
- On mode transition: `WARN Mode transition: {old} → {new}, reason: {reason}`.

### Live-only safety checks
- Dead-man switch armed (must receive heartbeat within interval).
- Reconciliation loop active (compare expected vs actual fills).
- Kill switch armed (can be triggered via telegram or API).

## Requirements
- **MOD-1** `TradingMode` enum MUST be defined in `core/src/mode.rs`.
- **MOD-2** Mode MUST be set in a config file outside the repo (`/etc/money-printer/mode.toml` on Linux, `%PROGRAMDATA%/money-printer/mode.toml` on Windows) or via `MONEY_PRINTER_MODE` env var. The config file path MUST NOT be inside the repo (PD-2).
- **MOD-3** Mode transitions MUST require the gates specified above. `funnel promote` MUST check gates and require human confirmation.
- **MOD-4** The mode MUST be logged on every startup and every decision.
- **MOD-5** `Live` mode MUST trigger additional safety checks: dead-man enabled, reconciliation loop active, kill switch armed.

## Acceptance criteria
- [ ] `TradingMode` exists and is configurable
- [ ] Test: `mod_1_mode_logged_on_startup` — verify log entry
- [ ] Test: `mod_2_backtest_to_paper_gate` — verify gate criteria checked
- [ ] Test: `mod_3_paper_uses_sim_fills` — live feed, verify no real orders
- [ ] Test: `mod_4_live_enables_safety_checks` — verify dead-man, recon, kill switch
- [ ] Test: `mod_5_mode_switch_requires_human_confirm` — mock human input, verify gate
- [ ] Guardrail: `ops/ci/guardrails.sh` checks for mode = "live" in repo (already exists, verify)

## Decisions
- 2026-07-19: Mode file: `/etc/money-printer/mode.toml` (outside repo, 0600 perms).
- 2026-07-19: Promotion: requires `funnel promote live --i-am-human` (already in funnel CLI).
- 2026-07-19: Demotion: automatic on any fault (asymmetry: auto-off, manual-on).

## Open questions
- None.
