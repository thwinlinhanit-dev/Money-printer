# Swing Data Plan — Higher-Timeframe Collector Set

**Status:** Implemented + deployed on the VPS (Option 1, owner decision
2026-08-22).
**Owner goal:** Focus collection solely on higher-timeframe (daily / 4h / weekly)
trading. Keep only the data that benefits HTF strategies; drop the L2
order-book + microstructure streams that swing strategies must never depend on.

Relates to: `specs/035-swing-focus.md` (SWG-1 data contract),
`specs/036-volume-profile-liquidity.md` (SLQ-D bar-only contract),
`docs/swing-liquidity-features-spec.md` (owner draft).

## Rollout topology (Option 1 — co-existence)

The owner chose to keep the two Phase-0 gate collectors compounding alongside
the swing set rather than replacing them. The VPS is the single recording host
(§6 handoff, 2026-08-18); the live unit layout there since 2026-08-22:

| Unit | Mode | Purpose |
|---|---|---|
| `mp-hyperliquid@BTC` / `@ETH` | **full** (book included) | Phase-0 promotion-gate recordings — UNTOUCHED |
| `mp-swing@bybit-btcusdt` / `@bybit-ethusdt` / `@bybit-solusdt` | `swing_only` | OHLCV tape + funding/mark/OI + live liquidations (COL-29) + cross-asset breadth |
| `mp-whale` | census | whale positioning / liq-est bands (spec 028) |
| `mp-swing-macro` | FRED daily rates | macro proxy — enabled 2026-08-22 after `FRED_API_KEY` provisioning (spec 030 MAC-2); writes `{date}_fred_macro.log` with a 90-day backfill |

Key properties:

- **Single writer per symbol** — the retired full-mode `mp-collector@*` bybit
  units are stopped and disabled, so no `{date}_bybit_*.log` has two hosts
  writing it and the nightly Windows drain never sees same-name/different-bytes
  collisions (the Aug 12–17 hyperliquid backlog lesson).
- **Disk relief** — dropping `orderbook.50` removes ~2.5 GB/day of bybit L2
  depth on a disk that was at 87%.
- The deploy is idempotent: `ops/scripts/deploy_swing.sh` (staged via
  `/home/mp-egress/mp-build`, self-elevating COL-29 pattern). Deploy at a UTC
  day boundary when practical; the swap mid-day only changes which STREAMS a
  bybit day-file carries (event schema is unchanged), and no required stream
  for any gate or research consumer is dropped (`--require-stream
  bybit:liquidation` still satisfied).
- `swing_collectors.ps1` (Windows launcher + Scheduled Task registration) is
  kept as an ISOLATED-HOST fallback for a machine that runs the swing set
  without the Phase-0 recorder. It is deliberately NOT registered on this
  workstation — registering it would make Windows a second writer for the
  bybit symbols and poison the drain with collisions.

## Why this set

Spec 035 / 036 are explicit: all swing features are computed from **closed bars
only** — no L2 order book, no tick-tape input. The full recorder's dominant
disk consumer is the L2 book stream (`l2Book` on Hyperliquid, `orderbook.50`
on Bybit, `depth@100ms` on Binance), followed by the whale/tick census. For a
swing-only focus that book stream is pure overhead: swing never consumes it.

The `swing_only` collector mode (added in `collectors/src/bin/mp-collector.rs`,
2026-08-21) drops exactly that stream while keeping everything swing
consumes. This plan wires that mode into a curated universe plus the two
low-cadence context collectors.

## Tier 1 — Core swing market data (spec 035 SWG-1)

| Stream | Granularity | Why for HTF | Venue |
|---|---|---|---|
| Trades (`trade`) | tick → daily/4h | OHLCV bars, volume, VWAP, volume-profile/POC, realized vol, ATR | all |
| Funding rate | per 8h | funding regime, carry drag, funding extremes | HL `activeAssetCtx` / Bybit `tickers` |
| Mark price | per bar | mark-vs-index basis, liq-distance | HL `activeAssetCtx` / Bybit `tickers` |
| Open interest | daily / 4h | OI regime, OI/volume ratio, OI-price divergence | HL `activeAssetCtx` / Bybit `tickers` |
| Liquidations | daily aggregate | liq-cluster context, `mp-query liq` (spec 029) | Bybit `allLiquidation` (credential-free live source) |

