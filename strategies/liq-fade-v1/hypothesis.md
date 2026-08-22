# liq-fade-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
Forced liquidations print at whatever price the exchange can fill — often far
from fair value. A sell cascade (longs being dumped) pushes the book down and
marks liquidation prints well below mid; a buy cascade (shorts squeezed) does
the mirror above it. The forced seller/buyer pays ANY price to exit, so they
accept the loss. Once the cascade is done, the mechanical pressure that moved
price is gone, and the market tends to revert toward the level the book says
is fair — the stretch created by forced flow, not information, mean-reverts.
We fade the cascade AFTER exhaustion: we do NOT catch the knife; we wait until
the flow has demonstrably stopped accelerating and then take the side of
reversion. Counterparty: late momentum chasers who extrapolate the cascade's
last prints. Short-horizon mean-reversion trade, NOT a continuation trade and
NOT a long-horizon position.

## Signal decomposition (features, spec 004 §Liquidation flow)
- `liq.vol_sell` — rolling Σ sell-side liquidation notional (5-min window).
  The cascade measure: big value = longs being dumped.
- `liq.vol_buy` — mirror: shorts being squeezed.
- `liq.dist` — liquidation price-distance from mid, bps, at each liq print.
  The stretch measure: how far from fair value the cascade is hitting.
- `liq.rate` — rolling liquidation event rate (events/sec). Context: the
  cascade is many events, not one print.

Entry (sell-cascade fade → BUY, mirrored for the buy cascade):
- `liq.vol_sell` ≥ `entry_vol` (a real dump, not noise), AND
- `liq.dist` ≥ `entry_dist_bps` (the prints are STRETCHED from fair — the
  knife has already fallen far), AND
- **exhaustion**: current `liq.vol_sell` ≤ `exhaust_frac` × the recent peak
  of `liq.vol_sell` (the rolling sum is DRAINING because the cascade stopped
  printing — flow is over, not paused mid-swing). This is the whole edge: we
  fade only what has already stopped moving price.

Exit:
- `liq.dist` < `exit_dist_bps` — the stretch is gone (reversion complete), OR
- `liq.vol_sell` makes a NEW peak above the entry peak — the cascade
  re-accelerated; the fade was wrong, cut it, OR
- hard time stop (default 4h).

## Regime dependency: declared_regime + why
The edge needs a liquidation cascade to exist at all, so it is inherently
episodic — it lives in volatile, cascade-prone sessions and is dead in calm
tape (and dead whenever the venue has no liquidation stream: hyperliquid
today). Declared regime: any, with the expectation that the edge concentrates
in high-vol windows (that IS the signal — cascades happen there). The
falsification window-count rule below is the honest check that we are fading
real cascades, not one lucky week.

## Falsification (written BEFORE any backtest)
Kill if, over the recorded history with full costs (entry+exit taker fees):
- expectancy ≤ 0 in the 2×-cost column at G1, OR
- the edge concentrates in < 3 distinct calendar windows (not a harvest, a
  fluke), OR
- walk-forward OOS flips sign vs in-sample in ≥ 2 of 3 windows (curve fit,
  not an edge).
The strategy must also prove determinism: two identical-seed runs over the
same log must produce byte-identical decision logs (spec 018 discipline).

## Honest data gate (written BEFORE implementation)
The `liq.*` features require a recording with a liquidation stream — Bybit's
public `allLiquidation.` topic (COL-29, spec 024). The current corpus is
hyperliquid-only, which has NO native liq stream, so `liq.*` never emits on
it. Therefore: this strategy CANNOT be backtested on the corpus as it stands.
The v1 deliverable is the hypothesis + implementation + unit tests (state
machine proven on synthetic feature updates) + sim registration; the honest
first verdict is "no data — not falsifiable yet," recorded as such, and the
backtest gate opens when a bybit recording day exists. No synthetic
liquidation data will be invented to fake a verdict (PD-6/INT-1: the audit
judges provenance, and so does the funnel).

## Expected characteristics
Horizon: minutes–hours (entry after exhaustion, exit on reversion or
re-acceleration). Trade rate: episodic — a handful per day in cascade-heavy
sessions, near zero otherwise. Hit-rate shape: moderate; the winner is the
stretch collapse, the loser is the re-accelerating cascade (which the new-peak
cut keeps bounded). Costs dominate at this horizon — the 2×-cost column at G1
is the first gate.

