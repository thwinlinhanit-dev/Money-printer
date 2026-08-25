# 042 — Wallet Cohort Grading

## Purpose

Classify Hyperliquid wallet addresses into behavioral cohorts (Smart Money,
Whale, Retail, Dormant) from recorded position and trade history — turning
opaque 0x addresses (spec 033) into graded, strategy-consumable features.
This is the on-chain analytics layer that iCrypto.ai built with entity
labeling; Freebuff builds it from its own recorded data through the RES-4
event-study gate, never importing external labels (WHL-3).

The graded cohorts feed features like `cohort.net_delta.{symbol}.{cohort}`
(smart money net positioning) and `cohort.whale_ratio.{symbol}` (whale share
of open interest) — the signals that make whale tracking actionable for
strategies.

## Scope

**In:** Offline wallet-scoring pipeline, cohort classification algorithm,
cohort-level aggregate features in the 004 catalog, per-cohort PnL/ROI
statistics for the analytics terminal (spec 041).

**Out:** Real-time cohort assignment (offline batch only in v1), external
label import (WHL-3 rejection carries over), copy-trading (BACKLOG rejected),
wallet identity resolution across chains, social identity linking.

## Design

### Architecture

```
Recorded data (specs 028 + 033)
  │  WhalePosition { address, symbol, size, entry, leverage, liq_price }
  │  TradeWithAddr { taker_addr, price, qty, side }
  ▼
Wallet Scorer (offline, weekly batch)
  │  Per address: aggregate PnL, trade frequency, hold time, leverage profile
  │  Score = f(realized_pnl, trade_count, avg_hold_bars, max_leverage, win_rate)
  ▼
Cohort Classifier
  │  Smart Money: top PnL quartile + trade_count ≥ min_trades + win_rate ≥ 0.45
  │  Whale:       |position_notional| ≥ whale_threshold_usd
  │  Retail:      everyone else (non-dormant)
  │  Dormant:     no activity in trailing N days
  ▼
Cohort Features (spec 004 catalog)
  │  cohort.net_delta.{symbol}.{cohort}    — Σ delta·OI·mult per cohort
  │  cohort.whale_ratio.{symbol}           — whale OI / total OI
  │  cohort.smart_flow.{symbol}.{w}        — smart money net flow window w
  │  cohort.concentration.{symbol}         — HHI of OI across cohorts
  ▼
Strategy consumption (after RES-4 grading, PD-4)
```

### Scoring Algorithm

Each address is scored from its recorded history (spec 028 positions log +
spec 033 trades log). The scorer is deterministic (CONV-9..12): pure function
of (events, config), BTreeMap iteration (CONV-10), seeded RNG where needed
(CONV-11).

**Per-address metrics:**

| Metric | Definition | Source |
|---|---|---|
| `realized_pnl` | Σ (exit_price − entry_price) × size × side for closed positions | WhalePosition snapshots |
| `trade_count` | Number of distinct position open/close cycles | WhalePosition delta detection |
| `avg_hold_bars` | Mean holding period (4h bars) from open to close | WhalePosition timestamps |
| `max_leverage` | Peak leverage observed across all positions | WhalePosition.leverage |
| `win_rate` | Fraction of closed cycles with positive PnL | Derived |
| `avg_position_size` | Mean |size × entry| (USD notional) | WhalePosition |
| `sharpe_approx` | realized_pnl / stddev(per_cycle_pnl), ≥3 cycles required | Derived |
| `last_activity_ts_ns` | Most recent position change timestamp | WhalePosition recv_ts_ns |

**Cohort classification (BTreeMap order, CONV-10):**

```
IF last_activity_ts_ns < (now − dormant_threshold_ns):
  cohort = Dormant

ELSE IF avg_position_size ≥ whale_threshold_usd:
  cohort = Whale

ELSE IF realized_pnl > 0
     AND trade_count ≥ min_smart_trades
     AND win_rate ≥ 0.45
     AND sharpe_approx ≥ min_smart_sharpe:
  cohort = Smart Money

ELSE:
  cohort = Retail
```

Priority: Dormant > Whale > Smart Money > Retail (an address is classified
into the FIRST matching tier). A dormant address that was previously Smart
Money stays Dormant until it reactivates — dormancy is a temporal state, not
a permanent label.

### Cohort Aggregate Features

Registered as global tick features (FEA-20) in the 004 catalog. Each emits
on every OptionTicker/WhalePosition batch using the current cohort membership:

| Feature ID | Emission | Semantics |
|---|---|---|
| `cohort.net_delta.{symbol}.{cohort}` | Σ delta × OI × mult | Net directional exposure per cohort |
| `cohort.whale_ratio.{symbol}` | whale_oi / total_oi | Fraction of OI held by whales (0..1) |
| `cohort.smart_flow.{symbol}.{w}` | Σ signed notional | Smart money net flow over window w |
| `cohort.concentration.{symbol}` | HHI = Σ (share_i)² | Herfindahl index of OI across cohorts (0.25=equal, 1.0=monopoly) |

### Refresh Cadence

- **Cohort membership:** re-scored weekly (offline batch, Sunday 06:00 UTC).
  Addresses are reclassified each week based on the trailing 90-day history.
  Membership changes are journaled to `journal/cohort_changes.jsonl` (W-6).
- **Cohort features:** emitted live from the feature engine using the
  LATEST weekly cohort snapshot. When the snapshot is stale (>7 days), the
  feature emits `None` (suppress, never use a stale cohort — fail-closed).

## Requirements

- **WCG-1** The wallet scorer MUST be a pure function of recorded events
  (WhalePosition + TradeWithAddr logs) and config parameters. MUST NOT read
  external data, wall clock, or network (PD-3/CONV-9).

