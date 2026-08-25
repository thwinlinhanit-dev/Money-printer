# research

## Purpose

Python research package for offline analysis: daily brief generation via LLM, strategy grading/screening, event studies, market coverage analysis, and data reader utilities.

## Ownership

- `registry.py` — research idea registry (roadmap Phase 1.3, spec 006): one
  owner-visible record per candidate (strategy hypothesis + backlog alpha
  bullets); stable slugs strip backlog parentheticals so a strategy and its
  backlog bullet resolve to ONE record; `check()` enforces exactly-one-record,
  the allowed funnel-state enum, and that every listed `run_id` exists in
  `runs/index.jsonl` (evidence agrees with the tracker, never free text);
  `render()` emits the weekly-review table
- `registry.jsonl` — the ledger itself (append/update via `run_registry.py`;
  never rebuilt from a winning backtest)
- `run_registry.py` — registry CLI: `check` (exit 1 on any problem), `seed`
  (scaffolds defaults for missing candidates, never overwrites curated
  records), `render`
- `feasibility.py` — economic feasibility gate (roadmap Phase 4.1): break-even
  holding period + minimum expected move from the ACTUAL proposed legs (fees,
  spread, slippage, funding at settlement, borrow, margin drag, transfer),
  base + stressed cases; REJECTS_BASE never claims the signal is false — it
  says the implementation cannot afford to discover it; funding legs without
  a settlement cadence are refused (realized post-entry cash flows, never
  annualized peaks); `hash_spec()` gives the deterministic assumptions hash
- `run_feasibility.py` — feasibility CLI: `--spec <json>` computes base +
  stressed economics; `--run-id` journals a `kind=feasibility` record into
  `runs/index.jsonl` (E7 duplicate refusal, exit 2); `--registry` +
  `--candidate` updates that record's `economic_feasibility`/`costs`
- `panel.py` — historical panel (roadmap Phase 3.1): versioned universe +
  manifest + membership from `data.binance.vision` daily klines
  (daily-bucket only); fidelity label `external_archive:binance-um-klines-1m-v1`
  (NOT evidence for queue position, maker fills, or sub-minute order-flow
  alpha); downloads are idempotent, 404s/missing sources are journaled
  `status=missing` records, never silently dropped; extended 2026-08-21 with
  `kline_url`/`fetch_kline` interval param (default `1m`), `month_range`,
  `download_klines` (monthly buckets), `kline_record` (with `day` param for
  daily-bucket records), `load_kline_manifest` (keyed
  `(symbol, interval, year_month, day|None)`), `append_kline_manifest` — the
  ledger is checksum-only (manifest rows carry `zip_sha256`/`rows`/
  `quote_volume_usd`; zip bytes are discarded after hashing, so readers must
  consume through the manifest, never raw files)
- `panel_universe.json` — panel universe v1 (binance-um-v1): BTCUSDT/ETHUSDT/
  SOLUSDT 2026-01-01..2026-08-18, daily threshold 10M quote volume,
  train/validation/test partitions chronological and non-overlapping, final
  test period untouched until hypothesis + parameters are frozen
- `run_panel.py` — panel CLI: `download` (manifest-ledgered), `check`
  (coverage gaps, exit 1), `verify` (seeded random sample rebuilt
  byte-identical from the manifest; corruption/missing => problems listed,
  exit 1), `fetch` (monthly 4h/1d klines with per-day daily-bucket fallback
  for the tail month whose monthly bucket is not yet published; ledger
  `panel/manifests/binance-um-v1-klines.jsonl`)
- `panel/` — per-universe manifest ledger (append-only); kline ledger + 1m
  ledger + `liq_daily.json` (daily liquidation aggregates from bybit logs,
  `mp-query liq --interval-secs 86400 --json`)
- `instrument_master.py` — point-in-time instrument master (roadmap Phase
  3.3): versioned records, last-match-wins resolve by `observed_from_ns`,
  fail-closed `InstrumentUnknown` on unresolved symbols; corpus filenames via
  `^(\d{8})_([a-z]+)_([A-Z0-9]+)\.log$`, `positions` census + `trace_*`/
  `watchdog_*` skipped as non-instruments
- `instrument_master.jsonl` — the master (9 instruments, timestamp-corrected)
- `run_instrument_master.py` — CLI: `resolve <SYMBOL>` (exit 2 on unknown),
  `check` (all corpus files resolve, exit 1 otherwise), `render`
