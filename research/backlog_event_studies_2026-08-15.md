# Backlog idea event studies (RES-4, 2026-08-15)

Graded three backlog alpha ideas over the recorded corpus BEFORE any
hypothesis was written — the funnel's cheapest gate (event-study first,
brainstorm 2026-08-15). Runner: `research/run_backlog_event_studies.py`
(record `backlog-event-studies-2026-08-15` in `runs/index.jsonl`).

Corpus: hyperliquid BTC + ETH raw logs 08-08..08-15 (154 BTC hours, 140 ETH
hours, 140 overlapping), hourly mark/OI/funding via `mp-query carry`
(compute-on-read; no compaction — quarantine-blocked on the known
stale-burst days anyway). Returns are mark-to-mark hourly; excess is
cross-asset (BTC−ETH / ETH−BTC) so a market-wide move is not an edge.

## oi-purge-continuation — NOT GRADABLE (n=1), honest

Event = hour where OI fell ≥ threshold vs the prior hour AND mark fell
(quadrant-4: longs flushed), then CAR of forward BTC−ETH excess to +24h.

| Threshold | BTC events | ETH events | BTC CAR[+24h] | ETH CAR[+24h] |
|---|---|---|---|---|
| OI ↓ ≥ 1% + price ↓ | 1 (of 3 raw) | 1 (of 5 raw) | −0.00302 | +0.00044 |
| OI ↓ ≥ 2% + price ↓ | 0 | 1 (of 2 raw) | — | +0.00044 |
| OI ↓ ≥ 3% + price ↓ | 0 | 0 (of 1 raw) | — | — |

Verdict: the corpus has too few quadrant-4 OI purges to grade this idea —
1–5 raw events over 8 days at hourly granularity, and full-window CAR drops
most of them (SIM-6: no partial windows). A single-event CAR is
uninformative regardless of sign. NOT a pass, NOT a kill — insufficient
events; revisit when the corpus spans real purge sessions (a ≥2% hourly OI
drop on these symbols is rare, which is itself a mild caution: the edge, if
real, fires rarely). No hypothesis written.

## listing-flow-v1 — NOT TESTABLE (n=0 by construction)

No collector subscribes a listing feed (spec 002/031), so the corpus has no
Listing events by construction. Grading it now would grade nothing. The
idea stays in the backlog; it becomes testable only after a listing
screener/collector exists.

## weekend-liquidity-v1 — WEAK/REJECTED direction (small sample)

Regime comparison (44 weekend hours vs 96 weekday hours, Sat/Sun UTC):

| Metric | Weekend | Weekday |
|---|---|---|
| cum \|BTC−ETH excess\| over 24h (vol proxy) | +0.0237 | +0.0278 |
| mean OI (contracts) | 438,474 | 450,239 |
| mean \|mark−index\| basis, bps | 3.96 | 3.79 |

Verdict: on this venue and sample, weekends are NOT measurably thinner —
|excess| vol is slightly LOWER on weekends, OI barely differs (−2.6%), and
the basis (the direct thin-liquidity proxy) is only 0.17 bps wider. The
idea's premise (weekend risk-off around known thin-liquidity windows) is
not supported on hyperliquid perps; as a *filter* it would add nothing here.
REJECTED for now on this venue — do not re-propose without a different venue
(bybit/OKX) or a different thin-liquidity definition. (One honest caveat:
8 days, and 08-08/09 were partial-coverage days — the known stale-burst
issue — so the weekend sample is the weakest part of the corpus.)

## What this means for the funnel

Two of three ideas are dead/untestable before a line of strategy code was
written — the event-study-first gate did its job cheaply. The remaining
(oi-purge) is parked on data availability, not on a hypothesis. Next corpus
milestone that changes these verdicts: more bybit/OKX symbols (weekend
study gets a second venue) and any session with real OI purges.
