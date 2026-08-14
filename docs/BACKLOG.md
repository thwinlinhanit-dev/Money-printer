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
- **[v1.x] liq-fade-v1 (IMPLEMENTED, NO DATA YET 2026-08-13)** — fade a
  liquidation cascade AFTER exhaustion: `liq.vol_*` spike + `liq.dist`
  stretch + rolling-sum drain vs peak (`strategies/liq-fade-v1/`,
  hypothesis + impl + 6 `lf_*` tests, registered as
  `sim backtest --strategy liq-fade-v1`). HONEST DATA GATE (written into the
  hypothesis BEFORE implementation): `liq.*` only fires on a recording with
  a liquidation stream — the hyperliquid corpus has none, so the real-day
  backtest (08-12 BTC, seed 42) is trades=0 by construction, recorded as
  "no data — not falsifiable yet," never faked. Determinism still proven
  (two same-seed runs byte-identical, hash 14867537910696322766). The gate
  opens when a bybit recording day exists (deploy item below).
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
  trading venues live.
- **[v1.x] basis-carry-v1** — dated-future vs perp/spot basis harvest where
  listed (OKX/Binance quarterlies).
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
`/home/mp-egress/deploy_bybit.sh` awaiting root approval — the last step
before `liq.*` features and `liq-fade-v1` get their first real data day.

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
- **[v1.x] off-host backup tier** — the nightly VPS→Windows pull protects
  against VPS loss but not the PC's disk; deploy.md §7 rclone/age remains the
  true off-host tier.

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
  from 010 v1 (determinism); needs its own spec: sources, dedupe, archival,
  and the grounding contract extension.
- **[v2] options/vol surface data** (pairs with Deribit strategy item).

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
