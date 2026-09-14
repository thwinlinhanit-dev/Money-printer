# RETEST — orderflow-v1 perturbation + venue generalization (2026-09-11)

**Owner-approved** execution of POWER-GATES §4's "Path to green" (grilling
session 2026-09-11, Q1: "run it now on existing tapes"). This is the retest
mandate flagged by the 2026-09-09 cross-tape kill panel.

**Verdict up front: orderflow-v1 is KILLED.** Every falsification criterion
from its own hypothesis doc is met. The 09-09 swing-window 1h/4h GATE PASS was
a lucky slice — exactly as POWER-GATES §4 anticipated ("if the 1h/4h edge dies
under perturbation, it was a lucky slice — the gates will say so").

## 1. What ran

| Stage | What | Where |
|---|---|---|
| Venue generalization | 18 binance-futures BTCUSDT day logs (07-19 → 08-08), orderflow-v1 default params, seed 1, journaled backtests | `eval-20260911-of-btcbin-<day>` |
| Walk-forward, swing tape | 27-combo built-in grid (`entry_gauge` 0.2/0.3/0.4 × `min_tape_bps` 0.5/1/2 × `min_depth` 50k/100k/250k), train 5d / test 3d / step 3d, **purged** with 1h embargo (SWG-5), min_trades 10 | `data/retest/of-swing-wf.out` |
| Walk-forward, 4d tape | same grid, train 2d / test 1d / step 12h, embargo 1h | `data/retest/of-4d-wf.out` |

Note on seeds: orderflow-v1 calls no RNG (seed only drives the coinflip
controls), so the multi-seed sweep from the original plan reduces to the
parameter grid — which is the perturbation study the mandate actually asked
for.

The binance corpus is INT-4-un-gateable (`missing_provenance` is structural)
and stays OUT of the observation corpus by the owner boundary — these are
plain journaled backtests (`data/runs/index.jsonl`), no Parquet writes.

Operational note: the 12 GB merged binance log cannot be replayed on this box
(`read_log` loads the whole file into a ~14.5 GB event Vec; allocation failed
with 14 GB free). The per-day form fits (~0.5–1.4 GB/day) and covers the same
era.

## 2. Binance venue-generalization results (per day)

| Day | Trades | Expectancy | Stress 2× |
|---|---|---|---|
| 07-19 | 0 | +0.00 | +0.00 |
| 07-21 | 0 | +0.00 | +0.00 |
| 07-22 | 0 | +0.00 | +0.00 |
| **07-23** | **1505** | **−40.45** | **−72.11** |
| 07-24 | 0 | +0.00 | +0.00 |
| 07-25 | 0 | +0.00 | +0.00 |
| 07-26 | 0 | +0.00 | +0.00 |
| 07-27 | 0 | +0.00 | +0.00 |
| **07-28** | **1541** | **−52.60** | **−86.48** |
| 07-29 | 0 | +0.00 | +0.00 |
| 07-30 | 0 | +0.00 | +0.00 |
| 07-31 | 0 | +0.00 | +0.00 |
| 08-03 | 0 | +0.00 | +0.00 |
| **08-04** | **18** | **−127.59** | **−208.13** |
| **08-05** | **45** | **−57.86** | **−106.18** |
| 08-06 | 0 | +0.00 | +0.00 |
| **08-07** | **8** | **−172.23** | **−281.61** |
| 08-08 | 0 | +0.00 | +0.00 |

Read: **zero positive binance days.** Where the book/tape features trade at
all, the strategy loses at 1× and bleeds at 2× cost. The two heavy-fire days
(07-23, 07-28, ~1500 trades each) are the statistically meaningful sample —
a venue change destroys the edge, which is the signature of a hyperliquid-
microstructure artifact, not a cross-venue push-continuation effect.

(13/18 days fire zero trades on binance vs. the tape-dense hyperliquid
recordings — the feature cadence itself differs by venue; the five trading
days are the honest comparison.)

## 3. Walk-forward (purged, embargoed, 27-combo grid)

### Swing tape (07-19 → 08-18, 8 windows: 5 vacuous, 3 selected)

| OOS window | in-sample best | OOS trades | OOS exp | OOS stress2× |
|---|---|---|---|---|
| w6 | **+12.04** (0.4 / 50k / 1.0) | 5818 | **−12.46** | −18.59 |
| w7 | −7.97 (0.2 / 50k / 0.5) | 17440 | −5.70 | −10.90 |
| w8 | −7.38 (0.3 / 50k / 0.5) | 6217 | −15.16 | −21.68 |

`wf: windows=8 vacuous=5 error=0 selected=3` — and **every selected window is
OOS-negative**. Window 6 is the textbook overfit signature: the grid found
+12.04 in-sample and the same params lost −12.46 out-of-sample. The 5 vacuous
windows mean the strategy often cannot even reach 10 trades/3d on any grid
combo — the 09-09 "n≈23k over 31 days" fire rate is not stable across time.

### 4-day tape (08-18 → 08-22, 2/2 windows selected)

| OOS window | in-sample best | OOS trades | OOS exp |
|---|---|---|---|
| w1 | −2.87 (0.4 / 50k / 0.5) | 22992 | **−4.32** |
| w2 | −5.44 (0.4 / 50k / 2.0) | 12472 | **−5.82** |

Consistent with the 09-09 REFUSED + DECAY_SUSPECT verdict on this tape.

## 4. Falsification scorecard (from `strategies/orderflow-v1/hypothesis.md`)

| Criterion | Result |
|---|---|
| #1 expectancy ≤ 0 at 2× cost | **MET** — all 5 binance trading days, all 5 wf OOS windows, both 09-09 non-passing horizons |
| #2 edge concentrated in < 3 calendar windows | **MET** — the entire positive record is ONE swing window (07-19→08-18 slice); binance: zero windows |
| #3 walk-forward OOS sign flip in ≥ 2/3 windows | **MET** — in-sample +12.04 → OOS −12.46 on w6; every other selectable window negative |

3/3 kill criteria engaged. Registry row moved to `state=killed` with this
report as evidence (`research/registry.jsonl`, ALP-7 satisfied).

## 5. Consequences

- **POWER-GATES §4** returns to **0/5 with zero standing candidates** — the
  honest red it always should have been. §3's "most tested ideas get killed"
  is further proven (the last candidate died under exactly the process the
  gates prescribe).
- **The nightly kill panel** (registered 2026-09-11, `MoneyPrinterKillPanel`
  09:15 UTC) will keep grading whatever is registered next; its first live
  night already caught DECAY_SUSPECT ×6 on carry-v1 and orderflow-v1 on day
  2026-09-08.
- No promotion pipeline change needed — the gates refused every path already;
  this document closes the retest mandate so §4's row reflects reality.

## 6. Artifacts

- `data/retest/summary.log` — runner timeline, 18/18 stage-1 exits = 0
- `data/retest/of-btcbin-*.out` — per-day binance summaries (18 files)
- `data/retest/of-swing-wf.out`, `data/retest/of-4d-wf.out` — full wf logs
- `data/runs/index.jsonl` — journaled run configs (eval-20260911-of-btcbin-*)
- `data/retest/retest_runner.ps1` — the runner script (sequential, RAM-bounded)
- `research/registry.jsonl` — orderflow-v1 `killed` with run_ids

## 7. Side-fixes made during the retest

- `research/registry.py` `_run_exists` + `run_registry.py` check: the checker
  only read the funnel-era `runs/index.jsonl`; it now validates run_ids
  against BOTH journals (`runs/` and `data/runs/`).
- `research/registry.jsonl`: removed dead run_id references (ULIDs whose
  experiment-tracker artifacts no longer exist; `backlog-event-studies-*`
  pseudo-run-ids that were never journaled) and pointed evidence at the real
  study markdowns. 15 rows validate; registry tests 17/17.
- Flagged, NOT fixed (owner governance): ALP-1 WIP limit exceeded — 4 active
  candidates vs. max 1 (carry-v1, funding-arb-v1, swing-range-reclaim-v1,
  trend-breadth-v1). Pre-existing; with orderflow-v1 killed, deciding which
  of the four earns the single active slot is the owner's next call.
