# Backlog idea event studies — batch 2 (RES-4, 2026-08-16)

Graded the next two backlog alpha ideas over the recorded corpus BEFORE any
hypothesis was written — the funnel's cheapest gate (same runner as batch 1:
`research/run_backlog_event_studies.py`, record `backlog-event-studies-2026-08-16`
in `runs/index.jsonl`).

Corpus: hyperliquid BTC + ETH raw logs 08-08..08-15 (as batch 1), plus the
same-underlying cross-venue funding legs that exist in the corpus — binance
BTCUSDT/ETHUSDT (07-19, 08-08) and bybit BTCUSDT (08-14) — hourly
mark/OI/funding via `mp-query carry` (compute-on-read). Both new studies
grade **gap dynamics**: event = hour where a gap crosses a threshold, series
= hourly change in |gap|, so a NEGATIVE CAR[+24h] reads as mean reversion
toward 0. Cross-venue funding rates are annualized per venue cadence before
comparison (hyperliquid hourly ×8760; bybit/binance 8h ×1095 — units never
mixed).

## funding-arb-v1 — DIRECTION PRESENT, SAMPLE TOO SMALL (n=4 windows / 1 day)

The idea: long the perp on the low/negative-funding venue, short the
high-funding one, collecting the differential. First gate question: does the
same underlying fund at materially different rates across venues in our
corpus, and does that spread persist?

| Overlap (venue pair, day) | hours | mean spread bps/yr | mean \|spread\| bps/yr |
|---|---|---|---|
| HL BTC vs binance BTCUSDT 07-19 | 1 | +958 | 958 |
| HL BTC vs binance BTCUSDT 08-08 | 15 | −113 | 370 |
| HL ETH vs binance ETHUSDT 08-08 | 1 | +322 | 322 |
| HL BTC vs bybit BTCUSDT 08-14 | 17 | **+1080** | **1080** |

The 08-14 bybit day is the one real, multi-hour overlap: hyperliquid BTC
funded at its +1095 bps/yr cap ALL 17 overlapping hours while bybit BTCUSDT
moved from −1183 to +754 bps/yr — a spread that PEAKED at +2278 bps/yr
(08:00 UTC) and decayed monotonically to +341 bps/yr (23:00 UTC), day mean
+1080 bps/yr. Event study (|spread| ≥ 500 and ≥ 1000 bps/yr, CAR of Δ|spread|
to +12h — the longest horizon a 17h overlap can fill; +24h honestly reports
n=0):

| Threshold | n (complete) | CAR[+12h] | CI95 | mean \|spread\| over +12h |
|---|---|---|---|---|
| ≥ 500 bps/yr | 4 (raw 14) | **−1278** | [−1715, −693] | 1103 bps/yr |
| ≥ 1000 bps/yr | 4 (raw 9) | −1278 | [−1715, −693] | 1103 bps/yr |

Read: the hourly series (2026-08-17 correction — the batch-2 read claimed the
spread "persisted at ~1100 bps/yr for all 17 hours"; it did not). The spread
CONVERGED within the day: bybit funding caught up from −1183 to +754 bps/yr
while HL sat pinned at its cap, so the spread halved roughly every few hours
(+2278 → +341 by day end). The negative CAR[+12h] = −1278 bps/yr is the
spread collapsing, NOT "spike decay above a persistent plateau" — the
"mean |spread| over +12h ≈ 1103" number is a conditioned mean over the 4
complete-window events, all of which start EARLY in the day (spread ≥ 500/1000
bps/yr), and it does not represent the level available at a later exit. The
day's own last observation (+341) is below every entry threshold.

Verdict: NOT a verdict — the DIRECTION precondition is met (venues do fund
the same underlying at materially different annualized rates, HL ≥ bybit on
BTC), but the PERSISTENCE precondition is NOT met on the one multi-hour day:
the spread mean-reverts to below-threshold within ~10 hours. A harvestable
carry would have to be captured in the early-window hours, before bybit
catches up. This is n=4 complete windows on a single day — still not
gradeable to a verdict. The gate re-opens as soon as the multi-day bybit
overlap exists (the bybit ETHUSDT/SOLUSDT deploy + the 08-15.. bybit days in
the drain, backlog sequencing note) — that adds days, not just hours, to the
same-underlying spread series. Also noted: the trading cost leg (two venues'
fees + the two perp spreads) must clear the spread available at ENTRY/EXIT —
with fast convergence, the honest bar is the post-entry level, not the peak.

## basis-carry-v1 — dated-future leg NOT TESTABLE; perp-vs-oracle proxy: BTC mean-reverts, ETH does not

The idea: dated-future vs perp/spot basis harvest where listed (OKX/Binance
quarterlies). **The tradable leg is NOT TESTABLE by construction** — no
collector subscribes a dated-future feed (spec 002), so the corpus has no
quarterly basis to grade (same honest verdict as listing-flow-v1 in batch 1).

