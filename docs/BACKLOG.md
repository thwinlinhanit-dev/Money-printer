# Backlog — every idea, so nothing lives only in a chat

The idea inventory. Rules: new ideas land here first (a line is enough);
nothing gets built from here without a spec (PD-6); nothing here jumps the
ROADMAP queue without the owner saying so. Items are grouped by theme and
tagged **[v1.x]** (fits current architecture), **[v2]** (needs a design
decision), or **[maybe-never]** (recorded so it stops being re-proposed).

## Strategies & alpha (each needs hypothesis.md first — spec 006)
- **[v1.x] orderflow-v1 (FIRST BACKTEST 2026-08-13)** — depth-gauge +
  tape-bps_delta alignment continuation trade (`strategies/orderflow-v1/`,
  hypothesis + impl + 7 `of_*` tests). Registered in the sim
  (`sim backtest --strategy orderflow-v1`); the `book.depth.*` / `tape.*`
  features are now strategy-visible in the sim engine (spec 004/006). First
  real-day results (08-12, hyperliquid, seed 42, 2x costs): BTC 262 trades
  expectancy -165, ETH 57 trades -238 — the naive v1 LOSES both legs, which
  is the honest first verdict (costs dominate at this horizon; falsification
  criterion #1 engages). Determinism proven: two same-seed runs over the BTC
  day produced byte-identical decision logs (hash 6263363002049928352).
  Next: param grid / walk-forward before any promotion — likely a kill or a
  major redesign (add costs-aware entry hysteresis, wider bands).
- **[v1.x] liq-fade-v1 (IMPLEMENTED; FIRST REAL-DATA BACKTEST 2026-08-14)** —
  fade a liquidation cascade AFTER exhaustion: `liq.vol_*` spike + `liq.dist`
  stretch + rolling-sum drain vs peak (`strategies/liq-fade-v1/`, hypothesis +
  impl + 6 `lf_*` tests, registered as `sim backtest --strategy liq-fade-v1`).
  Honest history: with the hyperliquid-only corpus (no native liq stream) the
  08-12 BTC real-day run was trades=0 by construction — "no data — not
  falsifiable yet," never faked (determinism hash 14867537910696322766). The
  gate OPENED with the first bybit recording day (08-14, COL-29 deploy above).
  FIRST REAL-DATA RUN (08-14 bybit BTCUSDT slice 07:54→10:30 UTC, 463k
  events, 175 liquidations; runs `01M00SJJPNDVA…`/`…D` in `runs/index.jsonl`):
  trades=0, expectancy 0 — but the pipeline is PROVEN live, not dead:
  `liq.*` features fired on every print (materialized `liq.dist` 33–51 bps on
  all 175, mean 48 bps — the stretch condition is trivially met on bybit);
  the blocker is magnitude — rolling 5-min `liq.vol_buy` peaked at $467K vs
  the $1M `entry_vol` (buy-side cascade tape; `liq.vol_sell` near-absent, 4
  prints ≤ $15K). So: no qualifying cascade in the window, NOT a data-gate
  failure. Determinism re-proven on real data: identical decision-log hash
  8929519729917322587 across 4 runs (default + relaxed `entry_dist_bps`
  1/0.5/0.1). Open questions: bybit BTCUSDT liq prints are retail-sized
  (~$10–50K), so a single-venue/symbol $1M 5-min cascade may be rare —
  `entry_vol` venue calibration or multi-symbol aggregation is the v2
  direction (param grid already includes $500K). FULL-DAY VERDICT (closed
  08-14 bybit day, 584 MB, 2.59M events, gate `promotable: true`, sha256-
  verified local; run `01M01A640WJBGD9NTX7MP2Q61R`, decision hash
  17538781418735680594): trades=0 over the WHOLE day — 419 liq prints,
  14.8 h; `liq.vol_buy` rolling 5-min: p50 $109K, p90 $360K, p99 $510K,
  max $513,548, ZERO readings ≥ $600K all day; `liq.vol_sell` max $80,699
  (structurally one-directional tape). DECISION: yes — venue-calibrate
  `entry_vol` for bybit (≈$300K, ≈p90, keeps ~3 cascade events/day; the $1M
  floor is ~2× the venue's largest full-day flow, unreachable by
  construction, not by absence of cascades — the 13:11 UTC event peaked
  $513K and sustained >$300K for ~115 s, exactly the drain-exhaustion shape
  v1 fades). CALIBRATION IMPLEMENTED (2026-08-15): $250K/$300K added to the
  sim grid (entry_vol 250K–2M × dist 15/30/60 × exhaust 0.6/0.8/0.9 = 45
  combos). FIRST WALK-FORWARD (within-day, 5 windows, train=4h/test=4h/
  step=2h): all windows in_exp=0, oos=0 — DEGENERATE but honest: the grid
  argmax maximizes in-sample expectancy and every TRADING combo loses
  (250K/15/0.8 → trades=1, exp=−264), so the search picks a non-trading
  combo and OOS is vacuous. Probe replication (run
  `01M01MTATAWXTJP5GB29ZW02M0`) proves the strategy DOES trade on real bybit
  data at the calibrated floor: full 08-14 day → trades=3, expectancy −969,
  stress2x −1261 — 3 round trips, all losers. FALSIFICATION GATE: criterion
  #1 (expectancy ≤ 0 at 2× cost) ENGAGED on first real data. Caveat: 3
  trades / 1 day is a small sample — the multi-day walk-forward (≥2-of-3
  windows rule) fires once 08-15 closes + drains.
- **SIM-9 wf rerun with --min-trades (2026-08-15, run
  `01M037PPE2BJATDYBXHZ1DW7T2`)** — the degeneracy above is now
  machine-checked, not argued: `sim wf --min-trades 10` on the closed 08-14
  day → **5/5 VACUOUS** (no combo trades ≥10× in a 4h train slice; the fix
  refuses selection instead of the old false-pass first-combo/0.0 output);
  `--min-trades 1` → **1 SELECTED / 4 VACUOUS**, the selected window picking
  250K/15/0.8 with in_exp **−264** — the only combo that trades, selected
  with negative in-sample expectancy. Within-day walk-forward stays
  kill-direction; the cross-day (08-14+08-15) confirmation is now DONE
  (2026-08-16, run `01M044WTG50DEVKEM863NSBFF5`) — see the cross-day
  grade bullet below.
- **≥2-of-3 rule — machine-checkable grade procedure (2026-08-15)** — the
  falsification rule is now a deterministic computation, not a hand-argued
  read: `ops/scripts/grade_wf.py` + the exact procedure in hypothesis.md
  ("Grade procedure" section). Verified on the captured 08-14 `--min-trades
  1` run: 1 SELECTED (OOS-inconclusive, oos_trades=0), 4 VACUOUS →
  gradeable=0 → C3 NOT-GRADED; C1 ENGAGED (stress2x −1261) → **OVERALL
  KILL**. Synthetic boundary checks pass (G=3/F=2 fires; G=4 needs F≥3;
  G=4/F=2 does not fire). EXECUTED on the closed 08-15 leg (2026-08-16):
  see the cross-day grade bullet below.
- **≥2-of-3 cross-day grade — EXECUTED (2026-08-16, run
  `01M044WTG50DEVKEM863NSBFF5`)** — 08-15 closed + drained (gate
  `promotable: true`, 425 MB sha256-verified); `grade_wf.py` over both
  days' captured `--min-trades 1` legs (13 windows): 08-15 is **8/8
  VACUOUS even at min_trades=1** — the tape had **26 liq prints all day**
  (13 buy/13 sell, ~$25K notional) vs 419 on 08-14, so no combo trades;
  08-15 G1 (calibrated 250K/15/0.8) = **trades=0**, excluded from C1 by a
  new traded-day guard in `grade_wf.py` (SIM-9: a 0-trade day scoring 0.0
  is absence of evidence, never a degenerate ENGAGED). **OVERALL: KILL**
  via criterion #1 (08-14 stress2x −1261.13); **C3 NOT-GRADED**
  (gradeable=0 of 13 — the rule cannot be exercised at this trade
  frequency, as the funnel predicted); C2 NOT-GRADED (2-day corpus < 3).
- **liq-fade ≤3 trades/day — ROOT-CAUSED (2026-08-15,
  scratch funnel analysis)** — not gate tuning, cascade
  scarcity + structure: (1) sell side is absent on this tape (max sell
  episode $80,699 < every floor) so the long-fade leg is dead by
  construction; (2) only 3 of 23 buy cascades reach ≥$250K (day-max 5-min
  flow $513K → the $1M default is unreachable, 0 episodes ≥$1M); (3) the
  exhaustion anchor is a day-level peak never reset — full-day qualifying
  readings at 13:02/14:05 are anchored to the 08:19 peak, but the wf's
  window-local runs reset the peak and trade 0 on the same episodes; (4)
  dist is NEVER binding at 15/30bps (every qualifying vol reading already
  ≥30bps); (5) 71 of 419 readings die to dist-staleness (book-gap FEA-8
  silence). Verdict: within-day wf cannot certify an edge at ≤3 trades/day
  — the ≥2-of-3 rule needs multi-day windows (more trades per window) and
  more cascades/day (bybit ETHUSDT/SOLUSDT legs, spec 032; multi-venue v2
  aggregate), not smaller windows.
- **[v1.x] funding-carry study (DELIVERED 2026-08-13)** —
  `research/funding_carry_study_2026-08-13.md` + `mp-query carry` (spec 003
  §Analytics: OI-weighted funding + mark-vs-oracle basis per hour, units
  never mixed). Findings: 232 real hours over 7 days; funding positive ~2/3
  of BTC hours and 85% of ETH hours (mean +6.2%/+8.7% annualized) while the
  perp trades at a persistent mark-vs-oracle DISCOUNT (~-4 bps) — funding
  and basis diverge (BTC corr 0.72, ETH 0.40), i.e. the perp is cheap vs
  spot but longs still pay. Basis here is venue-oracle, not tradable spot
  (needs the spot leg item below).
- **[v1.x] funding-arb-v1** — cross-venue funding spread (long perp on
  negative-funding venue, short on positive) — carry-v1's sibling, needs two
  trading venues live. EVENT-STUDY GATE (2026-08-16, RES-4 batch 2):
  precondition MET — hyperliquid BTC funded ~+10.9%/yr vs bybit BTCUSDT
  ~+0.9%/yr on 08-14, a ~1080 bps/yr spread that persisted all 17
  overlapping hours (mean |spread| after 12h 1103); event-study CAR n=4
  windows on that single day, so NOT GRADABLE to a verdict yet. HYPOTHESIS
  WRITTEN (2026-08-16, `strategies/funding-arb-v1/hypothesis.md`) with the
  two-venue data requirement spec (FARB-1..6): ≥ 24h same-underlying
  overlap on ≥ 3 distinct days before the gate re-opens, cadence-aware
  annualization (HL ×8760 / bybit+binance ×1095, units never mixed), and
  the two-venue cost leg (fees + spreads) must clear ~1100 bps/yr before
  any backtest. RE-GATE r2 (2026-08-16, record `backlog-event-studies-
  2026-08-16-r2`): the 08-15 bybit BTCUSDT day drained — second distinct
  overlap day, same direction (HL pinned at +1095 cap 15/21h, bybit lagged
  90–1086), mean |spread| 512 bps/yr, 12h ≥ 500 threshold, but n=0
  complete windows (HL funding gap 11–13 UTC breaks every window).
  Verdict: precondition RECONFIRMED on 2 days, still NOT GRADABLE (FARB-2
  needs ≥ 3 days). Re-gate is one command when the next bybit days drain
  (ETHUSDT/SOLUSDT deploy + 08-16..) — the harness is pair-driven,
  `--run-id` distinguishes re-gate records.

  FINAL VERDICT (2026-09-13, run `farb2-backtest-2026-09-13`): **KILLED** by
  the pre-registered falsification. FARB-2 cleared (14 same-day pairs/symbol)
  and the full-cost backtest graded both registered configs: 58 episodes,
  expectancy −28.13/−28.16 bps at BASE costs (−57.1/−57.2 at the 2× kill
  column), win rate 0/58. Mechanism: the cross-venue spread mean-reverts
  within hours, so collectible carry (max 4.3 bps) never clears the
  two-venue fee+spread leg (29 bps RT) — FARB-5's "the honest bar is the
  spread at the exit" warned exactly this. Edge breadth was fine (6–8
  calendar windows, WF sign-flips 0/3 — consistently negative, not curve
  fit). Report: `docs/research/BACKTEST-funding-arb-v1-2026-09-13.md`.

