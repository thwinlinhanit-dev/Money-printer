# 040 — IBIT ETF Options Integration

## Purpose

Collect and analyze options data on **IBIT** (iShares Bitcoin Trust, BlackRock's
spot Bitcoin ETF) and bridge it with the crypto-native Deribit options data.
IBIT options trade on CBOE/Nasdaq with traditional market hours; they represent
the institutional/TradFi channel for BTC options exposure. Cross-referencing
IBIT flow with Deribit flow reveals when institutional money is positioning
differently from crypto-native traders — a lead-lag signal.

## Scope

**In:** IBIT options data collection (CBOE public tape), IV surface construction
for IBIT, Greeks aggregation, flow analytics specific to IBIT, cross-market
analysis (IBIT vs Deribit IV divergence, flow lead-lag, risk comparison).

**Out:** Other ETF options (GBTC, FBTC — deferred), IBIT spot trading (not in
scope), options trading strategies on IBIT (BACKLOG [v2]), TradFi market data
beyond IBIT.

## Design

### Data Source

IBIT options trade on CBOE (primary) and are reported via:
- **OPRA** (Options Price Reporting Authority) public tape — real-time last sale
  and quote data. Free with 15-minute delay; real-time requires a data vendor
  subscription.
- **CBOE** free delayed data — accessible via HTTP, no auth required for
  delayed (15-min) data.
- **Yahoo Finance / similar** — free end-of-day options chain snapshots.

**v1 approach:** CBOE public delayed data (HTTP REST, 15-min lag) — zero cost,
no auth, sufficient for structural analytics (IBIT flow is slower-moving than
crypto-native options). The 15-minute delay is acceptable because IBIT's
institutional flow is multi-hour/multi-day, not sub-minute.

**v2 approach:** OPRA real-time feed (requires data vendor subscription) — for
live flow analytics and lead-lag detection.

### Collector Architecture

```
CBOE public delayed data (HTTP REST, 15-min lag)
  │  End-of-day options chain snapshot: strike, bid, ask, last, volume, OI, IV
  │  Daily: one snapshot at market close (16:00 ET)
  ▼
ibit_collector (new binary in collectors/)
  │  Parse options chain → OptionTrade/OptionTicker events (reuse spec 001 schema)
  │  Tag: venue = "cboe", underlying = "IBIT"
  ├──▶ raw/cboe/{date}/ibit_chain.ndjson.zst   (verbatim snapshot, COL-9)
  └──▶ cold/options/…/ibit_part.parquet         (append-only, W-6; manifest sampled=false)
```

The IBIT collector reuses the existing `OptionTrade`/`OptionTicker` event
schema (spec 001) with the following metadata:
- `venue` = "cboe" (new Venue variant, spec 001 amendment)
- `OptionLeg.underlying` = "IBIT"
- `OptionLeg.strike` = CBOE strike price (USD, same as IBIT spot USD price)
- `OptionLeg.expiry_ts_ns` = expiry date (monthly expiries, third Friday)
- `OptionLeg.kind` = Call/Put (CBOE standard)

The contract multiplier for IBIT options is 100 shares (standard US equity
option contract size). The `SymbolMeta.contract_multiplier` MUST be set to 100
for IBIT options.

### IBIT Analytics Pages

The analytics terminal (spec 041) renders a dedicated IBIT section mirroring
the crypto-native pages but adapted for TradFi conventions:

| IBIT Page | Path | Description |
|---|---|---|
| IBIT Dashboard | `/ibit/` | Overview: IV level, vol surface snapshot, top flow, Greeks summary |
| IBIT Vol | `/ibit/vol` | IV surface, term structure, IV percentile |
| IBIT Greeks | `/ibit/greeks` | Net delta/gamma/vega across chain |
| IBIT Flow | `/ibit/flow` | Net premium, block trades, call-put ratio |
| IBIT Risk | `/ibit/risk` | GEX profile, max pain, implied probability |
| IBIT Cross | `/ibit/cross` | IBIT vs Deribit IV divergence, flow lead-lag |
| IBIT Surface | `/ibit/surface` | Full IV surface (3D) |
| IBIT Chart | `/ibit/chart` | IBIT spot + options overlay |

### Cross-Market Analysis (spec 040-CROSS)

The key unique value of the IBIT section is **cross-market analysis** —
comparing IBIT (TradFi) with Deribit (crypto-native) for the same underlying
asset (BTC exposure).

**IV Divergence:**
```
ibit_deriv_iv_divergence.{window} = IV_ATM_ibit - IV_ATM_deribit
```

