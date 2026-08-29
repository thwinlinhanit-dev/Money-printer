# External Data Sources — Research Findings

**Date**: 2026-08-26
**Status**: Research complete, awaiting integration decisions — **revised 2026-08-26** (moat-first re-ranking: DeFiLlama elevated to #1, OpenMarket demoted to research-only, predicted-funding "alpha" claim corrected, legal/PD-2 section added)
**Scope**: All free/cheap external data sources worth integrating into the Money Printer trading research lab

---

## Executive Summary

Researched 25+ platforms across the crypto data ecosystem. Identified a set of **free** data sources that fill real gaps in the current pipeline. All recommended integrations are **$0**, but note: **cost is not the binding constraint — engineering hours and Phase-0 promotion are.** This list is deliberately trimmed to *best value first*, and every source is tagged by whether it is a **validation** of an existing model or a genuinely new **signal**. Hoarding inputs before the Phase-0 gate passes is scope creep — most of these belong in `docs/BACKLOG.md` until the funnel is operating.

The three highest-impact additions:
1. **DeFiLlama** — stablecoin-supply delta, DeFi TVL risk-on/off, DEX volume (a genuine new macro/regime pillar — feeds the allocator/regime detector)
2. **Coinalyze** — cross-exchange OI, funding, liquidations, long/short ratio (**validates** `oi_regime` (045) and `liq_est_bands` (029); long/short is a genuinely new sentiment feature)
3. **Own-recorded cross-asset correlation** (BTC-ETH-Gold-SPX from our own prices) — compounds our retained-moat asset with zero external dependency

**Frank note on OpenMarket:** previously listed as #3. It is **demoted** in this revision (see §3): it is a 7-day-history, hourly-cadence aggregator whose core data (OI/funding/liquidation/depth) overlaps what we already collect or what Coinalyze provides free. Its TPO/Market-Profile is an eyeball cross-check, not infrastructure. Its one genuinely differentiated feature — **Polymarket analytics / smart-money wallets** — was not even identified in v1 and is the only place OpenMarket earns priority.

---

## Current Data Architecture (Baseline)

The app currently collects from:
- **Hyperliquid** (live WS): trades, book, OI, mark price, funding, liquidations, whale positions
- **Bybit** (live WS, in progress): trades, book, OI, funding, liquidations
- **Binance** (credential-gated, in progress): futures REST/WS
- **Deribit** (live): options tickers, trades — feeds Greeks/IV/flow analytics
- **Etherscan** (REST, daily): exchange reserve USDT balances → netflow velocity
- **FRED** (REST, daily): macro economic observations (CPI, rates, etc.)
- **Hyperliquid whale census** (REST, 60s cadence): top-N leaderboard positions

**Known gaps** (from features.toml comments and spec 004):
- `book_depth` references "Cryexc/OpenMarket depth stats" — currently only Hyperliquid L2
- `tape` references "OpenMarket tape" — currently only Hyperliquid trade prints
- `liq_flow` and `liq_delta` are no-op on Hyperliquid (no native liq stream)
- `liq_est_bands` models liquidation zones but cannot validate against aggregated real data
- `oi_regime` uses only Hyperliquid OI — no cross-exchange context
- No sentiment, no DeFi/macro, no cross-asset correlation data

---

## TIER 1: MUST ADD — Free, High Signal, Direct Fit

### 1. Coinalyze API

| Field | Detail |
|---|---|
| **URL** | `https://api.coinalyze.net/v1/doc/` |
| **Cost** | FREE (40 req/min, free signup) |
| **Auth** | API key (header or query param) |
| **Citation** | Required in public use |
| **Data types** | Aggregated OI, funding rate, **predicted funding rate**, liquidations, long/short ratio, OHLCV |
| **Exchanges** | Binance, Bybit, OKX, Deribit, BitMEX, and more |
| **Granularity** | 1min to daily, historical |
| **Rate limit** | 40 calls/min per API key |
| **Intraday retention** | 1500–2000 datapoints (old data deleted daily) |
| **Daily retention** | Unlimited |

**Key endpoints**:
- `GET /v1/open-interest` — current OI per exchange (max 20 symbols/call)
- `GET /v1/open-interest-history` — historical OHLC of OI
- `GET /v1/funding-rate` — current funding rate
- `GET /v1/funding-rate-history` — historical funding
- `GET /v1/predicted-funding-rate` — next funding payment (public telegraphy — see caveat; carry *context*, not alpha)
- `GET /v1/liquidation-history` — historical liquidation longs/shorts
- `GET /v1/long-short-ratio-history` — long/short ratio per exchange
- `GET /v1/ohlcv-history` — OHLCV candles with buy/sell volume

**Integration points** (mostly *validation*, not new alpha):
- Cross-exchange OI validates `oi_regime` (spec 045) — currently Hyperliquid-only
- `liquidation-history` provides ground truth for `liq_est_bands` validation (RES-4) — a real, testable win we cannot get on Hyperliquid (no native liq stream)
- `long-short-ratio-history` is a genuinely **new sentiment feature** the app doesn't have
- `funding-rate-history` across exchanges enables funding-arbitrage *research* (backlog idea `funding-arb-v1`) — research input, not a live edge

**⚠️ Honest caveat on "predicted funding rate":** the original draft called this a "novel alpha signal." It is **not alpha** — the next funding payment is public telegraphy shown by every venue and helper before settlement, so it is priced in by definition. There is no edge in *knowing* it. Keep it as a **context/feature** (useful for carry monitoring), never as a strategy thesis.

**⚠️ Retention caveat:** intraday history is only ~1500–2000 datapoints (old data deleted daily); only daily-resolution is unlimited. So Coinalyze is for **validating/contextualizing** our live regime logic — it is **not** a source for building a permanent multi-year intraday carry backtest. For long-horizon history, use CryptoDataDownload (§8) or keep recording (W-6, our own moat).

**Concrete implementation**:
- New binary: `mp-coinalyze` (REST poller, hourly cadence)
- New event type or extend `NetflowSnapshot` with `DerivAggSnapshot`
- Store: OI, funding, predicted funding, liq, long/short per exchange per symbol
- 40 req/min is sufficient for hourly snapshots of BTC + ETH across ~5 exchanges (10 calls/hour)

---

### 2. DeFiLlama API — **TOP PICK (regime/signal, free)**

| Field | Detail |
|---|---|
| **URL** | `https://api.llama.fi/` (31+ endpoints) |
| **Cost** | FREE |
| **Auth** | None required |
| **Rate limit** | Not published (community reports ~50–100 req/min) |
| **Data types** | TVL, stablecoins, yields, DEX volume, fees/revenue, bridges, coin prices |
| **Coverage** | 6000+ protocols, 500+ chains |

**Key endpoints**:
- `GET /stablecoins` — all stablecoins with circulating supply + peg data
- `GET /stablecoins/{coin}` — per-stablecoin historical supply
- `GET /tvl/{protocol}` — historical TVL per protocol
- `GET /v2/historicalChainTvl/{chain}` — chain-level TVL history
- `GET /pools` — yield pool data across all DeFi (APY, TVL)
- `GET /dexs/{protocol}` — DEX volume per protocol
- `GET /fees/{protocol}` — protocol fees/revenue
- `GET /bridges` — bridge TVL and cross-chain transfers
- `GET /coins/prices/current/{coins}` — CoinGecko-based prices

**Why this is #1** (best ROI-per-engineering-hour in the whole survey):
- The strongest thesis in this document: **USDT/USDC stablecoin-supply delta is a known leading indicator for BTC** — minting often precedes rallies. Combine with TVL (risk-on/off) and DEX volume (on-chain activity gauge) and you get a genuine **third macro/regime pillar** beside FRED (030) and exchange reserves (034).
- This is exactly the kind of feed the **allocator/regime detector (blueprint §7)** was designed to consume.
- **Free, no auth, ~5 calls/day.** Significantly cheaper and cleaner than any derivative aggregator for this job.

**Honest tag:** mostly **regime/context signal** (not per-trade alpha) — use it to *condition* strategies and de-weight positions in risk-off regimes, not as a standalone entry signal.

**Concrete implementation**:
- New binary: `mp-defillama` (REST snapshotter, daily cadence)
- New event type: `MacroDeFi` or extend `MacroPoint` variants
- Store: total DeFi TVL, stablecoin supply (USDT/USDC), top-10 DEX volumes
- ~5 API calls per daily snapshot (stablecoins, TVL aggregate, top protocols)
- **Route to `docs/BACKLOG.md` now** and implement after the Phase-0 promotion gate.

---

### 3. OpenMarket API — **DEMOTED (was listed as #3; now Tier-2/research only)**

| Field | Detail |
|---|---|
| **URL** | `https://api.openmarket.xyz/v1/points` |
| **Cost** | FREE (10 weight-min/min) |
| **Auth** | API key |
| **History** | 7 days (free tier) |
| **Data types** | Candles, OI, funding, liquidations, volume profile, TPO, orderbook heatmaps, liq heatmap |
| **Exchanges** | 20+ exchanges, unified schema |
| **Unique** | TPO/Market Profile (POC, VA, poor highs/lows, single prints), Hyperliquid liquidation cascade heatmap, **Polymarket analytics / smart-money wallets** |

**Honest demotion rationale**: this is a 7-day-history, hourly-cadence, weight-limited aggregator (10 w/min). Its core data — cross-exchange OI/funding/liquidations/depth — **overlaps** what we already collect live on Hyperliquid/Bybit/Binance, or what Coinalyze (§1) provides free with 40 req/min. The TPO/Market Profile is an **eyeball cross-check** of our own `swing.value_area` (036), not infrastructure. Low throughput + low compounding value ⇒ it does **not** belong in the "highest-impact" trio.

**Key endpoints**:
- `GET /v1/points?type=TRADE_SIDE_AGNOSTIC_AGG` — cross-exchange candles (weight: 1x)
- `GET /v1/points?type=OPEN_INTEREST_AGG` — aggregated OI (weight: 1x)
- `GET /v1/points?type=FUNDING_RATE_AGG` — aggregated funding (weight: 1x)
- `GET /v1/points?type=LIQUIDATION_AGG` — aggregated liquidations (weight: 1x)
- `GET /v1/points?type=VOLUME_PROFILE_AGG` — POC, VA, HVN, LVN (weight: 2x)
- `GET /v1/points?type=TPO_AGG` — TPO Market Profile (weight: 5x, exclusive)
- `GET /v1/points?type=BLOCK_BOOK_SNAPSHOT_AGG` — 1000-level orderbook depth (weight: 10x)
- `GET /v1/points?type=HYPERLIQUID_LIQUIDATION_AGG` — HL liq cascade zones (weight: 5x)
- **`GET /v1/polymarket*` (Polymarket analytics / smart-money wallets)** — the *only* genuinely differentiated feature; **not part of the v1 doc's use case and worth adding.**

**Integration points** (narrowed to the differentiated value):
- **Polymarket smart-money wallet tracking** (win-rate, size, timing; live positions) — the one thing none of our other sources provides. This is where OpenMarket earns priority, *if anywhere*.
- TPO / `HYPERLIQUID_LIQUIDATION_AGG` / `BLOCK_BOOK_SNAPSHOT_AGG` — **optional eyeball cross-checks** only (validate `swing.value_area` 036, `liq_est_bands` 029), NOT primary data.
- Do **not** build `book_depth`/`tape`/OI/funding collection off OpenMarket — we collect those ourselves live; that's our moat.

**Concrete implementation**:
- **Defer.** New binary `mp-openmarket` only if/when the Polymarket wallet feed is pursued (research phase). Weight budget: 10 w/min allows ~6 hourly calls/min.
- 7-day history limit means this supplements, never replaces, live collectors.

---

### 4. Alternative.me Fear & Greed Index

| Field | Detail |
|---|---|
| **URL** | `https://api.alternative.me/fng/` |
| **Cost** | FREE, no auth |
| **Data** | Daily Crypto Fear & Greed Index (0–100) + historical |
| **Components** | Volatility (25%), volume (25%), social media (15%), dominance (15%), trends (10%) |

**Integration points**:
- Simple sentiment gauge — extreme fear (<25) historically correlates with bottoms, extreme greed (>75) with tops
- 1-line REST call in existing `mp-macro` binary
- New `MacroPoint::Sentiment` variant

**Concrete implementation**:
- Add to `mp-macro` (Rust, ~10 lines)
- `GET https://api.alternative.me/fng/?limit=1` → parse `value` field
- Store as `MacroPoint` with series "fear_greed_index"

---

### 5. Sharpe Terminal Correlation API

| Field | Detail |
|---|---|
| **URL** | `https://www.sharpe.ai/products/correlation` |
| **Cost** | FREE, no signup |
| **Data** | NxN Pearson correlation heatmaps (up to 10 assets), rolling 30d/90d/1y/3y |
| **Assets** | BTC, ETH, top 100 crypto + Gold, S&P 500, Nasdaq, DXY |
| **Access** | REST API, MCP server, CLI |

**Integration points**:
- Cross-asset correlation is a **regime signal** — when BTC-SPX correlation spikes, it indicates risk-on/off regime
- BTC-Gold correlation indicates whether crypto is acting as safe-haven or risk asset

**Honest recommendation — compute our own first (moat-first):**
- **Compute the correlations from our own recorded prices** — this is the *most* on-brand idea in the whole survey: it compounds our retained-moat asset and has **zero external dependency**. BTC-ETH-Gold-SPX correlations need only our recorded price feed + a free Gold/SPX/DXY source (FRED, spec 030).
- **Sharpe is a fallback/validation**, not primary: it gives pre-computed rolling correlations for cross-checking our own math. Use it to sanity-check, not as the source of truth.
- Prefer `MacroPoint` from own data first; only pull Sharpe if we find our own correlate diverging.
- New `MacroPoint::Correlation` variant.

---

### 6. Blockchain.com API

| Field | Detail |
|---|---|
| **URL** | `https://www.blockchain.com/api` |
| **Cost** | FREE |
| **Data** | Bitcoin hashrate, mining difficulty, mempool size, block times, transaction fees, network stats |

**Key endpoints**:
- `GET /charts/hash-rate` — Bitcoin network hashrate
- `GET /charts/difficulty` — mining difficulty
- `GET /charts/mempool-size` — mempool transaction count
- `GET /charts/median-transaction-fee` — transaction fees

**Integration points**:
- **Mining difficulty adjustment** is a macro signal — rising hashrate = miner conviction
- **Mempool congestion** = network demand proxy
- Both are free macro signals not currently in the app
- Complements FRED macro data (spec 030) with crypto-native network health metrics
- New `MacroPoint::NetworkHealth` variant

**Concrete implementation**:
- Add to `mp-macro` (~20 lines)
- `GET https://api.blockchain.info/charts/hash-rate?timespan=1days&format=json`
- Store as `MacroPoint` with series "btc_hashrate", "btc_difficulty"

---

## TIER 2: SHOULD ADD — Free, Good Signal, Moderate Effort

### 7. CoinGlass API

| Field | Detail |
|---|---|
| **URL** | `https://docs.coinglass.com/` |
| **Cost** | Free account required; some endpoints free, others paid |
| **Data** | Aggregated OI, funding, liquidation heatmaps, liquidation maps, long/short ratios, **ETF flows**, on-chain reserves, whale metrics, L2/L3 orderbook depth |

**Key unique data**:
- **BTC ETF flow data** (IBIT, FBTC, GBTC inflows/outflows) — a brand new signal category
- Liquidation heatmaps with cascade zone visualization
- ETF net flows as institutional demand gauge

**Caveat**: Some endpoints require paid API key. Free tier covers basic OI/funding/liq. ETF flow data may require paid tier.

**Integration points**:
- ETF flow data → `MacroPoint::ETFFlow` — BTC spot ETF inflows/outflows are a major institutional signal
- Liquidation heatmaps validate `liq_est_bands` model
- Consider after Coinalyze (which covers the derivatives basics for free)

---

### 8. CryptoDataDownload

| Field | Detail |
|---|---|
| **URL** | `https://www.cryptodatadownload.com/` |
| **Cost** | FREE (CSV downloads, REST API) |
| **Data** | Historical OHLCV from 28+ exchanges, correlation heatmap, Deribit options historical data |

**Integration points**:
- **Free historical data bootstrap** for Phase 3 (expand research corpus)
- Need labeled historical data separate from live recordings — this is the cheapest source
- Deribit options historical data for options research
- Free correlation heatmap for validation

**Concrete implementation**:
- One-time bulk download for backtest corpus (not a live collector)
- Use `panel.py` (spec 031) download infrastructure
- Fidelity label: `external_archive:crypto-data-download-ohlcv-v1`

---

### 9. CoinGecko API

| Field | Detail |
|---|---|
| **URL** | `https://www.coingecko.com/en/api` |
| **Cost** | FREE tier (10–30 calls/min) |
| **Data** | Prices, market cap, volume for 14,000+ coins, trending, global DeFi stats, BTC dominance |

**Integration points**:
- **BTC dominance** metric is useful for regime detection — rising dominance = flight to quality
- **Total crypto market cap** provides market-wide context
- **Global trading volume** across all markets (not just your recorded venues)
- Complement to your per-venue price data

**Concrete implementation**:
- Add to `mp-macro` or new `mp-market-context` snapshotter
- `GET https://api.coingecko.com/api/v3/global` → BTC dominance, total market cap
- ~2 API calls per daily snapshot

---

### 10. Dune Analytics

| Field | Detail |
|---|---|
| **URL** | `https://dune.com/` |
| **Cost** | FREE (web UI), API requires paid plan ($349/mo) |
| **Data** | Custom SQL queries against raw blockchain data — any on-chain metric |

**Integration points**:
- Dune is the "FRED of on-chain" — you can query any on-chain metric via SQL
- Web UI is free for research/exploration
- When you find a useful metric, replicate the query logic with DeFiLlama or direct on-chain calls
- Not for automated data collection (API is expensive)

**Concrete implementation**:
- Use Dune web UI for research only
- When a metric proves valuable, implement it via DeFiLlama or direct blockchain API

---

### 11. Bitquery

| Field | Detail |
|---|---|
| **URL** | `https://bitquery.io/` |
| **Cost** | Free tier available (limited queries) |
| **Data** | GraphQL API for 40+ chains — DEX trades, OHLCV, whale movements, money flow |

**Integration points**:
- DEX-specific data (Uniswap, Raydium, PancakeSwap) — not covered by your CEX collectors
- "Money flow" queries track whale movements across chains
- Free tier is limited — useful for specific queries, not bulk data

**Concrete implementation**:
- Consider for DEX volume data if on-chain DEX activity becomes relevant
- Low priority vs CEX-focused sources

---

### 12. Whale Alert API

| Field | Detail |
|---|---|
| **URL** | `https://developer.whale-alert.io/` |
| **Cost** | Free tier (limited history), paid for full |
| **Data** | Large on-chain transactions across 30+ blockchains, exchange inflows/outflows |

**Integration points**:
- Supplements `mp-whale` (spec 028) which only covers Hyperliquid
- BTC/ETH exchange inflows = potential selling pressure signal
- Cross-chain whale tracking (TRON USDT, SOL, etc.)

**Caveat**: Free tier is limited. Your existing Hyperliquid whale census is more granular for that venue. Consider as a supplement for BTC/ETH exchange flow data.

---

## TIER 3: NICE TO HAVE — Specific Research Angles

### 13. altFINS API

| Field | Detail |
|---|---|
| **URL** | `https://altfins.com/crypto-market-and-analytical-data-api/` |
| **Cost** | Free tier available |
| **Data** | 150+ pre-computed technical indicators, 130+ trading signals, screener data |

**Use case**: Validate your own computed features against pre-computed TA signals. AI trade setups for strategy research. Not a primary data source.

---

### 14. NewsData.io

| Field | Detail |
|---|---|
| **URL** | `https://newsdata.io/` |
| **Cost** | Free tier (limited), sentiment analysis requires paid ($200/mo) |
| **Data** | Crypto news from 100K+ sources, sentiment (positive/negative/neutral) |

**Use case**: Feed raw news articles into LLM nightly brief pipeline (spec 030). Your current brief uses LLM to find news — this provides structured input.

---

### 15. CryptoPanic

| Field | Detail |
|---|---|
| **URL** | `https://cryptopanic.com/` |
| **Cost** | Free tier available |
| **Data** | Aggregated crypto news with community sentiment voting |

**Use case**: Simpler alternative to NewsData.io for news aggregation. Free API for headlines.

---

### 16. NewHedge

| Field | Detail |
|---|---|
| **URL** | `https://newhedge.io/` |
| **Cost** | Free account for basic data |
| **Data** | BTC ETF flows, correlation charts, difficulty estimator, spot BTC ETF flows |

**Use case**: Specialized BTC data — ETF flow tracker and correlation charts. Manual research tool, not automated.

---

### 17. Coin Metrics (Community)

| Field | Detail |
|---|---|
| **URL** | `https://docs.coinmetrics.io/` |
| **Cost** | Free community data (limited metrics) |
| **Data** | Network data (hashrate, difficulty, fees), market data |

**Use case**: High-quality institutional data. Free tier supplements Blockchain.com for network metrics.

---

### 18. Blockchair API

| Field | Detail |
|---|---|
| **URL** | `https://blockchair.com/api` |
| **Cost** | Free tier (limited) |
| **Data** | Multi-chain blockchain explorer — Bitcoin, Ethereum, 40+ chains |

**Use case**: Alternative to blockchain.com for cross-validation of on-chain data.

---

## TIER 4: SKIP — Not Worth Adding

| Platform | Cost | Reason to Skip |
|---|---|---|
| **Token Terminal** | $200+/mo API | DeFiLlama gives free version of most metrics |
| **Velo.xyz** | $199/mo API | Free web UI only; same data available from Coinalyze |
| **Derivatives Monkey** | Free (UI only) | No API; your app already computes Greeks/IV from Deribit |
| **Cryexc** | Free (UI only) | No programmatic API; browser WASM app; design reference only |
| **Glassnode** | $79/mo+ | Free tier extremely limited; DeFiLlama covers most metrics free |
| **CryptoQuant** | Paid API | Exchange reserves already covered by spec 034 mp-netflow |
| **Santiment** | $35/mo+ | Social sentiment is noisy; not proven alpha for swing strategies |
| **LunarCrush** | Paid (free = market data only) | Free tier no longer includes social sentiment |
| **Kaiko** | $500+/mo | Institutional pricing; overkill for current use case |
| **Amberdata** | Institutional | Your Deribit collector already covers options data |
| **Tardis.dev** | $56/mo+ | Excellent L2 historical data but your collectors record live data |
| **CoinAPI** | Paid | Your existing collectors cover the same exchanges |

---

## Signal Coverage Matrix (with **kind**: NEW signal vs VALIDATION vs CONTEXT)

| Signal Category | Current Sources | Recommended Additions | Kind | Gap Filled |
|---|---|---|---|---|
| **Cross-exchange OI** | Hyperliquid only | Coinalyze, OpenMarket | VALIDATION | Multi-venue OI context |
| **Cross-exchange funding** | Hyperliquid only | Coinalyze, OpenMarket | VALIDATION | Funding arbitrage *research* |
| **Predicted funding** | None | Coinalyze | CONTEXT (NOT alpha) | Carry monitoring only — priced in by definition |
| **Liquidation validation** | Model only (liq_est_bands) | Coinalyze, OpenMarket, CoinGlass | VALIDATION | RES-4 ground truth |
| **Long/short ratio** | None | Coinalyze | NEW signal | New sentiment feature |
| **DeFi TVL** | None | DeFiLlama | CONTEXT/regime | Risk-on/off macro signal |
| **Stablecoin supply** | None | DeFiLlama | NEW signal | BTC leading indicator (delta) |
| **On-chain volume** | None | DeFiLlama | CONTEXT | DEX activity gauge |
| **Sentiment** | None | Alternative.me F&G | CONTEXT | Market sentiment |
| **Cross-asset correlation** | None | **own computed** (Sharpe = validate) | NEW signal | Regime detection |
| **Network health** | None | Blockchain.com | CONTEXT | Mining hashrate/difficulty (health, not price signal) |
| **Market context** | None | CoinGecko | CONTEXT | BTC dominance, total mcap |
| **Polymarket smart-money** | None | OpenMarket (only place) | NEW signal | Wallet/flow analytics (research phase) |
| **ETF flows** | None | CoinGlass (if free) | Institutional demand |
| **Historical corpus** | Live recordings only | CryptoDataDownload | Phase 3 backtest bootstrap |
| **TPO/Market Profile** | swing.value_area (bar-only) | OpenMarket | Independent validation |
| **News feed** | LLM-generated | NewsData.io, CryptoPanic | Structured news input |

---

## Recommended Build Order (revised — moat-first, validation before new inputs)

**Revised priority note:** The previous draft put analysis *before* assets (OpenMarket TPO in Week 2, own-recorded correlations at #7, CryptoDataDownload corpus at #8). This revision reverses that: **prioritize what compounds our retained-moat data and validates existing models, then add new-signal sources.** Engineering hours are the scarce resource (ROADMAP Phase 0 not yet promoted) — do not stand up 6 collectors at once.

### Week 1 — Highest ROI (regime + validation, fills real gaps)
1. `mp-defillama` — DeFi TVL + stablecoin supply (**top pick**; free, 5 calls/day, regime signal)
2. `mp-coinalyze` — cross-exchange OI/funding/liq/long-short — **validation** of `oi_regime` (045) + `liq_est_bands` (029) + new long/short feature. **NOT** "predicted-funding alpha" (see §1 caveat)
3. Add Fear & Greed Index to `mp-macro` (1-line addition)

### Week 2 — Compound our own data (moat-first)
4. **Compute BTC-ETH-Gold-SPX correlations from own recorded prices** (zero external dep; FRED for Gold/SPX/DXY) — raised from #7→#4
5. Add hashrate/difficulty to `mp-macro` (Blockchain.com API)
6. Add BTC dominance + total market cap to `mp-macro` (CoinGecko API)

### Week 3 — Research Phase (backtest corpus + exploration)
7. **Bulk-download CryptoDataDownload historical corpus for Phase 3** — esp. **Deribit options** + **non-Binance OI/funding** that spec 027 (`historical-bootstrap`, Binance-aggTrades-only) does not cover. Raised from #8→#7
8. Explore Dune Analytics for on-chain metrics worth automating (free web UI only)
9. Evaluate CoinGlass ETF flow data (may require paid tier)

### Deferred / research-only
10. **`mp-openmarket`** — **demoted from Tier 1 to research-only.** Only pursue the **Polymarket smart-money wallet** feed; do NOT build OI/funding/TPO collection off it (our own recorder is the moat; Coinalyze covers the derivative basics free). See §3.
11. News/Dune/Bitquery/NewHedge — **off-theme today → `docs/BACKLOG.md`**, not this build order.

---

## Technical Notes

### Event Type Design

New external data sources should emit events through the existing `EventEnvelope` infrastructure. Options:

1. **Extend `MacroPoint`** — add variants for sentiment, correlation, network health, market context
2. **New `DerivAggSnapshot`** — aggregated derivatives data from Coinalyze/OpenMarket (distinct from per-venue events)
3. **New `DeFiSnapshot`** — DeFi ecosystem data from DeFiLlama

The choice depends on whether the data is macro-context (→ `MacroPoint`) or derivatives-specific (→ dedicated type).

### Rate Limit Budget

| Source | Rate Limit | Calls/Day Needed | Budget Usage |
|---|---|---|---|
| Coinalyze | 40 req/min | ~120 (hourly BTC+ETH) | 5% |
| DeFiLlama | ~50 req/min | ~5 (daily snapshot) | <1% |
| OpenMarket | 10 w/min | ~18 (hourly, 3 endpoints) | 3% |
| Alternative.me | None published | 1 (daily) | negligible |
| Blockchain.com | None published | 2 (daily) | negligible |
| CoinGecko | 10–30 req/min | 2 (daily) | negligible |

Total: well within all limits. No conflicts.

### Data Retention

External data should follow the same append-only, never-overwrite convention (W-6):
- Raw API responses archived under `data/external/{source}/{date}/`
- Derived features materialized through the standard feature store pipeline
- Fidelity labels for external data: `external_api:{source}-v{version}`

### ⚠️ Legal / ToS / Secrets (PD-2) — read before integrating any source

**External data is not owned data; appending it to our corpus does not grant rights.** Add this as an explicit review step before each integration:

- **Licensing / ToS:** each source has different terms on storage, commercial use, redistribution, and citation.
  - Coinalyze **requires citation in public use** — plan attribution if our research is ever shared.
  - CoinGecko / DeFiLlama free tiers carry **commercial/redistribution restrictions** — verify against our use case (personal research vs any future indicator/index product).
  - Label external data `external_api:{source}-v{version}` (above) and **never relabel it as our own recorded corpus** — that distinction protects both our integrity and our legal position.
- **Secrets discipline (PD-2):** every keyed source (Coinalyze, OpenMarket, CoinGlass, Etherscan, FRED) must read keys **from env / secret store only — never committed to repo, config, or `.example` templates** (mirrors spec 030 MAC-2). Where possible, prefer keyless sources (DeFiLlama, Alternative.me, Blockchain.com, FRED with free key).
- **New external hosts** require **owner sign-off** per the CLAUDE.md safety table (the same rule spec 030 uses for FRED). Each integration above needs that sign-off documented before implementation.

---

## References

- Coinalyze API docs: `https://api.coinalyze.net/v1/doc/`
- DeFiLlama API docs: `https://api-docs.defillama.com/`
- OpenMarket API docs: `https://openmarket.xyz/docs`
- Alternative.me API: `https://api.alternative.me/fng/`
- Sharpe Correlation: `https://www.sharpe.ai/products/correlation`
- Blockchain.com API: `https://www.blockchain.com/api`
- CoinGlass API docs: `https://docs.coinglass.com/`
- CryptoDataDownload: `https://www.cryptodatadownload.com/`
- CoinGecko API: `https://www.coingecko.com/en/api`
- Dune Analytics: `https://dune.com/`
- Bitquery: `https://bitquery.io/`
- Whale Alert API: `https://developer.whale-alert.io/`
- altFINS API: `https://altfins.com/crypto-market-and-analytical-data-api/`
- NewsData.io: `https://newsdata.io/`
- CryptoPanic: `https://cryptopanic.com/`
- NewHedge: `https://newhedge.io/`
- Coin Metrics: `https://docs.coinmetrics.io/`
- Blockchair: `https://blockchair.com/api`
