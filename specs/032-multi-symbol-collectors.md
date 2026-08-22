# 032 — Multi-Symbol Collector Fan-Out

## Purpose / Scope

Collapse the ops surface of the recording layer. Today one
`mp-collector` process owns exactly one `(venue, symbol)` (one exclusive
`.lock_{venue}_{symbol}`, one daily log `{yyyymmdd}_{venue}_{symbol}.log`,
one systemd unit, one gate/scorecard/drain line per symbol). Every new
symbol therefore adds a full process + unit + gate + drain file — the ops
tax that grows linearly with the data menu (bybit BTCUSDT → ETHUSDT →
SOLUSDT → OKX → Coinbase → Kraken).

This spec lets ONE process own MANY symbols of the SAME venue: one WS
connection (venue rate budgets shared), one process, one unit — while
**writing one daily log per symbol** so every existing consumer (audit,
promotion gate, scorecard, nightly drain, materializer, `core_symbols.txt`
resolver) keeps its per-symbol contract unchanged.

In scope: multi-symbol subscription fan-out, per-symbol log writers,
per-symbol lock ownership, per-symbol watchdog/health semantics, config
shape, and the migration path from the one-symbol-per-process deployment.
Out of scope: multi-VENUE in one process (venue adapters stay separate),
raw NDJSON capture layout (unchanged per symbol), and any change to the
event schema (spec 001).

## Design

Current (one process per symbol):

```
mp-collector --venue bybit --symbol BTCUSDT        (unit mp-collector@BTCUSDT)
  └── 1 WS conn, 1 EventLogWriter → 20260814_bybit_BTCUSDT.log
mp-collector --venue bybit --symbol ETHUSDT        (unit mp-collector@ETHUSDT)
  └── 1 WS conn, 1 EventLogWriter → 20260814_bybit_ETHUSDT.log
```

Target (one process per venue, N symbols):

```
mp-collector --venue bybit --symbols BTCUSDT,ETHUSDT,SOLUSDT
  └── 1 WS conn (subscribe frame carries all N topics)
  └── normalizer (symbol table interns N symbols — precedent: `--hip3-symbols`
      already fans out multiple hyperliquid symbols in one process, spec 030)
  └── N EventLogWriters, one per symbol:
        20260814_bybit_BTCUSDT.log
        20260814_bybit_ETHUSDT.log
        20260814_bybit_SOLUSDT.log
  └── N lock files (one per symbol, so a second process cannot double-own)
```

Routing: each normalized `EventEnvelope` is written to the log of its own
`(venue, symbol)` — the envelope already carries `venue` + `symbol_id`
(spec 001), and the writer table is keyed by interned symbol id, so the
routing is a single table lookup, no re-parse. Cross-symbol events
(venue-level `Status`, e.g. connect/disconnect) are written to EVERY
symbol log owned by the process (they are data for every symbol's
integrity pass — same as today, where each single-symbol process logs its
own Status stream).

### Config shape

`--symbols a,b,c` (comma list) replaces `--symbol x` for the fan-out mode;
`--symbol x` remains valid and is the degenerate one-symbol case (identical
behavior to today, byte-for-byte — migration is a config change, not a code
fork). Config file gains `symbols = ["BTCUSDT", "ETHUSDT"]` (replacing
`symbol = "BTCUSDT"`, mutually exclusive). The lock name, log name, and
heartbeat name per symbol are unchanged: `.lock_bybit_BTCUSDT`,
`{date}_bybit_BTCUSDT.log`, `mp-collector-bybit-BTCUSDT.heartbeat`.

## Requirements

- **MSC-1** One process MUST be able to own N symbols of one venue, writing
  one daily log per symbol, with N ≥ 1. `--symbols` (comma list) is the
  fan-out entry point; a single-element list MUST behave identically to
  today's `--symbol` mode.
- **MSC-2** Per-symbol exclusive ownership MUST be preserved: the process
  acquires one `.lock_{venue}_{symbol}` per owned symbol before writing, and
  refuses to start if ANY lock is held (all-or-nothing — a partial fan-out
  that silently owns half the set is worse than no fan-out). Lock files keep
  the existing name/lifecycle (binutil, audit 08-04: delete stale lock only
  if sure).
- **MSC-3** Per-symbol log rotation MUST follow the existing daily rotation
  rule (`{yyyymmdd}_{venue}_{symbol}.log`, flush-on-date-change,
  torn-tail recovery) independently per symbol — one symbol rolling over
  must not block another.
- **MSC-4** The recv-monotonic append boundary (spec 024, 2026-08-04:
  sort + clamp each batch per log) MUST apply per log writer — the
  monotonicity guarantee is per (symbol, log) stream, unchanged.