When IBIT IV > Deribit IV → TradFi is pricing more uncertainty (institutional
hedging demand or regulatory fear). When IBIT IV < Deribit IV → crypto-native
market is pricing more uncertainty (leverage unwinding, exchange risk).

**Flow Lead-Lag:**
```
ibit_flow_lead_lag.{window} = cross-correlation(ibit_net_delta, deribit_net_delta, lags)
```

Does IBIT institutional flow lead or lag Deribit flow? The answer varies by
regime: during institutional accumulation (ETF inflows), IBIT may lead;
during crypto-native deleveraging, Deribit may lead.

**Risk Comparison:**
```
ibit_gex_vs_deribit_gex = GEX_ibit normalized - GEX_deribit normalized
```

Where normalization = GEX / (OI × spot²). Divergence in normalized GEX
indicates different dealer positioning across the two venues.

### IBIT-Specific Features

| Feature ID | Description | Formula |
|---|---|---|
| `ibit.oi.total` | Total IBIT options OI (contracts) | Σ OI across all strikes/expiries |
| `ibit.oi.call_put_ratio` | Call OI / Put OI | Σ OI_call / Σ OI_put |
| `ibit.volume.total` | Daily options volume | Σ volume from snapshot |
| `ibit.iv.atm` | ATM IV (nearest monthly expiry) | Interpolated at K=spot |
| `ibit.net_premium` | Signed premium (daily close) | Σ side × last × volume × 100 |
| `ibit.block_count` | Number of block trades (>$500k) | Count where notional ≥ $500k |
| `ibit.max_pain` | Max pain strike (nearest expiry) | argmin payout (spec 037 pattern) |

### Trading Hours Alignment

IBIT trades on CBOE: 09:30–16:00 ET (Mon–Fri, US market holidays excluded).
Crypto-native options trade 24/7.

Cross-market features (lead-lag, divergence) MUST handle the time mismatch:
- IBIT data is available only during US market hours + after-hours snapshots
- Deribit data is continuous
- Lead-lag computation MUST align on overlapping hours or use daily aggregates
- Feature emission timestamps MUST reflect when the data was actually
  available (not synthetic fill-forward)

## Requirements

- **IBI-1** A new `ibit_collector` binary MUST connect to CBOE public delayed
  data (HTTP REST), parse the options chain snapshot, and emit
  `OptionTrade`/`OptionTicker` events per spec 001 with venue="cboe" and
  underlying="IBIT". The collector MUST follow the collector architecture
  (spec 002): reconnection, status events (COL-3), staleness watchdog
  (COL-2), raw frame capture (COL-9).

- **IBI-2** The IBIT collector MUST set `contract_multiplier = 100` in
  SymbolMeta for all IBIT options (US equity standard). All aggregate
  computations (specs 037/038/039) MUST use this multiplier for IBIT.

- **IBI-3** IBIT options MUST be written to the same `cold/options/` Parquet
  layout as Deribit options (spec 031), with `venue=cboe` in the partition.
  The existing Parquet schema (flat-with-metadata-columns) MUST accommodate
  IBIT rows without schema change.

- **IBI-4** The Greeks computation (spec 037) and IV surface builder (spec 038)
  MUST work on IBIT data when underlying="IBIT". The feature ids embed the
  underlying (`gex.profile.ibit`, `iv.term.ibit.1m`), so the same code paths
  handle both Deribit and IBIT via the underlying filter.

- **IBI-5** Cross-market features (IV divergence, flow lead-lag) MUST be
  registered as GLOBAL tick features (FEA-20) because they require events
  from both venues (Deribit + CBOE). They MUST suppress emission when only
  one venue's data is available (same pattern as OFI-12).

- **IBI-6** Trading-hours alignment: cross-market features MUST use daily
  aggregates (closing values) for v1, not intraday, to avoid the
  24/7 vs 9:30-16:00 mismatch. The daily aggregate MUST use the CBOE
  closing snapshot for IBIT and the Deribit end-of-day state for BTC.

- **IBI-7** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16),
  entries in `ibit_collector.example.toml`; data source URL, poll interval,
  market hours, block threshold explicit.

- **IBI-8** Tests MUST use recorded CBOE fixtures in `testdata/`, no network
  (CONV-23); requirement-ID test names (CONV-21).

- **IBI-9** The IBIT collector MUST NOT block or delay the Deribit collector
  (spec 031). They run as separate processes (COL-1 architecture); the IBIT
  collector's slower poll cycle (daily) does not affect the Deribit
  collector's real-time WebSocket stream.

- **IBI-10** Cross-market lead-lag analysis MUST be materialized as Parquet
  for offline research (spec 010). The materialized table carries:
  `{ date, ibit_iv_atm, deribit_iv_atm, ibit_net_delta, deribit_net_delta,
     iv_divergence, flow_corr_lag0, flow_corr_lag1, flow_corr_lag2 }`.