- **[v1.x] basis-carry-v1** — dated-future vs perp/spot basis harvest where
  listed (OKX/Binance quarterlies). EVENT-STUDY GATE (2026-08-16, RES-4
  batch 2): dated-future leg NOT TESTABLE by construction (no quarterly
  collector); perp-vs-oracle proxy mean-reverts significantly on BTC
  (CAR[+24h] −0.64/−1.18 bps at |basis| ≥ 4/5 bps, CI excludes 0; n=62/17)
  but not on ETH (CI crosses 0) — evidence of tight BTC mark-oracle
  tracking, not a tradable edge (no spot leg). Parked on data.
- **[v1.x] oi-purge-continuation** — after quadrant-4 OI purges (longs
  flushed), momentum continuation entry; event-study first (RES-4).
- **[v1.x] listing-flow-v1** — new perp listings: systematic flow pattern in
  first days; screener + event study before hypothesis.
- **[v1.x] weekend-liquidity-v1** — regime-conditional risk-off/on around
  known thin-liquidity windows; likely a *filter* for other strategies rather
  than standalone.
- **[v2] cross-venue divergence arb** — needs inventory on both venues +
  transfer/inventory management spec; execution-sensitive, L2 fills minimum.
- **[v2] market-making on small venues** — REQUIRES L3 queue-position fill
  model spec + inventory risk spec; do not attempt with L1/L2 honesty.