## Honest scope for v1
Single venue (bybit, once recorded), market intents only (IOC), no position
scaling, no venue arbitrage, no cross-cascade (buy cascade + sell cascade on
the same window do not compound). The `liq.*` features are new (spec 004,
2026-08-13); this hypothesis is their first consumer in the funnel.

## Edge results (first real data, 2026-08-14)
The data gate opened with the first bybit recording day (08-14, COL-29).
First real-data run: bybit BTCUSDT slice 07:54:56→10:30:13 UTC, 463,224
events, **175 liquidations** (runs `01M00SJJPNDVA46811VDM8KA80` +
determinism rerun, `runs/index.jsonl`). Result: **trades=0**, deterministic
(decision-log hash 8929519729917322587 identical across 4 runs incl.
relaxed `entry_dist_bps` 1/0.5/0.1).

Why zero — the honest decomposition (feature materialization over the same
slice, `liq.*` parquet):

- **Stretch is trivially met**: `liq.dist` emitted on all 175 prints, range
  33–51 bps (mean 48.4) — the bybit forced tape prints well through the
  book, and the book mirror stayed clean enough to read mid. The 30 bps
  `entry_dist_bps` floor is not the binding constraint.
- **Magnitude is the blocker**: rolling 5-min `liq.vol_buy` peaked at
  $467K vs `entry_vol` $1M; `liq.vol_sell` was near-absent (4 prints,
  ≤ $15K). This window was a short-squeeze tape (buy-side liquidation
  orders dominant), but no single-side 5-min rolling notional reached the
  $1M cascade floor, so exhaustion never became eligible.
- **Pipeline proven live**: `liq.*` emits on real bybit data — this is NOT
  the old "no data" verdict. The kill/falsify machinery finally has
  something to grade.

## Full-day verdict (2026-08-14 closed day, after VPS gate + nightly drain)
The complete 08-14 bybit BTCUSDT day (584 MB, 2,590,773 events, gate
`promotable: true`; landed locally via the nightly drain, sha256-verified
`ed3417ad…` = VPS) materialized and backtested: run `01M01A640WJBGD9NTX7MP2Q61R`
(`runs/index.jsonl`), decision-log hash 17538781418735680594 — **trades=0 again**,
now over the FULL day: 419 liquidation prints across 14.8 h (07:54→22:42 UTC).

**Slice vs full-day, same default params (entry_vol $1M, seed 42):**

| | Partial-day slice | Full day |
|---|---|---|
| Window | 07:54:56→10:30:13 | 07:54:56→22:42 UTC |
| Events | 463,224 | 2,590,773 (5.6×) |
| Liquidations | 175 | 419 (2.4×) |
| Run id | `01M00SJJPNDVA46811VDM8KA80` | `01M01A640WJBGD9NTX7MP2Q61R` |
| Decision-log hash | 8929519729917322587 | 17538781418735680594 |
| Trades | 0 | 0 |
| `liq.vol_buy` max | $467K | $513,548 |
| Verdict | no qualifying cascade | **same** — 0 readings ≥ $600K all day |

Both windows: trades=0 at the default $1M floor — the full day adds 244 more
liq prints and 3 distinct cascade events (08:33 $467K, 13:11 $513K, 14:21
$418K) but NOT one ≥ $1M, so the honest verdict is unchanged and now covers
the whole day: the floor is unreachable on this tape, not the pipeline dead
(the $1M calibrations are examined below; the calibrated-floor run trades and
loses — see Walk-forward section).

`liq.vol_*` rolling 5-min one-sided distribution (materialized parquet):

- **`liq.vol_buy`** (339 readings): mean $158K, p50 $109K, p90 $359K, p99 $510K,
  **max $513,548** — and **0 readings ≥ $600K** all day. 51.6% of readings
  ≥ $100K, 28% ≥ $250K, 15.6% ≥ $300K, 8.3% ≥ $400K, 2.9% ≥ $500K.
- **`liq.vol_sell`** (80 readings): **max $80,699** — the sell side is
  structurally tiny on this tape; flow is one-directional (buy-side).
- **`liq.dist`**: mean 48.3 bps, p50 49 bps, max 55 bps — stretch is met all day.
- Distinct buy-side cascade events (≥5 min apart, ≥ $200K): **3** —
  13:11 UTC ($513,548 peak, sustained > $300K for ~115 s), 08:33 ($467K),
  14:21 ($418K). Only ONE event crossed $500K; none came close to $1M.