- **MSC-5** Subscription fan-out MUST share ONE WS connection and ONE venue
  rate budget across all owned symbols (COL-4 budget struct per venue).
  Bybit topic-batch limits per connection (spec 002 venue notes) MUST be
  honored: batch the subscribe frame if N exceeds the venue's per-frame
  topic cap, or split into the venue-supported number of args.
- **MSC-6** Per-symbol health MUST remain per-symbol: staleness watchdog
  thresholds (COL-2), `Status::Stale`/`Status::GapDetected` emissions, and
  the `mp-collector-{venue}-{symbol}.heartbeat` file are evaluated per
  symbol, not per process — a dead symbol inside a live process MUST be
  visible to the watchdog and the daily gate exactly as if it were its own
  process.
- **MSC-7** `write_symbols` (symbol metas) MUST be written to each symbol
  log that the symbol table change affects (all logs owned by the process
  carry the full interned table, matching today's per-process behavior).
- **MSC-8** The watchdog / `recordings.ps1` resolver / daily pipeline
  (which all derive from `core_symbols.txt` through ONE shared resolver,
  audit 08-10) MUST keep working unchanged: the fan-out is a deployment
  grouping, NOT a change to the recorded==required contract. `core_symbols.txt`
  keeps one `venue:SYMBOL` per line.
- **MSC-9** Migration MUST be non-destructive: switching a venue from N
  single-symbol processes to one N-symbol process must not lose a frame or
  overwrite an existing log. Recommended path: stop units (graceful flush),
  start the fan-out process, verify per-symbol first frames + the 00:05
  gate on the next day, then remove the old units. A-B overlap during
  migration is handled by the existing drain collision policy (identical =
  already landed).
- **MSC-10** A multi-symbol process MUST emit venue-level `Status` events
  into every owned symbol log (connect/disconnect/resubscribe are data for
  each symbol's integrity pass — COL-3 semantics preserved per symbol).

## Acceptance criteria

- [ ] `MSC-1`: fan-out process writes N daily logs; single-symbol list
  produces a byte-identical log to the current `--symbol` mode (fixture
  replay: same input frames → same output log).
- [ ] `MSC-2`: starting a fan-out with one held lock fails all-or-nothing
  (test with two symbols, one pre-held lock → exit nonzero, no partial
  ownership).
- [ ] `MSC-3`: per-symbol rotation — advance the date for one symbol only
  (injected clock), assert its log rolls while the other stays open.
- [ ] `MSC-4`: injected recv regression per symbol is clamped per-log
  (existing monotonicize tests parameterized over N writers).
- [ ] `MSC-5`: subscribe frame carries N topics under the venue topic cap;
  a venue cap test splits the frame correctly (bybit batch cap fixture).
- [ ] `MSC-6`: staleness on one symbol emits that symbol's Status + is
  visible in its heartbeat staleness without disturbing the other symbols'
  streams (mock-transport chaos test, collector_chaos pattern).
- [ ] `MSC-9`: migration smoke — old logs unchanged by the switch, new
  fan-out logs start clean, next-day gate passes for every owned symbol.

## Decisions

- 2026-08-15: **per-symbol log files, not one multi-symbol log.** The
  alternative (one log, many symbols — the hip3 precedent) would change
  every downstream consumer (audit/gate/drain/scorecard assume one
  symbol per `{date}_{venue}_{symbol}.log` name). Keeping the per-symbol
  file contract makes the fan-out a pure deployment grouping with zero
  consumer changes. The multi-symbol-in-one-log form remains available
  via the existing hip3 path and is NOT superseded.
- 2026-08-15: one process per VENUE (not per venue-symbol-set). Venue
  adapters stay separate (spec 002); fan-out is scoped to same-venue
  symbols so one venue's topic dialect and rate budget stay in one place.
- 2026-08-15: `--symbols` is the fan-out entry point; `--symbol` is
  preserved as the degenerate case so the deployed single-symbol units
  (binance, hyperliquid BTC/ETH, bybit BTCUSDT) are untouched until the
  owner chooses to regroup them.

## Open questions

- Should the fan-out also group the whale-positions census
  (`mp-whale`, spec 028) into the market collector, or does mp-whale stay
  its own process? (Spec 028 owns it today; grouping would reuse one WS
  connection but adds a second normalizer to the process — defer to spec
  028 owner.)
- VPS deployment grouping: one fan-out unit for all bybit symbols vs one
  per venue — recommended bybit fan-out first (it is the newest venue and
  the one with a growing symbol list); confirmation of the unit name
  pattern (`mp-collector@bybit` vs `mp-collector@bybit-multi`) before
  wiring systemd.
