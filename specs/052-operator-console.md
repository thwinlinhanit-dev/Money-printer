# 052 — Operator Console (041 slice A, no WASM)

## Purpose

Give the owner one true screen: is the lab alive, what did yesterday
score, is paper diverging, is the kill latch set. This **completes the
app as a product** without building spec 011’s Cryexc clone.

## Scope

In: read-only HTTP + a single HTML page (or the existing `termd.py`
routes) bound to localhost. Out: order tickets, Auth0, Vite SPA,
footprint GPU, WASM/egui, remote exposure without SSH tunnel.

## Design

`mp-ops status` already aggregates mode, promotion, scorecard, backup,
latch (OPS-16). The console is a **renderer**, not a second source of
truth.

```
browser 127.0.0.1:8787
    → GET /v1/status     { mp-ops status JSON }
    → GET /v1/scorecard  latest DailyScorecard
    → GET /v1/paper      latest kind=paper run
    → GET /             static page
```

Reuse `research/termd.py` origin check and CSP (TER-10). Do not invent a
second server. If termd already serves `/v1/insight`, add the ops routes
beside it **or** a tiny `mp-ops serve-console` — pick one in Decisions
when implementing; default: extend termd so one port is the app.

## Requirements

- **CON-1** Console MUST bind 127.0.0.1 only. Binding 0.0.0.0 is a
  test-failing defect (personal tool, PD-2 adjacent).
- **CON-2** Page MUST show: UTC date, mode, last scorecard date,
  days_since (LAB-9), promotable bool, consecutive_clean / required,
  burst_days list, paper expectancy, paper faults, kill-latch tripped
  bool, data_home (redact if it contains a username — show only the
  last two path components plus a “Downloads?” warning boolean).
- **CON-3** Stale: if `days_since > 1`, the header MUST use the same
  severity language as pipeline-stale P1 (“gate did not land”).
- **CON-4** No trading controls. No “arm live” button. Mode display is
  read-only.
- **CON-5** Refresh ≤ 30s via existing WS batching or HTTP poll. Not a
  120 FPS terminal.
- **CON-6** Tests: `con_1_bind_localhost`, `con_2_status_fields`,
  `con_3_stale_copy`, `con_4_no_order_routes` (no POST /order).
- **CON-7** Spec 011 remains **deferred**. Implementing 052 MUST NOT
  mark 011 implemented.

## Acceptance criteria

- [ ] `con_1_bind_localhost`
- [ ] `con_2_status_fields` — JSON contract snapshot
- [ ] `con_3_stale_copy` — days_since=2 fixture
- [ ] `con_4_no_order_routes`
- [ ] `con_5_refresh` — documented; poll test optional
- [ ] `con_7_011_status_unchanged` — README row for 011 still not
      “implemented”

## Decisions

| Date | Decision |
|---|---|
| 2026-08-31 | v1 UI = operator console, not Cryexc. Completes “the app” for daily use. |
| 2026-08-31 | Localhost only. SSH tunnel if the owner wants a phone view. |
| 2026-08-31 | v1 console routes: extend `termd.py` (one port). `mp-ops serve-console` not needed. |
| 2026-08-31 | CON-2: `data_home` shows last two path components + `downloads_warning` boolean for the Downloads? display. |
| 2026-08-31 | CON-3: stale copy triggered when `days_since > 1` from LAB-9 status JSON. |
| 2026-08-31 | CON-5: refresh via existing WS batching in termd (≤500ms batch). |

## Open questions

- Port 8787 vs current termd port — use whatever termd already binds.
