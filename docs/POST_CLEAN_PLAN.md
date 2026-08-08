# Post-Clean-Data Plan

What the system does once the promotion gate passes (7 consecutive promotable
scorecards, tracked automatically by `data/scorecards/` + `mp-ops promote`).
Grounded in tooling that exists in this repo today.  This file is the
owner-facing execution plan; each step names the real binary/script that
performs it.

---

## Phase 0 — The gate fires (automatic)

- **Tracked by:** `ops/scripts/daily_pipeline.ps1` (Scheduled Task
  `MoneyPrinterDailyPipeline`, 00:05 UTC daily) → archives a lightweight
  scorecard to `data/scorecards/<date>.json` and prints the streak verdict.
- **Verdict:** `mp-ops promote --scorecards-dir data/scorecards
  --required hyperliquid:BTC hyperliquid:ETH` — the authoritative
  `check_promotion` gate (7 consecutive promotable days, all required
  recordings clean).
- **When it passes** the pipeline logs `PROMOTION GATE: PASSED` with the
  qualifying window.

**Gate check on this host right now:** `cargo run -p mp-ops --bin mp-ops --
promote --scorecards-dir data/scorecards`.

---

## Phase 1 — Promote clean days to cold storage (the ROADMAP Phase 0 gate)

- `mp-ops compact --date <yesterday> --venue hyperliquid --symbol BTC
  --require-stream trade book funding mark_price open_interest`
  writes verified Parquet to `data/cold/` through the INT-4 gate
  (2026-08-08: hyperliquid has no WS liquidation stream by design — that data
  comes from the on-chain whale census, spec 028, and is not a required gate
  stream; re-add `liquidation` when a venue with a native liq stream joins)
  (`compact_day_verified` refuses anything not clean).  The daily pipeline
  already does this automatically for promotable days.
- `research/archive_data.py` archives verified copies (originals remain
  append-only).
- **Written evidence** (ROADMAP rule): the `data/scorecards/` series + the
  `mp-ops promote` verdict JSON = the Phase 0 validation gate met on paper.

---

## Phase 2 — Feature materialization (spec 016)

- Feed promoted events through the feature engine into `FeatureStore` Parquet
  at `data/features/{feature}/{venue}/{symbol}/{date}.parquet` (deterministic
  output; `config_hash` + source-manifest hash for replay identity).
- Unlocks: historical screener grading, offline replay of identical feature
  state for backtests, and the ML/order-flow substrate (BACKLOG
  "orderflow dataset exports").
- **Shipped 2026-08-06:** `cargo run -p mp-storage --bin mp-materialize --release
  -- --log data/raw/<date>_*.log --config features/features.toml --out data/features
  --git-sha $(git rev-parse HEAD)` turns recorded event logs into the FeatureStore
  (deterministic, idempotent, multi-log symbol remap per EVT-5). Implementation:
  `storage/src/materialize.rs` + `features::engine_from_config`; integration
  coverage in `storage/tests/materialize.rs` (layout/round-trip, determinism,
  params-change version bump, multi-log merge, CLI e2e).

---

## Phase 3 — Research on clean data (parallel, cheap)

- **Backtests:** `cargo run -p mp-sim --bin sim -- backtest --log
  data/raw/<date>_binance_BTCUSDT.log --strategy carry-v1 --run-id <ulid>
  --runs-dir runs` — walk-forward (`wf`), Monte Carlo (`mc`) and the
  replay-live determinism diff are wired.
- **Event studies:** `python research/run_brief.py` /
  `research/event_study.py` — liq-cluster CAR, funding-extreme persistence,
  now on trustworthy provenance.
- **Screener grading:** `research/run_grading.py` grades screener hits
  (hit rate / Sharpe / edge decay) → promote / demote / kill per spec 017.
- **Honest constraint:** carry-v1 remains the one real strategy; everything
  else in `strategies/` is a baseline.  The funnel (spec 006) exists to kill
  ideas honestly before they cost money.

---