**Decision: yes — `entry_vol` should be venue-calibrated for bybit.** The
$1M floor is ~2× the largest single-side 5-min flow the venue produced in a
full day (max 0.51× the gate), so v1's floor is unreachable on this tape by
construction, not by absence of cascade structure. The 13:11 event is a
textbook cascade (peak $513K, sustained > $300K for ~2 min) that v1's
`exhaust_frac` drain logic would have faded had the floor been ~$250–400K
(peak × 0.8 = $411K drain line crossed while still ≥ floor).

**Calibration implemented (2026-08-15):** per-venue `entry_vol` values
$250K/$300K added to the sim grid (`strategies/src/liq_fade_v1.rs`), keeping
$500K/$1M/$2M for the multi-venue v2 aggregate semantics.

## Walk-forward (first real data, 2026-08-14 day)
`sim wf` over the full day, train=4h / test=4h / step=2h → 5 windows, 45-combo
grid (entry_vol 250K–2M × entry_dist_bps 15/30/60 × exhaust_frac 0.6/0.8/0.9),
seed 42.

**Result (rerun with the SIM-9 min-trades fix, run `01M037PPE2BJATDYBXHZ1DW7T2`):**

- `--min-trades 10` (default): **5/5 windows VACUOUS** — no grid combo
  reaches 10 trades in any 4h train slice (the strategy trades ≤3×/day on
  this tape), so the fix correctly refuses to select anything. The old
  output — `in_exp=0.000000, best_params = first combo` on every window —
  was the degenerate argmax this fix eliminates: every trading combo loses
  (e.g. entry_vol=250K/15bps/0.8 → trades=1, exp=−264 in the 07:54→11:54
  window), every 0-trade combo scores exactly 0.0, so the old argmax picked
  a non-trading combo and reported a false pass.
- `--min-trades 1` (the only setting that admits a trading combo): **1
  SELECTED / 4 VACUOUS**. The selected window (07:54→11:54 train) picks
  entry_vol=250K/15bps/0.8 with **in_exp = −264.15** — the only combo that
  traded, selected with a negative in-sample expectancy, OOS vacuous (0
  trades in the test slice).

Read honestly: the fix does its job — a window with no eligible combo is now
VACUOUS, never a false pass. And the one SELECTED verdict is negative, so the
within-day walk-forward remains kill-direction on 08-14.

The strategy DOES trade on real bybit data once the floor is calibrated —
probe run, full 08-14 day at entry_vol=250K/15bps/0.8 (run
`01M01MTATAWXTJP5GB29ZW02M0`, `runs/index.jsonl`): **trades=3, expectancy
−969.5, stress2x −1261.1, maxDD $3,072** — 6 intents, 6 fills, all gate Pass.
Three round trips, all losers.

**Grade against the falsification gate:** criterion #1 (expectancy ≤ 0 in the
2×-cost column) is ENGAGED on the first real data — stress2x = −1261 < 0. The
within-day walk-forward cannot certify an edge: no in-sample combo is
profitable, and the combos that trade lose even at 1× cost. Caveats: 3 trades
is a tiny sample and ONE day; the multi-day read (walk-forward over the
closed 08-14 + 08-15 days, the ≥2-of-3-windows rule) is the confirmation run
and can only fire once 08-15 closes + drains (00:05 gate, 01:00 drain). The
sell-side floor is moot on this tape (max $81K). The exact grade procedure
for that confirmation run is below; the verdict is machine-computed by
`ops/scripts/grade_wf.py`, not hand-argued.

## Cross-day walk-forward (08-14 + 08-15 closed days) + machine grade

08-15 closed and landed by the nightly drain (gate 2026-08-15
`promotable: true`, bybit BTCUSDT clean, 2,114,156 events, 0 findings;
file 425,241,823 bytes = VPS). Machine grade run `01M044WTG50DEVKEM863NSBFF5`
(`runs/index.jsonl`) via `ops/scripts/grade_wf.py` over the captured
`--min-trades 1` legs of both days (13 windows):

- **08-15 leg: 8/8 VACUOUS at both min-trades settings** (run
  `01M0458CZWYF0GE7MCN2KHVH1B`: `--min-trades 1` AND `--min-trades 10`)
  — no grid combo trades even once in any 4h slice. The tape explains it
  (census over the file): **26 liquidation prints all day** (13 buy / 13
  sell, ~$25K total notional) vs 419 on 08-14 — a quiet cascade day, no
  episode anywhere near the $250K floor, so nothing qualifies regardless of
  the exhaustion anchor. The funnel's "episodic by construction"
  prediction, confirmed on the second day.
