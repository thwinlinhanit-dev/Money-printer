# Backlog — every idea, so nothing lives only in a chat

The idea inventory. Rules: new ideas land here first (a line is enough);
nothing gets built from here without a spec (PD-6); nothing here jumps the
ROADMAP queue without the owner saying so. Items are grouped by theme and
tagged **[v1.x]** (fits current architecture), **[v2]** (needs a design
decision), or **[maybe-never]** (recorded so it stops being re-proposed).

## Strategies & alpha (each needs hypothesis.md first — spec 006)
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

## Data & features
- **[v1.x] more venues** (add-venue skill): OKX (checksummed books), Coinbase
  + Kraken (spot cross-check), Hyperliquid (complete liq visibility).
- **[v1.x] spot venues for basis truth** — perp-vs-spot features need spot legs.
- **[v1.x] liquidation-level estimator** — OI + leverage-tier assumptions →
  projected liq bands; overlay for liq-fade and the terminal.
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
  clears). Needs its own small spec: API surface, wallet identity handling,
  storage layout. Explicitly NOT copy trading — see rejected list.
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

## Ops & reporting
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