- **[v2] vol/options overlay (Deribit)** — options collector spec, vol
  surface features, then covered structures around the spot/perp book.
- **[maybe-never] sub-second HFT anything** — blueprint §2 stands: no arms race.

## Sequencing note (2026-08-13) — data menu before more machinery

The system's code is two phases ahead of its roadmap: sim/OMS/risk/strategies
(Phases 3–7) exist while Phase 0 is unproven. The highest-value next work is
not building new collectors — it is making the machinery that exists *prove
itself daily* against the live corpus. Delivered 2026-08-13: `mp-ops status`
(OPS-16, one JSON truth surface), `mp-ops pipeline-stale` (OPS-17, P1
dead-man for the daily gate), spec 026 whole-day value-level veracity
(CVG-13..15: `price_divergence`/`trade_drought` — presence can't see silent
corruption), `scorecard --reuse-unchanged` (delta re-score, spec 024
sidecar), and spec 018 upgraded to ready (one runtime, four modes). Delivered
2026-08-13 (batch 2): **spec 018 MOD-9..11 implemented** — `mp-determinism`
replays yesterday's recorded session through the production runtime
(features -> strategy -> risk, SIM-5, same loader as the materializer) and
requires a byte-identical decision log; the PASS artifact
(`<date>.determinism.json`) is a promotion-gate condition, wired fail-closed
into both daily pipelines (VPS cron step 1.5, Windows task), verified
byte-identical on the real 08-10..08-13 corpus (843k events / 854k decision
lines per day).Delivered
2026-08-13 (batch 3 — Cryexc/OpenMarket deep-dive
implementation): **read-time analytics transforms** — `mp-query footprint`
(block-bucketed (interval × price) order-flow grid over cold Parquet, the
OpenMarket blockSize/heatmap aggregation) and `mp-query oiwa` (OI-weighted
funding Σ(rateᵢ·oiᵢ)/Σoiᵢ, units never mixed), plus the `book.depth.{pct}` /
`book.depth_total.{pct}` liquidity-band features (0.5/2/10% of mid) and
`tape.tps.{tf}` / `tape.bps_delta` tape micro-features — all tested
(analytics + `om_*` acceptance tests) and live-smoked on the real corpus.
Delivered 2026-08-13 (batch 4 — bybit liq deploy, staged-on-VPS):
**bybit:BTCUSDT joined the VPS promotion gate** (COL-29 follow-up). The
collector's bybit subscribe list gained `orderbook.50.{symbol}` — WITHOUT it
(the deployed binary's blind spot) a bybit recording would lack the gate's
required `book` stream and fail every day. Rebuilt both binaries on the VPS
(mp-collector 8.9 MB, mp-ops 14.3 MB) and verified live: the fresh mp-ops
venue-scoped `--require-stream bybit:liquidation` is ignored for hyperliquid
(clean 0 blocking) while a literal bogus stream blocks — the COL-29 gate
works as spec'd. Bybit WS egress confirmed from the VPS (HTTP 101). The
install itself (binary → `/opt/money-printer/bin`, unit
`mp-collector@BTCUSDT`, gate script `RECORDINGS`) is staged at
`/home/mp-egress/deploy_bybit.sh` — **DEPLOYED 2026-08-14** (root-approval
step completed; the fix build with the `allLiquidation.` topic was the
missing piece — the first deployed binary used the dead `liquidation.` topic,
which made Bybit reject the whole subscribe frame, so the unit connected and
heartbeated but recorded nothing). Today's log
(`data/raw/20260814_bybit_BTCUSDT.log`) now records real events —
`liq.*` features and `liq-fade-v1` finally have their first real data day.

Still on the menu, in priority order:
- **[v1.x] veracity event study (RES-4)** — DELIVERED 2026-08-13
  (research/veracity_study_2026-08-13.md): detector output on the cold corpus
  is 0+0 (no cohort); the only real overlap day (08-08) shows 0/15
  `price_divergence` (feeds track to 0.02%, 70-250x inside the 5% band) and
  ~10 structural `trade_drought` flags that are all print-granularity noise;
  the real corruption (07-19/07-21 cross-stream leaks, provenance-less
  format) was caught at the INT-4 ingress gate. Action: trust
  `price_divergence` in read paths; do not gate `trade_drought` on a
  cross-venue count median without venue-relative baselines.
- **[v1.x] off-host backup tier** — ✅ DELIVERED 2026-08-15 as
  `ops/scripts/offhost_backup.ps1` + `offhost_restore_drill.ps1` (deploy.md
  §7): per-file age-encrypted corpus + state pushed incrementally via rclone
  (vendored portables in `ops/tools/`, keypair in `ops/keys/`), fail-closed
  preflight, live-file skip (exit 10), quarterly restore drill proves
  pull→decrypt→size-verify→sim-golden end-to-end (verified local-only via
  `localdrill:offhost`, byte-identical restore). Owner TODO: pick a real
  cloud remote (`rclone config`), store the age PRIVATE key off-host, run
  `-Register` for the 02:30 UTC task.
- **bybit ETHUSDT + SOLUSDT deploy (spec 032 interim path)** — ✅ DEPLOYED
  2026-08-16 01:52 UTC: `deploy_bybit_multi.sh` on the VPS enabled
  `mp-collector@ETHUSDT`/`@SOLUSDT` + added both to the gate's RECORDINGS
  and grew swap (`swapfile2` 8 GiB); all three units verified active +
  recording (ETHUSDT/SOLUSDT partial first day ~3h, gate accepts partials
  per the 08-14 precedent). The script self-elevates (mp-egress is in
  `google-sudoers`, GCP NOPASSWD — root SSH with the egress key is denied,
  so the guard is how VPS deploys run); idempotent, re-runnable. The 08-14
  nightly gate's determinism OOM (exit 137) is now protected by the swap
  headroom for the 5-log gates.
- **VPS relay never accumulates** — ✅ DELIVERED 2026-08-14 as
  `ops/scripts/vps_drain.ps1` (task `MoneyPrinterVpsDrain`, 01:00 UTC, after
  the 00:05 gate + 00:30 backup): closed day-files move into the Windows
  master `data/raw` sha256-verified, then the VPS copy is released
  (delete-only-when-verified, owner-approved W-6 exception). Collision-aware:
  a name already in `data/raw` with different content (the A-B overlap) is
  never overwritten or released until the handoff. Guards: staged tar stream,
  per-file sha256 verification, re-hash right before deletion. This is the
  operationalization of the OPS-15 relay cap's "VPS accumulates" branch —
  the 15 GB VPS budget is now a tripwire for drain failure, not normal
  growth. Release-leg bug fixed 2026-08-16: `~/vps_drain_release.sh` could
  not delete (raw/ is printer-owned 755, mp-egress not in printer group) —
  every nightly release since shipping aborted with `rm: Permission denied`,
  and PS 5.1 EAP=Stop turned the remote stderr into a terminating error that
  skipped the manifest (LastTaskResult 1 nightly). Fixed via self-elevation
  in the release script + EAP override + `$LASTEXITCODE` check in
  `vps_drain.ps1`; the backlogged bybit 08-14/08-15 VPS copies were then
  released and verified. Manifest entries now carry per-file release state
  (`release`: released | skipped:<reason> | ssh_failed | no_release | kept) so
  a silent release failure is visible in the audit trail, not just the log
  (tested via the script's `-VpsBase`/`-MasterRaw` scratch harness: landed →
  released, collision → kept, already-landed-identical → released). The
  storage-budget watch now CONSUMES that field: `mp-ops storage-budget
  --manifest data/vps_drain_manifest.jsonl` (passed by the Windows
  daily_pipeline hook; the VPS timer has no manifest) fires the same
  storage-budget P2 when any file's LATEST entry is action=landed with
  release not in {released, no_release} — a silently held VPS file fails
  loudly instead of living only in the drain log (`held_drain_files` in
  ops/src/storage.rs; latest-entry resolution so a later successful release
  clears an earlier failure; fail-closed on an unreadable manifest).
  Slow-link economy: the drain now consults the master corpus + manifest
  BEFORE the pull (`vps_drain_pull.sh` takes an include list) — byte-
  identical release re-attempts and KNOWN A-B collisions (latest manifest
  entry action=collision, same VPS sha256) are never re-transferred, so the
  389 MiB hyperliquid A-B backlog now moves ZERO bytes/night until the
  handoff (real run: 8 known collisions skipped, complete in 1s vs ~25 min;
  master-side hash stays authoritative — a deleted master file re-pulls,
  verified via scratch runs). A-B
  verdict IN (2026-08-16): 4 overlap days
  (08-12..08-15) — Windows still bursts daily (cov 0.84-0.97, 3.5h gap on
  08-15) while the VPS is clean (cov 1.0, promotable all 4 days) → egress
  confirmed → the §6 Handoff is DUE: the VPS hyperliquid units are the
  canonical recorder and stay (gate-required in RECORDINGS); the WINDOWS
  recorder stops, ending the ~130 MB/day accumulation + nightly 389 MiB
  collision re-transfer at the source (runbook §6 verdict + Handoff; handoff
  execution pending owner go).

## Data & features
- **[v1.x] read-time analytics transforms** — ✅ DELIVERED 2026-08-13 as
  `mp-query` (spec 003 §Analytics): `footprint` (block-bucketed (interval ×
  price) order-flow grid over cold Parquet) and `oiwa` (OI-weighted funding,
  units never mixed). Compute-on-read (Cryexc/OpenMarket pattern) — raw
  points stored once, research views derived on demand.
- **[v1.x] liquidity-band + tape features** — ✅ DELIVERED 2026-08-13 as
  `book.depth.{pct}` / `book.depth_total.{pct}` (0.5/2/10% of mid) and
  `tape.tps.{tf}` / `tape.bps_delta` (spec 004 catalog, `om_*` tests).
- **[v1.x] liquidation-flow features** — ✅ DELIVERED 2026-08-13 as
  `liq.vol_buy`/`liq.vol_sell` (rolling notional by side), `liq.rate`
  (rolling event rate), `liq.dist` (liq price-distance from mid, bps) +
  `liq.delta.{a}_{b}` cross-venue pressure divergence — the first features
  over the COL-29 real liquidation stream (spec 004 §Liquidation flow,
  `liq_*`/`liq_delta_*` + `mat_6_liq_flow_*` tests). `liq.delta` shipped
  the engine seam it needed: **`register_global_tick`** (spec 004 FEA-20) —
  one instance sees every event, because the per-symbol state model can
  never hold cross-venue state (the reason `px.divergence`/`leadlag`/
  `cvd.agg` were never implemented — now unblocked). Strategy-visible in
  the sim; no-op on hyperliquid (no native liq stream) until a bybit
  recording joins. Natural next: the cross-venue liq-delta divergence as an
  input to the liq-fade-v1 cascade filter.
- **[v1.x] liquidations leg** (Cryexc/OpenMarket gap) — ✅ DELIVERED
  2026-08-13 as COL-29 (spec 024): the `Liquidation` event has a real
  source. Ground truth verified live: Binance's public liq paths are all
  dead from this egress (WS `forceOrder` dropped; `allForceOrders` REST is
  USER_DATA, 404s without creds), so the **live source is Bybit's public
  `allLiquidation.` WS topic** (normalizer pinned by `col_29_bybit_*` tests,
  config example shipped). Binance's signed `allForceOrders` leg
  (`liq_source="rest"`, HMAC, order-id dedup + update-time resume,
  FILLED-only) is wired and dead-until-creds (`MP_BINANCE_API_KEY`/
  `MP_BINANCE_API_SECRET`). The audit gate judges provenance via
  venue-scoped `--require-stream binance:liquidation` / `bybit:liquidation`.
  Remaining gap: Hyperliquid has no native liq feed — its liquidation data
  stays on the on-chain whale census (spec 028).
- **[v1.x] history-server protocol (BYOD)** — serve the corpus over a thin
  `start_ms/end_ms/limit` HTTP protocol (Cryexc history-spec pattern) so the
  determinism replay queries history exactly like the live loop. `mp-query`
  covers the data path; an HTTP layer needs a server dep — deferred until the
  replay seam needs it.
- **[v1.x] more venues** (add-venue skill): OKX (checksummed books), Coinbase
  + Kraken (spot cross-check), Hyperliquid (complete liq visibility).
- **[v1.x] multi-symbol collector fan-out** — ✅ SPEC'D 2026-08-15 as spec
  032 (MSC-1..10): one process per venue, N symbols, per-symbol logs (every
  consumer keeps its per-symbol contract), all-or-nothing locks, per-symbol
  rotation/health; `--symbols` entry point, `--symbol` preserved as the
  degenerate case. Not yet implemented — the bybit ETHUSDT/SOLUSDT deploy
  (see VPS relay note) uses the existing per-symbol template units as the
  interim path (deploy script staged: `deploy_bybit_multi.sh`).
- **[v1.x] spot venues for basis truth** — perp-vs-spot features need spot legs.
- **[v1.x] liquidation-level estimator** — OI + leverage-tier assumptions →
  projected liq bands; overlay for liq-fade and the terminal. ✅ SPEC'D +
  BUILT as spec 029 `liq.est_bands`; validated against spec 028 real liq
  prices via the RES-4 `whale_study` study (band accuracy, SIM-10-journaled).
  Remaining: calibrate tier weights from recorded real leverage.
- **[v1.x] orderflow dataset exports** — clean labeled Parquet extracts
  (events + forward returns) as the ML substrate, from the feature store.
- **[v1.x] positioning collectors (whale tracking, tier 1)** — poll the free
  public positioning endpoints: Binance/Bybit/OKX top-trader long/short
  ratios and global account ratios. Needs an additive `Positioning` event
  variant (schema amendment → owner sign-off), then passthrough features +
  z-scores. Evidence as contrarian signal is mixed — record, grade via RES-4,
  promise nothing.
- **[v1.x] Hyperliquid whale position tracking (whale tracking, tier 2 — the
  unique one)** — positions there are public on-chain: collector for large
  positions (entry, size, leverage, liquidation price), then features:
  aggregate whale net positioning + deltas, whale liquidation-level bands
  from REAL positions (upgrades liq-fade-v1 context vs leverage-assumption
  bands), and wallet-cohort grading (score wallets by realized PnL from our
  recorded history; cohort flow becomes a feature only after its event study
  clears). ✅ SPEC'D + BUILT as spec 028 (collector + `WhalePosition` event +
  cold `positions/` stream); real liq prices now grade spec 029's bands via
  the  RES-4 `whale_study` study (WHL-5: data → graded feature → strategy).
  Aggregate whale net positioning + deltas BUILT as `whale.net.{venue}` /
  `whale.delta.{venue}` in the feature engine (004), gated on the RES-4 study
  (spec 028 Decisions 2026-08-05). Remaining: wallet-cohort grading (after
  the event-study gate). Explicitly NOT copy trading — see rejected list.