- **08-15 G1 full-day at the calibrated 250K/15/0.8: trades=0** (probe,
  same production engine; `stress2x=0`, `maxdd=0`). Excluded from C1 by
  the traded-day guard — a 0-trade day is absence of evidence, never a
  degenerate 0.0 pass.
- **Grade: OVERALL KILL** — C1 ENGAGED (08-14 G1 `stress2x −1261.13`, the
  only traded day); **C3 NOT-GRADED** (gradeable=0 of 13 windows — the
  ≥2-of-3 rule cannot be exercised at this trade frequency, exactly the
  funnel's verdict); C2 NOT-GRADED (2-day corpus < 3).

Read honestly: the cross-day confirmation keeps the kill from the only day
that produced any trade, and adds the structural fact that a quiet cascade
day yields zero trades by construction. The falsification gate has done its
job via criterion #1; the ≥2-of-3 rule still needs multi-day windows and
more cascades/day (the ETHUSDT/SOLUSDT legs, now recording, spec 032)
before it can certify anything.

## Grade procedure — the ≥2-of-3-windows rule (machine-checkable)

This is the EXACT procedure for the falsification rule "walk-forward OOS
flips sign vs in-sample in ≥ 2 of 3 windows (curve fit, not an edge)", as
implemented by `ops/scripts/grade_wf.py`. The verdict is deterministic from
the captured `sim wf` stdout — no hand interpretation at grade time.

**Inputs.** One captured `sim wf` stdout file per recording day (the
`--min-trades 1` variant — the only variant that can produce SELECTED
windows on this tape; `--min-trades 10` is recorded separately as the
conservative no-eligible-combo read), each labeled with its calendar day;
plus each day's G1 full-day 2×-cost expectancy (`stress_expectancy_2x`) and
trade count. The two days share shape (train=4h/test=4h/step=2h), seed 42,
and the 45-combo grid, so their windows are directly comparable.

**Window classification** (mechanical, per `window ... verdict=` line):
1. `VACUOUS` — no grid combo reached min-trades on the train slice; the
   window carries NO selection and contributes nothing to any count.
2. `SELECTED` with `oos_trades = 0` — **OOS-inconclusive**: the selection
   was never exercised out-of-sample; reported but excluded from the flip
   count (zero OOS trades cannot evidence a curve fit).
3. `SELECTED` with `oos_trades ≥ 1` — **gradeable**: carries a sign pair
   (`in_exp`, `oos_exp`).

**Flip definition.** A gradeable window flips iff `in_exp × oos_exp < 0` —
strictly opposite NONZERO signs. A zero expectancy on either side is never a
flip (this is what keeps an OOS-inconclusive window from ever counting). Sign
only: a small-magnitude flip counts the same as a large one.

**Cross-day aggregation.** The rule grades the UNION of the days' windows in
chronological order, not each day separately — the unit of evidence is the
selection, wherever it occurred.

**Criterion #3 firing.** With `G` gradeable windows and `F` flips:
- `G < 3` → **NOT-GRADED** ("rule cannot be exercised at this trade
  frequency"); this NEVER reads as a pass.
- `G ≥ 3` and `F ≥ ⌈2G/3⌉` → **FIRED** (kill). The anchor case `G = 3`
  needs `F ≥ 2`, matching the rule's literal wording; larger G scales the
  same 2/3 rate (e.g. `G = 4` needs `F ≥ 3`) so extra windows cannot
  dilute the test.

**Full falsification gate** (the rule's OR — kill if ANY fires):
- **C1** — ENGAGED iff any day's G1 `stress_expectancy_2x ≤ 0` (already
  true on 08-14: −1261).
- **C2** — FIRED iff the edge traded on < 3 distinct calendar days AND the
  corpus has ≥ 3 days. With a < 3-day corpus this is NOT-GRADED: the corpus
  cannot distinguish harvest from fluke, and auto-killing on corpus size
  would reproduce the SIM-9 degeneracy (a verdict driven by absence of
  data, not presence of evidence).
- **C3** — FIRED as above.
- else **NOT-YET-FALSIFIED**.

**Current machine state (08-14 + 08-15, graded 2026-08-16, run
`01M044WTG50DEVKEM863NSBFF5`).** Cross-day union of 13 windows: 1 SELECTED
(08-14, OOS-inconclusive — `oos_trades=0`), 12 VACUOUS → `gradeable=0,
flips=0` → **C3 NOT-GRADED**; **C1 ENGAGED** (08-14 G1 stress2x −1261.13;
08-15's 0-trade G1 excluded by the traded-day guard) → **OVERALL: KILL**.
C2 NOT-GRADED (corpus 2 days < 3). The rule returns its designed terminal
state: the trade frequency is too low for any within-day window shape to
exercise it (funnel analysis below).

## Why ≤3 trades/day — funnel analysis (2026-08-15)

The 08-14 bybit BTCUSDT log (419 liquidation events) replayed through the
EXACT sim feature engine, with the strategy's decision path replicated gate by
gate (scratch probe, removed after use — same pattern as the earlier
`liq_probe.rs`):

**Funnel at the calibrated 250K/15/0.8 combo (per qualifying-reading counts):**

| Gate | Readings killed | Note |
|---|---|---|
| `entry_vol` ($250K) | 256 | 61% of all readings die here — the big filter |
| `entry_dist_bps` (15) | **0** | dist is NEVER the binding gate at 15/30 — every qualifying vol reading already has dist ≥ 30bps (only 60 bps binds: 92 killed) |
| exhaustion (`0.8×peak`) | 63 | the second real filter |
| stale (`30s`) | 71 | all dist-stale, 0 vol-stale — `liq.dist` goes stale when the book is gapped/FEA-8-silent |
| **entered** | **29** | collapse to **3 real trades** via the state machine (one per episode) |

**The three gates ranked by actual binding power:**

1. **`entry_vol` is the dominant constraint, but for a structural reason:** the
   sell side is effectively ABSENT on this tape — max sell-cascade episode
   notional is **$80,699**, below even the $250K floor (11 sell episodes, all
   < floor). Only the BUY side can ever qualify, so the long-fade leg is dead
   by construction here. On the buy side, only **3 of 23 distinct cascades
   reach ≥$250K** (episodes at 08:19 $467K, 13:02 $513K, 14:05 $418K); the
   day-max 5-min flow is $513K, so the $1M default floor is unreachable by
   construction (0 episodes ≥ $1M) — this is cascade scarcity, not gate
   tuning.
2. **The exhaustion gate is the second filter, and it has a subtle
   window-dependence:** the strategy anchors exhaustion to the RUNNING peak
   of the current cascade side (`peak_vol_*` is a day-level high, never
   reset). Full-day, the 13:02 and 14:05 episodes qualify because the anchor
   is the 08:19 peak ($467K) — `vol ≤ 0.8×peak` is satisfied on the *rise*
   of the later cascades ($253K ≤ 0.8×$467K). Window-local (the wf's 4h
   train slices), the peak starts at 0, so the same 13:02 episode does NOT
   qualify until its own peak drains — and the wf's window-1 train
   (09:54→13:54, containing the 13:02 $513K cascade) trades **0** through the
   real strategy. The full-day funnel's 29 qualifying readings are anchored
   to earlier peaks; the wf sees a different, stricter exhaustion surface.
3. **`entry_dist_bps` is NOT the bottleneck at 15/30:** bybit liquidation
   prints are already ≥30bps from mid whenever vol qualifies — the stretch
   gate never binds at the tested low end. (60bps binds: 92 kills.)

**Can the within-day walk-forward certify an edge at this frequency? No.**
With ≤3 trades/day and episodes concentrated in ~3 bursts, a 4h train slice
contains at most one qualifying episode (usually zero): at `min_trades=10`
every window is VACUOUS (correct but uninformative), at `min_trades=1` only
the window that CONTAINS a full build-and-drain cycle (window 0, 07:54→11:54)
selects — and even that one trade loses. Two compounding reasons: (a) the
4h/4h/2h shape slices a 3-trade day into windows that mostly see nothing;
(b) the window-local peak reset makes the exhaustion surface STRICTER than
full-day (a build-and-drain must complete inside the slice). The ≥2-of-3
falsification rule cannot be exercised on this data — the trade frequency is
simply too low for any within-day window shape to accumulate statistical
power (would need ~10 days of cascades for a 30-trade sample). The honest
path to a certifiable read is NOT smaller windows: it is MORE data per
window (multi-day windows) and MORE cascades per day (bybit ETHUSDT/SOLUSDT
legs + the multi-venue v2 aggregate), which is exactly the spec 032 deploy
and the v2 direction.
