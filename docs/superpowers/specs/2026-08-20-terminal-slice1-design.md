# Terminal Slice 1 — Recorded-Data Research Viewer (2026-08-20)

Status: implemented 2026-08-20 (termd.py + terminal/ + mp-query bars/dom
subcommands + tests).
Governed by `specs/011-terminal.md` (draft) — this is Slice 1 of that spec, staged early per owner decision.

## Purpose

A read-only local research terminal that renders this system's **recorded** data —
footprint, CVD, DOM depth, funding/OI, whale-print markers — so the research loop
(autopsy, event studies, grading) can *look* at what the backtests grade. It is the
proto-History-Protocol (UI-3 shape) and the view-layer pilot for Slice 2's live
egui/WASM endgame, which stays deferred.

## Scope

In: local read-only HTTP API over the feature store + raw logs; single-page canvas
viewer; tests; DOX.
Out (Slice 2, deferred): live exchange WS in the browser, egui→WASM renderer, GPU
heatmap path, SvelteKit shell, ClickHouse warm store, `histd` production binary,
public hosting, token auth, any order entry.

## Contracts (from spec 011, honored early)

- **UI-1**: aggregation reuses the existing feature store (materialized from the
  `features`/`core` crates) — one code path extends to the eyes.
- **UI-2**: read-only. No order entry, no venue keys in the browser, ever.
- **UI-3**: the API is versioned (`/v1/`) and read-only; Slice 2's `histd` upgrades
  these same endpoints.
- **UI-5**: never render stale-looking data — missing symbol-days show an explicit
  empty state, never a blank chart.
- PD-1/PD-2: read-only + no secrets. CONV-2: lives in `research/`, never on the live
  VPS or live paths. Local-only (127.0.0.1).

## Architecture

```
[feature store] ─┐
[raw logs]      ─┼→ research/termd.py  (stdlib http.server, 127.0.0.1:8765)
[symbols snapshot]─┘   GET /v1/... (JSON)
                            ▼
        research/terminal/index.html + app.js (canvas, no framework, no build)
```

- `research/termd.py` — read-only HTTP server, Python stdlib `http.server`
  (`ThreadingHTTPServer`); polars/pyarrow for Parquet reads. Binds `127.0.0.1` only.
- `research/terminal/index.html` + `research/terminal/app.js` — single-page canvas
  viewer. No framework, no build step.
- Data layer — reads feature-store long format (`symbol_id, venue_code, ts_ns, value,
  ver`) filtered by venue/symbol/date; `data/features/symbols/*.json` snapshot for
  id→(venue, venue_symbol) resolution; raw logs for OHLCV + DOM ladder.

## API surface (proto-History Protocol v1)

All endpoints `GET`, scoped to a `(venue, symbol, date)` triple, JSON responses.
Unknown symbol/date → 404 with message; empty feature set → `[]`; corrupt parquet →
500 with file path; malformed query → 400 with reason.

| Endpoint | Returns |
|---|---|
| `GET /v1/symbols` | (venue, symbol, id) pairs + dates that have features |
| `GET /v1/dates?venue=&symbol=` | Available dates for that symbol |
| `GET /v1/bars?venue=&symbol=&date=&tf=60s` | OHLCV derived from raw logs |
| `GET /v1/footprint?venue=&symbol=&date=&bucket=mid\|small\|whale` | Footprint delta + imbalance series (60s buckets) |
| `GET /v1/cvd?venue=&symbol=&date=` | Cumulative volume delta series |
| `GET /v1/depth?venue=&symbol=&date=` | `book.depth` + `book.depth_total` series |
| `GET /v1/funding?venue=&symbol=&date=` | Funding rate series |
| `GET /v1/oi?venue=&symbol=&date=` | OI delta series |
| `GET /v1/whale?venue=&symbol=&date=` | Whale-print marker events |
| `GET /v1/dom?venue=&symbol=&date=&from=&to=` | Order-book ladder from raw logs (bounded window, row cap, timeout) |

Decisions:

- **Price series:** the feature store has no OHLCV feature; bars are derived on
  demand from raw-log trades/mark, bucketed to `tf` (default 60s). One symbol-day is
  cheap.
- **DOM ladder:** reconstructed from raw book deltas only for a user-selected window
  (`from`/`to`), row-capped and time-boxed — full-day reconstruction deferred.
- **Payloads:** JSON v1; Arrow IPC noted for when rows grow (Slice 2).

## Views (v1)

- Top bar: venue/symbol/date pickers.
- Main pane: price bars + footprint delta/imb colored on bars (mid/small/whale
  bucket toggle) + whale-print markers pinned to their bar.
- CVD pane below, time-synced with the main pane.
- DOM ladder side pane (bounded window).
- Funding/OI strip along the bottom.
- One shared time axis; hover/scrub crosshairs all panes.
- Empty symbol-day → explicit "no data for this symbol-day" state (UI-5).

## Error handling

- Server binds 127.0.0.1; request log to stderr.
- 400 malformed query, 404 unknown symbol/date, 500 corrupt parquet (path in body).
- Raw-log derivations (bars, DOM) run under a per-request timeout + row cap; a day
  with no trades returns `[]` + note, never a hang.

## Testing

- `research/tests/test_terminal.py` (pytest, matching `research/tests` layout):
  - API handlers over a synthetic feature-store fixture (footprint/cvd/funding/oi/
    depth/whale rows + a small raw log);
  - 400/404/500/empty cases;
  - bars-parity: derived OHLCV == hand-built expectation from the synthetic raw log.
- Verification: `py -3.13 -m pytest research/tests -k terminal`,
  `ruff check research`, and a manual launch smoke test (open page, pick a symbol-day,
  confirm every pane renders).

## DOX

- `research/AGENTS.md`: add `termd.py` + `terminal/` ownership, the proto-protocol
  note, and verification.

## Decisions

- 2026-08-20: owner approved Slice 1 before Slice 2 (live/wasm endgame). Stack:
  stdlib HTTP server + canvas page (zero new deps, local-only). Full view set v1.
- Not committed to git — repo rulebook requires explicit approval before commits.