- **[v2] on-chain collectors (whale tracking, tier 3)** — stablecoin flows,
  exchange wallet balances, dormant-wallet awakenings, bridge flows; new
  source class, own spec (rate limits, providers, trust). Noisiest whale
  tier: custody shuffles and MM rebalancing masquerade as signals — event-
  study gate mandatory.
- **[v2] news/social ingestion + narrative tracker** — deliberately excluded
  from 010 V1 (determinism); needs its own spec: sources, dedupe, archival,
  and the grounding contract extension.
- **[v2] options/vol surface data** (pairs with Deribit strategy item).
- **[v1.x] DeFiLlama regime collector — ✅ SPEC'D 2026-08-26 as spec 046
  (DEF-1..8; draft).** Free keyless stablecoin-supply delta + TVL + DEX-volume
  regime pillar (third macro pillar beside FRED 030 / netflow 034). Best RoI
  in the external-data survey (`research/external_data_sources_findings.md`),
  classified *regime/context, not alpha*. Awaiting owner sign-off on event-
  variant shape (reuse `MacroPoint` vs new `DeFiSnapshot`, DEF-2) + new-host
  sign-off before implementation. Implement after Phase-0 gate.
- **[v1.x] Coinalyze cross-exchange validation collector (external SPEC'D
  2026-08-26 as spec 047, COZ-1..10):** real cross-exchange OI / funding /
  liquidation / long-short to **validate** `oi_regime` (045) + `liq_est_bands`
  (029). Explicitly NOT "predicted-funding alpha" (public telegraphy, COZ-9);
  NOT a long intraday history source (COZ-10). Written to a separate
  `cold/external/coinalyze/` namespace (W-6).