- `autopsy.py` — strategy autopsy (roadmap Phase 4.5): classification
  (economic failure / data limitation / execution-model failure) computed
  deterministically from journaled run records; zero-trade runs are absence
  of evidence, never averaged into the mean; P&L by day from corpus or
  data_from_ns
- `run_autopsy.py` — autopsy CLI: writes `autopsies/<id>-<date>.md`
  (append-only, duplicate day refused exit 2) and links it in the registry
  record's `evidence` (no orphan reports); exit 2 when the candidate or any
  journaled run_id is missing
- `autopsies/` — generated autopsy reports
- `run_weekly_review.py` — weekly research review (roadmap Phase 1.2):
  ISO-week windowing of journaled runs (run_id date, else data_to_ns),
  registry table, autopsies list, benchmark grounded on
  `docs/OWNER_POLICY.md` §3 (BINDING 2026-08-25; fails closed to the explicit
  `unset` row when the policy or any benchmark field is missing/unparseable);
  append-only per week (exit 2 on duplicate); generated date is the only
  wall-clock input
- `reviews/` — weekly review reports
- `brief.py` — daily brief generation pipeline
- `insight_composer.py` — per-token AI insight agent (spec 044 TOK): deterministic pre-LLM flags, versioned prompt, InputBundle hash, `verify_grounded` numeric-subset check (signed numbers ground against signed metrics; a `%`-suffixed claim grounds against value×100), TTL cache + daily archive (latest wins per day, TOK-9); LLM injected as `llm_fn`, failure returns last cached insight marked stale (TOK-7)
- `telegram_bot.py` — Telegram `/insight [asset]` bot (spec 044 scope): command parsing incl. `@botname` suffix, cache-hit answers vs synchronous regeneration when stale, default-assets broadcast; `get_updates`/`send_message` transport is INJECTED so the whole command path runs hermetically; only `send_message(token, ...)` touches the network
- `run_telegram.py` — Telegram production entrypoint: reads `TELEGRAM_BOT_TOKEN` from the environment (never committed/logged), wires `run_polling` to real urllib long-poll transport; `--once` performs a single poll cycle then exits (cron/systemd-timer mode); `--assets` overrides watched underlyings; verified live against api.telegram.org 2026-08-24
- `grading.py` — strategy grading logic
- `grading_job.py` — automated grading job (emits machine `{week}.json` + human `{week}.md` report with next-stage recommendations, spec 017 GRD-3/5)
- `event_study.py` — event study framework (CAR + seeded bootstrap CI; every `StudyRecord` discloses `n_days` and `ci_reliable` — CI from <3 distinct UTC days is flagged unreliable, never presented bare)
- `run_backlog_event_studies.py` — RES-4 event-study gate for backlog alpha ideas (batch 1 2026-08-15: oi-purge-continuation, listing-flow-v1, weekend-liquidity-v1; batch 2 2026-08-16: funding-arb-v1, basis-carry-v1; re-gate r2 2026-08-16 when the 08-15 bybit day drained; re-gate 2026-08-18-r2 when the 08-16 bybit days drained — FARB-2 breadth met, verdict still NOT GRADABLE on event breadth): hourly mark/OI/funding via `mp-query carry` over raw logs, cross-asset excess + cross-venue funding-spread/basis gap dynamics, seeded bootstrap CI, honest NOT TESTABLE verdicts journaled to `runs/index.jsonl`; `n_days`/`ci_reliable` disclosure on every study row; oi-purge events gated to same-UTC-day windows (E3 — NOT TESTABLE at post=24 on the intraday-only corpus, reason journaled); the listing study is a static reason string, never a dead loop (E6 — the corpus has no Listing events by construction, spec 002/031); duplicate `--run-id` refused (append-only tracker, E7); `--run-id` distinguishes re-gate records; `DAYS`/`CROSS_VENUE_PAIRS` are updated when a drained day lands (2026-08-18: 08-16 bybit days added)
- `run_positioning_stress.py` — RES-4 positioning-stress confluence gate (first run 2026-08-18-r2, journaled): same-hour funding stress (corpus-tail bands 800/1200 bps/yr — the overlap corpus's funding range is ~±1352 bps/yr — plus absolute bands 2000/3000 that report NOT TESTABLE with the measured range as reason) x OI purge x one-sided bybit liquidation pressure; `mp-query carry` (hyperliquid) joined to `mp-query liq` (bybit, spec 029 COL-29) on `interval_ts_ns`; cross-asset excess CAR at +1/3/6h, seeded bootstrap, `n_days`/`ci_reliable` disclosure, E3 same-UTC-day windows, E7 duplicate run-id refusal; re-gate when bybit liq days with true funding stress drain
- `band_accuracy.py` — RES-4 whale band-accuracy parsing + weekly trend math
- `band_accuracy_job.py` — weekly band-accuracy job (shells out to `whale_study`, idempotent)
- `calibrate_leverage.py` — LIQ-11 leverage-tier calibration parse + TOML rendering
- `calibrate_leverage_job.py` — calibration job (shells out to `whale_study --leverage-calibration`)
- `archive_data.py` — data archiving
- `coverage.py` (in `mp_data/`) — market coverage analysis
- `reader.py` (in `mp_data/`) — event log reader
- `run_brief.py`, `run_grading.py`, `run_band_accuracy.py`, `run_calibrate_leverage.py` — CLI entry points
- `conftest.py`, `tests/` — pytest test suite (`tests/test_insight.py` = tok_1..10; `tests/test_telegram.py` = injected-transport bot tests; `tests/test_ws.py` = ter_1/8/10 live-server WS/CSP/pagination tests; `tests/test_accumulation.py` = acc_5 forward-return study)
- `prompts/daily-brief.md` — LLM prompt template
- `termd.py` — terminal server (spec 011 Slice 1 read-only HTTP API + spec 041
  analytics-terminal server surface): stdlib `http.server`, binds 127.0.0.1,
  serves the recorded feature store (long-format Parquet) + raw-log derivations
  via `mp-query bars`/`mp-query dom` to the local viewer; `/v1/*` JSON endpoints
  with `offset`/`limit` pagination + honest `total` (TER-8); strict security
  headers on every response — CSP without unsafe-eval, frame-ancestors 'none',
  nosniff (TER-10); `/v1/ws` versioned RFC6455 WebSocket (`X-Ws-Protocol-Version`,
  RFC-vector accept key, origin validation, subscribe/history/ping, coalesced
  batch pushes ≥100ms apart — inside TER-1's 500ms budget); `/v1/insight`
  (spec 044 TOK-5) cached-insight-with-staleness; static `terminal/`; read-only,
  no venue keys (UI-2), research-only (CONV-2 — never on live paths); missing
  symbol-days return `[]` + a `note`, never a silent blank chart (UI-5);
  `TERMD_DATA_ROOT`/`TERMD_MP_QUERY`/`TERMD_TERMINAL_DIR` env overrides;
  verify with `py -3.13 -m pytest research/tests/test_terminal.py research/tests/test_ws.py`
- `terminal/` — static canvas viewer (`index.html` + `app.js`, no framework,
  no build): venue/symbol/date/tf/footprint-bucket/kind pickers, price candles
  + footprint-delta coloring + whale markers, CVD pane, DOM ladder (scrubbed
  window via `/v1/dom`), funding/OI strip; one shared time axis with scrub
  crosshairs

## Local Contracts

- Python 3.12+ with pandas, pyarrow
- Research output feeds into `runs/index.jsonl` (append-only; `--run-id`
  duplicates are refused — re-runs need a fresh id)
- Grading results consumed by ops report pipeline

## Verification

- `cd research && python -m pytest`
- `cd research && ruff check .` — lint contract (config: `research/ruff.toml`); run `ruff check --fix .` to auto-fix
- `cd research && ruff format --check .` — format contract (config: `research/ruff.toml`, line-length 88, py312); run `ruff format .` to normalize before committing
- `py -3.13 research/run_registry.py check` — registry integrity (exit 1 on any problem)
- `py -3.13 research/run_instrument_master.py check` — every corpus file resolves to an instrument (exit 1 otherwise)
- `py -3.13 research/run_panel.py check` — panel manifest covers the universe (exit 1 on gaps)
- The CI research job (`.github/workflows/ci.yml`) enforces lint + format + pytest on every push
- Dependency advisories: the CI deps-audit job runs `pip-audit -r requirements.txt` (resolves the loose `>=` pins to the latest set, fails on ANY known vulnerability — no unmaintained/informational class like RustSec's; verified 2026-08-18: 0 findings)

## Child DOX Index

None.