## Phase 4 — Funnel → Paper → Shadow → Live (spec 018, PD-1 human-gated)

- `funnel promote <mode>` checks the gate table: backtest → paper (2 weeks
  clean) → shadow (paper ≈ sim within tolerance) → live-small (4 weeks,
  human click, trade-only keys).
- **Only the human promotes; demotion is automatic.**  Nothing in this plan
  changes that (CLAUDE.md / PD-1).

---

## Phase 5 — Extend coverage (ROADMAP Phase 0 proper)

- The gate as written wants ~50 symbols.  Add symbols/venues (BACKLOG
  "more venues": OKX, Hyperliquid, spot basis legs; `funding-arb-v1` needs a
  second venue live).
- Add `positioning` collectors (BACKLOG tier-1) once the two-symbol corpus is
  promotable.

---

## Hard dependency: the egress block (spec 024 incident)

The 7-day streak cannot start until recordings are complete again.  Current
recordings are book-only (`trade_source=rest` backfills trades via REST and
`mark_source=rest` backfills mark/funding on a 15s poll, but the WS
`markPrice`/`forceOrder` streams are still filtered), and even the REST
mitigation does NOT produce promotable days: the 2026-08-05 scorecard shows
`sequence_gap` ×57 + `stale_stream` + `coverage_gap` (the 15s REST mark poll
cannot hold the 1s WS cadence).  Re-verified live 2026-08-06 with
`node ops/scripts/ws_probe.mjs` (20s direct probe): `depth@100ms`=190,
`bookTicker`=729 flow; `aggTrade`=0, `markPrice@1s`=0, `forceOrder`=0.  The
filter is still active; the proxy path below is the fix.  Two paths:

1. **Proxy/VPN** (preferred, restores full WS): provision a proxy/VPN with
   allowed-region egress, set `MP_WS_PROXY` (or `proxy = "http://…"|
   "socks5://…"` in the collector config), restart the collectors via
   `Stop-ScheduledTask MoneyPrinterCollectorsWatchdog` →
   `Start-ScheduledTask MoneyPrinterCollectorsWatchdog`, then verify BEFORE
   trusting new recordings: `forceOrder` must appear in `mp-audit --json`'s
   `streams` map (it has no REST fallback, so its presence proves the WS
   non-book streams flow again) and `mark_price` must jump from ~4488/day
   (15s REST poll) to ~86400/day (1s WS).  TLS still terminates against
   Binance (the proxy never sees plaintext).  Full procedure:
   `ops/runbooks/ws-egress-filter.md`.
2. **Fix egress from the network** (regional restriction, not a collector
   defect — verified 2026-08-04: `fstream.binance.vision` is globally
   NXDOMAIN; REST works; Bybit/OKX/Hyperliquid WS work).

Until one of these lands, `mp-audit`/`mp-ops scorecard` correctly report the
blocking findings (`sequence_gap`/`stale_stream`, and `missing_stream` if the
REST mitigations are ever removed) and the gate stays at 0.

**2026-08-08 resolution:** the proxy/VPN path is no longer the dependency.
The Phase-0 required venue switched to **Hyperliquid**, whose permissionless
API is not geo-filtered from this egress (verified live: WS trades/l2Book/
activeAssetCtx flow, audit CLEAN coverage 1.0).  The gate's required stream
set is now `trade book funding mark_price open_interest`, with liquidation
coming from the on-chain whale census (spec 028, `mp-whale`, recorded
separately).  See `ops/core_symbols.txt` + `ops/watchdog_collectors.ps1`.

---

## Verification checklist (owner)

- [ ] `mp-ops promote` shows `PROMOTED` with a 7-day window
- [ ] `data/cold/trades/binance/BTCUSDT/<date>.parquet` exists for clean days
- [ ] `mp-audit --data-dir data --json` shows `all_clean: true`
- [ ] A backtest run is archived in `runs/` with its run-id journal
- [ ] One idea has been honestly killed in the funnel with an autopsy report
