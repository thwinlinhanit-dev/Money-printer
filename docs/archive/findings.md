**STATUS: FROZEN — docs moratorium (2026-09-03), see CLAUDE.md §Docs moratorium.** Superseded by `docs/COMPLETION-MASTER-PLAN.md` (D1–D6) and `docs/STATUS.md`. Read-only history: do not update, extend, or act on this file until D1–D6 land.

# Findings: Serious Research Lab Roadmap

## Requirements

- Give the owner a detailed, ordered plan to turn the project into a serious
  research lab.
- Do not imply a guaranteed trading return or authorize live trading.

## Research Findings

- The binding roadmap holds capital at zero in Phase 0 until seven consecutive
  qualifying days have coverage of at least 0.995 and no stale bursts
  (`ROADMAP.md`).
- The 2026-08-17 audit identifies recorder/gate reliability and data location
  as the immediate constraints, not an absence of strategy code.
- Historical Binance aggTrades are intentionally isolated as
  `external_archive` / `aggregated` data; they cannot be represented as
  live-recorded L2 or maker-fill evidence (`specs/027-historical-bootstrap.md`).
- The current strategy evidence includes a losing order-flow baseline,
  liquidation-fade kill-direction evidence, and funding/carry ideas that are
  not yet gradeable as tradeable strategies (`docs/BACKLOG.md`).
- 2026-08-31 audit: last scorecard 2026-08-24; pipeline blind through 08-30;
  41.7 GB / 40 GB cap; drain ssh_failed ×14; compact 0-row; no promoted
  strategy; Pine README is not funnel evidence.
- v1 done = D1–D6 in `docs/COMPLETION-MASTER-PLAN.md`, not specs 011–049
  completeness.

## Technical Decisions

| Decision | Rationale |
|---|---|
| Two-track dataset: labelled historical data plus clean live recording | Historical data enables slow-horizon research; live data validates features and execution fidelity. |
| Research priority: breadth trend, then hedged carry/funding, then rare-event liquidation studies | It matches current data feasibility and avoids further high-turnover taker variants. |
| No discretionary execution exception | A personal trade journal may be useful context, but it must not masquerade as a promoted strategy. |
| Add registry, feasibility, and autopsy systems to the operating roadmap | They reduce research inventory and force honest economics before strategy work. |
| Add instrument-master and execution-calibration requirements | Historical breadth and paper P&L are otherwise vulnerable to metadata and fill-model errors. |

## Resources

- `ROADMAP.md`
- `docs/POST_CLEAN_PLAN.md`
- `docs/AUDIT-2026-08-17.md`
- `specs/005-backtester.md`
- `specs/018-mode-switch.md`
- `specs/027-historical-bootstrap.md`
- `docs/BACKLOG.md`