Bars are derived offline from the trade tape (`mp-query bars --interval-secs
14400|86400`, `features/src/bar.rs` `BarBuilder`), so **no kline REST collector
is needed** — the trade stream is the source of daily/4h OHLCV.

## Tier 2 — Low-cadence context (swing edge builders)

| Data | Granularity | Why | Collector / spec |
|---|---|---|---|
| Cross-asset breadth (SOL) | daily | beta / correlation / breadth (trend-breadth idea) | `swing/bybit-solusdt.toml` |
| FRED macro (DXY/10Y/2Y/SOFR/FF) | daily | macro proxy (spec 035 §4) | `mp-macro`, `swing/macro.toml` (spec 030) |
| Whale position census (BTC/ETH/SOL) | ~60s snapshots | whale positioning / liq-est bands (spec 028/029) | `mp-whale`, `swing/whale_positions.toml` |

## Explicitly dropped (not collected in this set)

- L2 order-book depth (`l2Book` / `orderbook.50` / `depth@100ms`) — swing Non-Goal.
- Tick footprint / microstructure — frozen (SWG-8).
- `tape.tps`, `cvd`, `imbalance` tick features — short-horizon only.
- Sub-minute bar types — swing Non-Goal.
- Deribit options (spec 031) — only if options-regime research is pursued.

## Swing universe & launch

VPS (canonical, deployed 2026-08-22): managed by systemd —
`mp-swing@bybit-btcusdt mp-swing@bybit-ethusdt mp-swing@bybit-solusdt`
(+ `mp-swing-macro` when the FRED key lands), alongside the untouched
Phase-0 units. Redeploy/repair with `ops/scripts/deploy_swing.sh`.

Isolated-host fallback (Windows, NOT registered here): supervised by
`swing_collectors.ps1` (Scheduled Task
`MoneyPrinterSwingCollectorsWatchdog`). In that mode it supervises the full
standalone set INCLUDING hyperliquid swing_only configs, because there is no
Phase-0 recorder to piggyback on:

```
collectors/swing/hyperliquid-btc.toml   (HL  BTC, trades + activeAssetCtx)
collectors/swing/hyperliquid-eth.toml   (HL  ETH, trades + activeAssetCtx)
collectors/swing/bybit-btcusdt.toml     (Bybit BTCUSDT, + liq)
collectors/swing/bybit-ethusdt.toml     (Bybit ETHUSDT, + liq)
collectors/swing/bybit-solusdt.toml     (Bybit SOLUSDT, + liq, breadth)
collectors/swing/whale_positions.toml   (mp-whale, BTC/ETH/SOL)
collectors/swing/macro.toml             (mp-macro, FRED; needs FRED_API_KEY)
```

Usage:
```
.\swing_collectors.ps1                 # build + supervise (foreground)
.\swing_collectors.ps1 -RegisterTask   # auto-start at logon/startup
```

## Gate caveat (important)

A `swing_only` recording intentionally omits the `book` stream, so **this set
does NOT satisfy the Phase-0 promotion gate** (`ops/core_symbols.txt` requires
`book`). This corpus is for swing research and backtests only. Run the full
recorder if you still need the Phase-0 gate; run `swing_collectors.ps1` when
you only care about HTF trading.

## Validation

Each config parses via the collector's `--check-config` (all 7 green), and the
`swing_only` behavior is unit-tested in `collectors/src/bin/mp-collector.rs`
(`swing_only_bybit_*`, `swing_only_hyperliquid_*`,
`swing_only_binance_*`): the L2 book/depth stream is dropped while trades,
funding/mark/OI, and liquidations are kept.
