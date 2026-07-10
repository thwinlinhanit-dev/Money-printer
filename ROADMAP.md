# Roadmap — the single execution plan

One page tying phases → specs → deliverables → validation gates. This
consolidates SYSTEM_BLUEPRINT §10 and ARCHITECTURE_BRAINSTORM §8; if they
disagree, this file wins. Capital at risk may only increase after the phase's
validation criterion is met **in writing** (checked row in this table, with
evidence links).

| Phase | Build (specs) | Deliverable | Validation gate (written evidence) | Capital at risk |
|---|---|---|---|---|
| **0 — Record** | 000, 001, 002, 003 | Bybit collector 24/7 on a VPS; trades+book+funding+OI+liq for ~50 symbols → Parquet + manifests | 7 consecutive days with manifest coverage ≥ 0.995 on core symbols | $0 |
| **1 — See** | 009 (partial: opsd, Telegram, dead-man) + Grafana | Funding/OI/liq dashboards; cascade + funding-extreme alerts to phone | One alert you acted on; dead-man fires in a kill-drill | $0 |
| **2 — Perceive** | 004 | Feature engine live + materialized store; screener with hit journal | 30 days of graded screener hits; online/offline golden test green | $0 |
| **3 — Judge** | 005, 006 (funnel CLI) | Backtester L0/L1, walk-forward, MC, experiment tracker; funnel operating | One idea honestly killed with autopsy; golden determinism fixture in CI | $0 |
| **4 — Rehearse** | 007 (paper/shadow path), 008 | Paper mode on live feeds; sizing engine; chaos drills passed | 2 weeks paper, zero faults, paper ≈ sim within G3 tolerance | $0 |
| **5 — Execute small** | 007 (live path), 009 (complete) | Bybit testnet → live-small; carry-v1 through G3 | 4 weeks live-small: clean reconciliation, live ≈ paper within G4 tolerance | fixed min risk (owner sets $) |
| **6 — Portfolio** | 006 (strategies 2–3), 008 (allocator) | trend-breadth-v1 + liq-fade-v1 through funnel; allocator + monthly report | Blended curve smoother than best component; first monthly report generated | allocator-managed, ≤ quarter-Kelly |
| **7 — Compound & extend** | 010 (LLM agents), 011 (terminal), BACKLOG picks | Daily brief; more venues; scale within capacity | Rolling 6-month expectancy > 0 after all costs vs benchmark row | scales with evidence only |

**Standing rules across all phases**
- Any Prime Directive conflict halts the phase (CLAUDE.md).
- Demotion/de-risking is automatic; promotion/re-risking needs the human click.
- Every phase ships something used daily even if the project stops there.
- New ideas do not jump the queue: they enter `docs/BACKLOG.md`, get a spec,
  then get built. (This file changes rarely; the backlog changes often.)

**Current status:** pre-Phase-0. Next action: run the `bootstrap-workspace`
skill, then `implement-spec` for spec 001, then 002 (Bybit).