- **[v1.x] Cross-asset correlation feature (external SPEC'D 2026-08-26 as spec
  048, COR-1..7):** BTC-ETH-Gold-SPX rolling correlation **from our own recorded
  prices** + FRED (moat-first, zero external dep for the crypto leg). Regime
  conditioner for the allocator/regime detector, not a standalone signal.

## Simulation & research
- **[v2] L3 queue-position fill model** — unlocks maker strategies; spec must
  define queue estimation from trade flow + conservative bounds (SIM
  Decisions note holds it).
- **[v2] ML research spec** — purged/embargoed CV, feature importance with
  leakage tests, model registry, and the rule that models emit *features*
  consumed by ordinary strategies (never raw orders). Gate: only after
  Phase 3 machinery proves one non-ML edge end-to-end.
- **[v1.x] capacity/impact study harness** — estimate strategy capacity from
  L2 depth history before scaling (feeds G4→scale decisions).
- **[v1.x] cross-strategy correlation monitor** — live rolling correlation
  matrix feeding the allocator's corr_penalty with alerting on convergence.
- **[v1.x] regime-model upgrade** — HMM as a *shadow* regime feature next to
  the threshold ensemble; adopt only if it improves allocator outcomes in
  walk-forward.

## Execution & risk
- **[v1.x] execution algos** — TWAP/iceberg intent kinds in OMS for larger
  entries (spec 007 amendment; needed before any scaling past top-of-book size).