What IS recorded is the perp-vs-oracle basis (mark vs the venue's own index)
on hyperliquid — the funding-carry study's proxy. Gate question: when the
basis is wide, does the mark snap back toward the oracle (tight tracking =
low basis risk / small mean-reversion edge)?

| Symbol | hours | mean basis (bps) | mean \|basis\| (bps) | threshold | n | CAR[+24h] of Δ\|basis\| | CI95 | mean \|basis\| after 24h |
|---|---|---|---|---|---|---|---|---|
| BTC | 164 | −3.83 | 3.85 | ≥ 4 bps | 62 | **−0.637 bps** | [−0.93, −0.37] | 4.27 |
| BTC | 164 | −3.83 | 3.85 | ≥ 5 bps | 17 | **−1.175 bps** | [−1.63, −0.65] | 4.21 |
| ETH | 150 | −3.97 | 3.97 | ≥ 4 bps | 22 | +0.125 bps | [−0.33, +0.60] | 3.80 |
| ETH | 150 | −3.97 | 3.97 | ≥ 5 bps | 4 | +0.369 bps | [−0.17, +0.91] | 3.66 |

Read: on BTC, a wide perp-vs-oracle basis **does mean-revert significantly**
within 24h (CI excludes 0 at both thresholds, ~0.6–1.2 bps of tightening) —
the mark tracks the oracle tightly. On ETH it does not (CI crosses 0). The
reversion is small in absolute terms and the perp-vs-oracle basis is **not
directly tradable** (no spot leg; the funding-carry study's caveat still
holds), so this is evidence about mark-quality, not a tradeable edge.

Verdict: the dated-future basis-carry idea stays parked on data (needs an
OKX/Binance quarterly collector — or the spot-perp leg) — NOT TESTABLE, no
hypothesis written. The proxy result is a mild green light on hyperliquid
mark-quality (BTC basis mean-reverts; basis risk in any perp book is small),
and a caution that ETH's basis dynamics differ.

## funding-arb re-gate (r2, 2026-08-16) — second bybit day drained: precondition RECONFIRMED, still NOT GRADABLE

The 08-15 bybit BTCUSDT day drained into the corpus (record
`backlog-event-studies-2026-08-16-r2`, `runs/index.jsonl`), so the gate
re-ran over 2 bybit overlap days. New leg added to the harness:

| Overlap (venue pair, day) | hours | mean spread bps/yr | mean \|spread\| bps/yr |
|---|---|---|---|
| HL BTC vs bybit BTCUSDT 08-15 | 21 | **+440** | **512** |

08-15: hyperliquid BTC pinned at its +1095 bps/yr funding cap for 15 of 21
overlap hours while bybit BTCUSDT lagged (90–1086 bps/yr) — same direction
as 08-14 (HL ≥ bybit), mean |spread| **512 bps/yr**, 12 hours ≥ the 500
bps/yr entry threshold. The spread is real and directional on a SECOND
distinct day — but it did not persist there either: the 08-15 spread ranged
−486..+1005 bps/yr (HL came OFF its cap late day, −12 at 18:00) and the day
closed at +295. The extreme 08-14 level (+2278 peak) was that day's
crowding; the DIRECTION (HL ≥ bybit on BTC) is the robust part; the LEVEL
converges within the day on both days.

Event study on 08-15 (|spread| ≥ 500/1000 bps/yr): **n=0 complete windows
at both +12h and +24h** — the 21 overlap hours are split by an HL funding
gap (no HL funding rows 11:00–13:00 UTC), so every event window is broken
mid-span and the new day contributes no complete-window CAR evidence. The
+24h horizon remains n=0 by construction (no overlap day yet has 25
contiguous bars).

Verdict: unchanged in kind — **direction RECONFIRMED on 2 distinct overlap
days (HL ≥ bybit on BTC), persistence NOT confirmed (the level converges
within the day on both days), NOT GRADABLE**. FARB-2 (≥ 24h overlap on ≥ 3
distinct days) is still unmet: 2 bybit days in corpus (08-14, 08-15), no
ETHUSDT/SOLUSDT logs drained yet (only the 07-19 ETHUSDT from the earlier
batch). The re-gate fired on corpus change with no code change beyond adding
the drained day to the pair list (the harness's `--run-id` now distinguishes
re-gate records).

## What this means for the funnel

- **funding-arb-v1**: the arb's DIRECTION is real and stable (HL funds at or
  above bybit on BTC all three bybit days: 08-14 mean +1080 with a +2278
  peak, 08-15 mean +440, 08-16 mean +927; peak spread far above the
  two-venue cost leg). The PERSISTENCE precondition is NOT met on any day —
  the spread converges to below-threshold within ~10 hours (CAR[+12h]
  −1278, CI excludes 0), so a harvestable carry would have to be captured
  in the early-window hours. FARB-2 (≥ 3 distinct overlap days) is MET as of
  the 2026-08-18 re-gate (record `backlog-event-studies-2026-08-18-r2`,
  08-16 bybit days added to the pair list), but complete-window events
  still exist only on 08-14 (n=4, n_days=1, CI unreliable) — the verdict
  stays NOT GRADABLE on event breadth, not corpus breadth. Hypothesis
  written 2026-08-16 with the FARB-1..6 data spec (corrected 2026-08-17).
  Next corpus milestone: full multi-day bybit coverage of one symbol —
  the 08-17 bybit days drain tonight, while the host recording gap on
  08-17 means the HL side stays partial (drain collision policy keeps the
  local copy).
- **basis-carry-v1**: dead until a dated-future or spot leg is recorded. The
  perp-vs-oracle proxy says hyperliquid's BTC mark is tightly oracle-anchored
  — useful for basis-risk budgeting, not a trade.
- Batch 2 cost ~3 lines of strategy code and produced two honest gate
  verdicts; both ideas remain backlog items pending data, exactly the
  event-study-first gate's job (batch 1: same shape — 2 untestable/parked, 1
  rejected).