## Acceptance criteria

- [ ] `ibi_1_collector_connects_and_emits_events` — fixture replay produces
  OptionTrade/OptionTicker events with venue="cboe", underlying="IBIT".
- [ ] `ibi_2_multiplier_100` — IBIT notional = price × qty × 100, not × 1.
- [ ] `ibi_3_parquet_writes_to_cold_options` — IBIT rows appear in
  cold/options/ with venue=cboe partition.
- [ ] `ibi_4_greeks_work_on_ibit` — GEX profile for IBIT uses the correct
  multiplier and spot reference.
- [ ] `ibi_5_cross_market_suppressed_single_venue` — no Deribit data →
  cross-market features emit None.
- [ ] `ibi_6_daily_alignment` — cross-market features use closing snapshots
  from both venues.
- [ ] `ibi_7_check_config_rejects_unknown_fields`.
- [ ] `ibi_8_fixtures_no_network`.
- [ ] `ibi_9_ibit_collector_does_not_block_deribit` — concurrent runs
  produce independent output.

## Decisions

- 2026-08-22: New spec. IBIT is the bridge between crypto-native options
  (Deribit, 24/7) and institutional options (CBOE, market hours). The
  IBIT section of derivativesmonkey.com is unique in the market — no other
  platform cross-references crypto-native and TradFi options on the same
  underlying. This is a genuine competitive advantage.

- 2026-08-22: v1 uses CBOE public delayed data (15-min lag, free) rather
  than OPRA real-time (expensive). IBIT options are less liquid than Deribit
  BTC options; the 15-min delay is acceptable for structural analytics.
  The collector is designed to accept a real-time feed drop-in (v2) without
  architecture changes.

- 2026-08-22: Contract multiplier = 100 (US equity standard) makes IBIT
  options very different from Deribit options (1 BTC/ETH per contract).
  A 10-contract IBIT block represents 1,000 shares of IBIT ≈ $50k notional
  (at $50/share), while a 10-contract Deribit block represents 10 BTC ≈
  $1M. USD normalization in flow features (spec 039) is essential.

- 2026-08-22: Cross-market features use daily aggregates (not intraday)
  because IBIT's market hours and Deribit's 24/7 cycle do not overlap
  cleanly. Daily aggregates are the minimum viable comparison. v2 can
  use overlapping-hours alignment (09:30–16:00 ET mapped to Deribit's
  corresponding UTC window).

- 2026-08-22: The IBIT collector reuses the existing event schema
  (OptionTrade/OptionTicker) with a new venue variant ("cboe"). This
  requires a spec 001 amendment (CONV-20) + owner sign-off (CLAUDE.md).
  The amendment is minimal: one new Venue variant, no structural change.

- 2026-08-22 (review): IBIT options are AMERICAN-style (US equity-listed
  convention), unlike Deribit's European options. Every downstream model in
  specs 037–039 that assumes European exercise (Black-Scholes delta fallback,
  delta-as-probability implied distribution, European GEX conventions) carries
  a small early-exercise premium error when applied to IBIT. Accepted for v1 —
  the error is small for an ETF without meaningful dividends and far below the
  analytics' signal value — but it is documented here and MUST be noted in
  feature metadata for any `*.ibit` feature. A binomial/CRR pricer is the v2
  fix if IBIT analytics graduate from research to strategy inputs.

- 2026-08-22 (review): The CBOE free delayed endpoints are unofficial,
  undocumented JSON whose shape changes without notice. The staleness watchdog
  (COL-2) covers downtime but NOT silent schema drift (a 200 response with a
  renamed field parses as zero rows = silent data loss). IBI-1 therefore
  additionally requires a parse-canary: a snapshot that yields zero parsed
  contracts (or >X% field-level parse failures) MUST emit a Status event
  (COL-3, severity per spec 009) — never a clean empty recording.

## Open questions

- Should we also collect IBIT **spot** volume and price? IBIT spot trades
  on Nasdaq; the volume is useful context for the options flow (high spot
  volume + high options volume = conviction). Deferred — IBIT spot is
  available from Yahoo Finance / free APIs; can be added to the collector.
- OPRA real-time data: what is the actual cost for a data vendor? If < $50/mo,
  the v2 upgrade may be worth it for live lead-lag analysis. Research needed.
- Other Bitcoin ETF options (FBTC, GBTC, ARKB): should we plan for
  multi-ETF collection now or add them one at a time? The architecture
  supports multiple underlyings (feature ids are underlying-scoped), so
  adding more ETFs is additive. Plan for it; implement one at a time.