- **[v1.x] fee-tier awareness** — venue fee schedule by rolling volume in the
  cost model and live accounting.
- **[v2] real cross-strategy netting & portfolio margin** — spec 006 open
  question; revisit when margin efficiency costs real money.
- **[v2] multi-region failover** — second VPS, warm standby, split-brain
  rules (who may trade?); only after Phase 6.
- **[v2] security hardening spec** — threat model (key theft, VPS compromise,
  supply chain), key rotation runbook, withdrawal-address allowlisting at
  venue level, dependency audit cadence.
- **[v1.x] bincode 2 migration** — ~~bincode 1.3.3 is unmaintained~~
  IMPLEMENTED 2026-08-14 as a codec amendment to spec 001 (✅): workspace
  swapped to `bincode-next` 3.x + `config::legacy()` (byte-identical to
  bincode 1's
  default format → no data migration, originals untouched); golden-bytes
  tests (BDC-1/2/3) pin the identity; `cargo audit` now reports only the
  `paste` RUSTSEC-2024-0436 debt (RUSTSEC-2025-0141 cleared); real-corpus
  readback of pre-swap logs matches (bdc_6). `rust-version` raised 1.80 →
  1.90 (bincode-next 3.x MSRV).

## Ops & reporting
- **[v1.x] relocate workspace out of ~/Downloads + external-volume backup** —
  the whole tree (including `data/`) lives under `~/Downloads` on a single C:
  drive (audit 08-10; the repo folder itself is double-nested). W-6 says keep
  recorded data on a backed-up drive. `backup_data.ps1` already mirrors to
  `C:/mp-backup` nightly, but that is the SAME physical volume — a dead disk
  takes both. Move the workspace to `C:\mp` (or another drive) and point the
  three Scheduled Tasks at the new path, then set the backup `-Destination` to
  a separate physical drive or network share.