- **WCG-2** Cohort classification MUST use BTreeMap iteration order for
  address processing (CONV-10). Same input data + config MUST produce
  identical cohort assignments (CONV-12 golden test).

- **WCG-3** The scorer MUST handle degenerate inputs fail-closed (CONV-8):
  addresses with zero trades → Retail; non-finite PnL → skip the address;
  fewer than `min_smart_trades` trades → cannot qualify as Smart Money.

- **WCG-4** Dormancy threshold MUST be configurable (default 30 days). A
  dormant address re-evaluated on reactivation MUST be classified by its
  historical metrics (not reset to Retail).

- **WCG-5** The `whale_threshold_usd` MUST be configurable (default $100k
  notional). A single large position qualifies as Whale regardless of PnL.

- **WCG-6** Smart Money classification MUST require ALL of: positive
  realized PnL, ≥ `min_smart_trades` (default 5) closed cycles, win rate
  ≥ 0.45, and approximate Sharpe ≥ `min_smart_sharpe` (default 0.5). No
  single metric is sufficient.

- **WCG-7** Cohort features MUST register in the 004 catalog with prefix
  `cohort.` (FEA-7/CONV-20). MUST be global tick features (FEA-20) since
  cohort membership spans all symbols per address.

- **WCG-8** Cohort features MUST suppress (emit None) when the weekly
  cohort snapshot is stale (>7 days since last scoring run). MUST NOT
  use a fallback heuristic (fail-closed on stale data).

- **WCG-9** The weekly scoring run MUST journal its output to
  `data/cohorts/{date}.json` (atomic replace, W-6) and a diff to
  `journal/cohort_changes.jsonl` (append-only). The journal MUST include
  per-address: old_cohort, new_cohort, score_breakdown, ts_ns.

- **WCG-10** The scoring pipeline MUST be idempotent: re-running on the
  same data produces the same cohorts (CONV-12) and the same journal
  entry hash.

- **WCG-11** Tests MUST use recorded fixture data in `testdata/`, no network
  (CONV-23); requirement-ID test names (CONV-21); proptest for cohort
  classification boundary conditions (CONV-22).

- **WCG-12** Wallet cohort features MUST enter the signal catalog (spec 025)
  at the `Hypothesis` stage. Promotion to `Tested` requires an event study
  (RES-4) demonstrating that smart money net flow predicts forward returns
  with positive expectancy and n ≥ 30.

## Acceptance criteria

- [ ] `wcg_1_scorer_is_pure_function_of_events_and_config` — golden test:
  same recorded logs + config → byte-identical cohort assignments.
- [ ] `wcg_2_classification_respects_btreetmap_order` — two addresses with
  identical scores get deterministic order.
- [ ] `wcg_3_degenerate_addresses_classified_as_retail` — zero trades,
  non-finite PnL, single cycle with loss.
- [ ] `wcg_4_dormant_threshold_suppresses_inactive_addresses` — address with
  no activity in 31 days (default threshold) → Dormant.
- [ ] `wcg_5_whale_threshold_classifies_large_positions` — $150k notional
  position → Whale regardless of PnL.
- [ ] `wcg_6_smart_money_requires_all_conditions` — high PnL but low win
  rate → not Smart Money; high win rate but few trades → not Smart Money.
- [ ] `wcg_7_catalog_registration_and_feature_ids` — all four feature IDs
  registered; Emission count matches cohort count × symbol count.
- [ ] `wcg_8_stale_snapshot_suppresses_features` — feature emits None when
  snapshot age > 7 days.
- [ ] `wcg_9_cohort_changes_journaled` — re-score after adding new trades
  produces correct diff journal entries.
- [ ] `wcg_10_idempotent_scoring` — run twice on same data, same output
  hash.
- [ ] `wcg_11_proptest_cohort_boundary_classification` — proptest: varying
  PnL/trades/win_rate around thresholds always produces valid cohort.

## Decisions

- 2026-08-23: New spec (derived from iCrypto.ai smart money tracking +
  Freebuff's existing spec 028/033 data layer). The core insight: Freebuff
  already records the raw data (WhalePosition census + TradeWithAddr);
  wallet cohort grading is the missing layer that turns opaque addresses
  into actionable features.

- 2026-08-23: Offline batch only in v1 (weekly). Real-time cohort
  reclassification would require streaming position updates through the
  scorer — deferred to v2 when the scoring pipeline is validated. The weekly
  cadence is sufficient for swing strategies (daily/4h rebalance, spec 035).

- 2026-08-23: Dormant is a temporal state, not a permanent label. An address
  that was Smart Money and goes dormant is classified Dormant until it
  reactivates, then re-evaluated. This prevents stale "Smart Money" labels
  from polluting features.

- 2026-08-23: Sharpe approximation requires ≥3 closed cycles to avoid
  division by near-zero variance. Addresses with 1-2 cycles cannot qualify
  as Smart Money — they lack statistical evidence of skill.

- 2026-08-23: Cohort features are GLOBAL (FEA-20) because cohort membership
  is cross-symbol: one address trades multiple perpetuals, and its cohort
  classification applies to all of them. Per-symbol features would duplicate
  the same address in multiple symbol contexts.

- 2026-08-23: The HHI concentration index (WCG-7) ranges from 0.25 (four
  equal cohorts) to 1.0 (one cohort holds everything). High concentration
  means one cohort dominates the book — a regime signal for position sizing
  (RG-13 correlation adjustment).

## Open questions

- Should the scoring window be trailing 90 days or all-time? Trailing 90d
  adapts to regime changes but loses long-term track records. Default 90d;
  revisit when enough history exists.

- Minimum viable cohort sizes: if Smart Money has <5 addresses in a symbol,
  is the cohort signal statistically meaningful? Consider a
  `min_cohort_size` suppression threshold for features.
