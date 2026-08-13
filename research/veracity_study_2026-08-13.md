# Veracity study — spec 026 value-level check on the real corpus (2026-08-13)

**Question:** how many `price_divergence` / `trade_drought` windows (CVG-13..15)
on the real recorded corpus are real corruption versus venue reporting noise —
can the veracity pass be trusted in read paths?

**Method:** (1) run the shipped detector (`mp-cross-venue`, reads cold Parquet
only, CVG-1) over every day with cold data; (2) for the multi-venue overlap
days that the INT-4 gate refuses to compact, a temporary read-only probe
(deleted after the study) read the raw logs directly and computed the same
per-window metrics the detector would; (3) grade each candidate window against
the raw trade series. No network, no cold writes (the detector's own
`findings.json` output excepted, CVG-8).

## The detector's output on the corpus: 0 + 0

`mp-cross-venue` over the only cold days (binance_futures 07-20/07-22/07-23,
single venue each):

```
schema_ver=2 date=2026-07-20 findings=0 veracity=0
schema_ver=2 date=2026-07-22 findings=0 veracity=0
schema_ver=2 date=2026-07-23 findings=0 veracity=0
```

Zero gap findings and zero veracity findings — not because the corpus is
clean, but because **no cold day has a cohort** (`min_cohort=2` unmet: one
venue per day). The veracity section is present and empty. The detector ran
correctly; it had nothing to compare.

## Why no multi-venue cold data exists: the quarantine survey

Every overlap day is refused by the INT-4 gate before cold writes. Probing the
raw logs shows why, and grades each:

| Day | Recorded | Verdict | Evidence |
|---|---|---|---|
| 07-19 | binance 241 MB, bybit 0.1 MB, hyperliquid 4.6 MB | **corruption, caught** | binance log contaminated: 39,941 hyperliquid trades + 900 hyperliquid book/funding/mark/OI events leaked in (911k events total); bybit log essentially empty — 482 events, all leaked hyperliquid trades. INT-4 quarantines both. |
| 07-21 | binance 386 MB, hyperliquid 0.2 MB | **corruption, caught** | binance log contaminated: 123,750 hyperliquid trades + 32.5k each funding/mark/OI + 6.2k book snapshots leaked in. The "hyperliquid" log is a ~4-minute sliver (2,418 events) — the real hyperliquid day lives inside the binance file. Quarantined. |
| 08-08 | binance 528 MB, hyperliquid 61 MB | **prices trustworthy; format pre-provenance** | No cross-leak (each file is pure). Median prices agree to 0.02% across 15 overlapping hours (below). But both logs fail provenance/coverage checks (pre-provenance collector, stale_stream, coverage 0.9886) → quarantined. |
| 08-12 (local) | hyperliquid only | presence-level degradation | coverage 0.9568, 3 coverage gaps, stale bursts — the home-network signature; no second venue to value-check it. |

The contamination signature is the old collector writing every venue's stream
into one file (07-19/07-21), plus provenance-less events (all pre-08-09
recordings). The `venue_mismatch`/`missing_provenance` audit findings are real
corruption and correctly block ingress. The one day whose *value* is
trustworthy (08-08) still can't enter cold because its *format* predates
provenance.

## The value-level grading (raw pass, read-only)

**08-08 BTC — 15 comparable hours, the only real cross-venue window set:**

Median trade prices, binance_futures vs hyperliquid (both ~64.9k):

```
hour 00:  64861.90  vs  64869.00   (0.011%)
hour 04:  64987.10  vs  64974.00   (0.020%)
hour 08:  64945.20  vs  64955.00   (0.015%)
hour 12:  64973.70  vs  64959.00   (0.023%)
hour 14:  65031.90  vs  65078.00   (0.071%)
```

Worst pairwise gap over all 15 windows: **0.07%** (hour 14). The 5% default
band has ~70-250x headroom. True VWP (Σp·q/Σq) agrees within 0.05%.

**→ 0 real `price_divergence` windows. Zero. The feeds track.**

Trade counts, same day (per hour): binance 5.6k–11.8k, hyperliquid 2.9k–8.5k —
a structural 1.3–3.6x ratio. Under the default `veracity_trade_ratio = 0.5`,
hyperliquid would be flagged `trade_drought` in ~10 of 15 windows. **All of
those are noise**: the ratio is print granularity (binance splits prints more
finely), not dropped frames — hyperliquid's own count series is stable (no
collapse within itself), and the prices prove the feed was live.

07-19/07-21 offer **no valid comparison at all**: the "second venue" is a copy
of the same leaked feed, so a value check would either trivially "agree"
(false clean) or produce nonsense. 07-19's binance VWP showed an apparent ~5x
divergence in an early aggregation pass — that was an artifact of a
count-weighted mean (Σp·q/n) in the probe, not the data; the corrected
notional/qty VWP is unit-safe. The detector's own implementation uses
notional/qty (correct).

## Verdict

| Question | Answer |
|---|---|
| `price_divergence` windows found on the corpus | **0** (0/15 comparable hours) |
| `trade_drought` windows that would have fired | ~10 on 08-08, **all noise** (print granularity) |
| Real corruption found | 07-19 + 07-21 cross-stream leaks, provenance-less format — **all caught at the INT-4 ingress gate, not by veracity** |
| Real presence degradation found | 08-12 local coverage 0.9568 / stale bursts — veracity can't see it (no cohort) |

The value-level check found **zero windows to flag**, and the corruption that
exists on this corpus is exactly the class it is designed to catch — but
ingress got there first. The honest conclusion: the pre-08-08 multi-venue
corpus never existed in trustworthy form, and the one trustworthy overlap day
(08-08) proves the feeds agree to ~0.02%.

## Recommendations

1. **Trust `price_divergence` in read paths.** On the one clean overlap day it
   fired 0/15 with 70-250x headroom against the 5% band, and the feeds track
   to 0.02%. A real 5%+ feed corruption (wrong feed, hallucinated book) would
   be caught with certainty. It can gate (or at least quarantine-flag) once
   any cohort exists — it is the cheapest silent-corruption tripwire the
   system has.
2. **Do NOT trust `trade_drought` cross-venue as shipped.** The cohort-median
   count ratio false-positives on every venue pair with different print
   granularity (1.3–3.6x here). Fix options: per-venue expected-count bands
   from the venue's own history, or a collapse vs the venue's own running
   baseline (the stale_burst logic) — never a cross-venue median.
3. **Write off 07-19 and 07-21 for cross-venue research.** The leaked streams
   share symbol ids and cannot be separated post-hoc; migration (W-6) would
   re-tag, not de-contaminate. Document them as quarantined history.
4. **VWP formula discipline.** VWP = Σp·q / Σq is unit-safe; a count-weighted
   mean is not (it fabricated a 5x "divergence" in this study). Any consumer
   re-deriving VWP from raw logs must use the detector's formula.
5. **The single-venue gap remains.** Phase-0 records hyperliquid only; the
   veracity pass needs a second venue in the daily set to ever fire on live
   data. A spot leg (Coinbase/Kraken — already in the collector set, spec 026
   original intent) would make the value check live without new machinery.

## Files

- Detector output: `data/cold/cross_venue/date=2026-07-20|22|23/findings.json`
  (schema v2, `veracity: []`).
- Probe: temporary `storage/src/bin/venue_probe.rs` — deleted after the study
  (read-only; per-log symbol tables + per-hour venue trade/VWP/median matrix).