- **[v1.x] tax/accounting export** — fills journal → per-jurisdiction lot
  report; boring, mandatory, cheap to spec early.
- **[v1.x] soak-test farm** — long-running mock-venue chaos environment that
  replays recorded chaos patterns nightly against collectors/OMS.
- **[v1.x] data-quality dashboard** — manifests visualized; coverage trends
  per venue/stream (catch slow rot before it poisons research).
- **[v2] equities/futures expansion (IBKR/CME)** — new asset class, market
  hours, different data economics; entire spec family; only after crypto loop
  compounds.

## Agent infrastructure (extends docs/AGENT_FORCE_MULTIPLIERS.md roadmap)
- **[v1.x] event-study-first alpha gate** — ✅ DELIVERED 2026-08-15 as
  `research/run_backlog_event_studies.py` (RES-4): grades backlog ideas over
  the corpus BEFORE any hypothesis (hourly mark/OI via `mp-query carry`,
  cross-asset excess, seeded CI, honest NOT TESTABLE verdicts). First batch
  verdicts (record `backlog-event-studies-2026-08-15`): oi-purge-
  continuation NOT GRADABLE (n=1 quadrant-4 purges in 8 days — too few
  events, revisit on purge sessions); listing-flow-v1 NOT TESTABLE (no
  listing feed subscribed — needs a screener first); weekend-liquidity-v1
  REJECTED on hyperliquid (weekend NOT thinner: |excess| vol lower, basis
  only 0.17 bps wider — do not re-propose without a different venue). Batch
  2 (record `backlog-event-studies-2026-08-16`): funding-arb-v1 precondition
  MET but NOT GRADABLE (n=4 windows on the single 08-14 bybit overlap day;
  ~1080 bps/yr persistent spread) — re-gate on multi-day bybit overlap;
  basis-carry-v1 dated-future leg NOT TESTABLE (no quarterly collector),
  perp-vs-oracle proxy mean-reverts on BTC only (mark tightly oracle-
  anchored).
- **[v1.x] two-agent review flow** — implementer agent + fresh-context
  reviewer agent checking diff against spec; wire as a skill.
- **[v1.x] mutation testing** on risk gate + sizing (cargo-mutants) once
  those crates exist.
- **[v1.x] requirement-coverage report** — CI artifact listing every spec ID
  → implementing tests (extends guardrails' implemented-spec check to a
  human-readable matrix).
- **[v1.x] session-start hook** — auto-run guardrails + `cargo test` summary
  at agent session start so every agent begins knowing the tree's health.

## Explicitly rejected (don't re-propose without new evidence)
- **Copy trading** — mirroring individual whale wallets/accounts blind.
  Whale data enters this system only as features graded by event studies
  (spec 004 Decisions). Copying imports someone else's risk process without
  their exits, sizing, or information — and the best wallets stop working
  the moment they're crowded.
- Live-decision LLMs (010 Decisions) — human-read only.
- Touch-fills-at-limit-price backtesting (SIM-2 trade-print rule is law).
- Sizing from backtest trades (RSK-3 — live trades only feed Kelly).
- Committing any recorded data to git (W-6; data lives outside the repo).
