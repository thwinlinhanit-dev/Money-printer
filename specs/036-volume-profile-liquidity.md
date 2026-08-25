# Spec 036 — Volume-Profile Liquidity Features + Range-Reclaim Swing Strategy

Derived from the owner's KillaXBT-confluence draft (`docs/swing-liquidity-features-spec.md`),
renumbered into the repo spec sequence. Governs the `swl` requirement set.
Addendum to spec 004 (feature engine), 006 (strategy API), 035 (swing focus) —
it replaces none of them.

**Status:** §1–§6 implemented. §7 POC-flip + A/D classifier implemented 2026-08-22 (see §7 status); regime-tag plumbing remains deferred. Originally: MVP scope approved by the owner 2026-08-21;
POC-flip state machine, accumulation/distribution classifier, and manual
regime tag are deferred (see §7). Non-goals carry over unchanged from the
draft §0: no fractal/cycle pattern matching, no sentiment/narrative inputs,
and **never** the leveraged re-entry ladder ("God Mode") — it is structurally
a martingale with no bounded invalidation and can never satisfy the EDGE
bounded-loss gate (§6). It must not be implemented as strategy-crate code
under any configuration.

## 1. Data requirements (SLQ-D)

- All features in this spec are computed from closed BARS ONLY (SWG-2
  contract — swing features never require tick or L2 inputs). Volume-at-price
  is therefore always the OHLCV approximation path of draft §1: bar volume
  bucketed by close price. Every output of this family is approximate by
  construction; it is never blended with tick-derived volume in the same
  profile computation.
- Timeframe: daily bars primary (`RebalanceCadence::Daily` dispatch, SWG-3/7).
- Windows configurable via `[swing]` config; defaults per §2/§3 below.

## 2. Feature family: `swing.atr`, `swing.range.*`, `swing.profile.*`

All computations are pure functions over `&[Bar]` slices plus thin `BarFeature`
adapters that keep bounded windows and emit at most once per closed bar
(FEA-3 warmup semantics). Degenerate input (empty window, non-finite or
non-positive values) fails closed to `None`/no-emission (CONV-8).

### 2.1 ATR (SLQ-A)

- Wilder ATR(n): TR = max(high−low, |high−prev_close|, |low−prev_close|);
  ATR = mean(TR) over the trailing n closed bars (first bar of a series has
  no prev_close → TR = high−low). Emitted as absolute price units under
  `swing.atr.{n}` (default n=20). Warm after n bars.

### 2.2 Value-area volume levels (SLQ-V)

- `volume_levels(bars, bucket_size, hvn_frac, lvn_frac)` extends the existing
  close-bucketed profile: POC = max-volume bucket; value area = tightest
  contiguous band around POC covering ≥70% of window volume (unchanged from
  spec 035 SWG-2); HVN = local volume maxima with bucket volume >
  `hvn_frac` × POC volume (default 0.7); LVN = local minima with bucket
  volume < `lvn_frac` × POC volume (default 0.3). Local extrema are judged on
  each occupied bucket against its adjacent occupied neighbors.
- Nearest-level adapters emit one scalar each vs the latest close:
  `swing.profile.hvn_above.{win}` / `swing.profile.hvn_below.{win}` /
  `swing.profile.lvn_above.{win}` / `swing.profile.lvn_below.{win}` —
  the closest level strictly above/below the close; absent when no such level
  exists. Default win=90 bars (draft §1 lookback).
- `swing.close` — the closed bar's close price. A trivial passthrough, but
  required: every strategy-side exit rule in §3 is CLOSE-evaluated (draft
  §4.2), and this is the only bar-only channel that carries it.

### 2.3 Range detection + liquidity sweep detector (SLQ-R)

Definitions (the draft's §3.1 compression wording is ambiguous; this is the
explicit repo definition):

- Baseline volatility: ATR(`atr_n`) computed over the `atr_n` bars immediately
  BEFORE the range window (no self-reference).
- A compressed range exists at bar i when the mean true-range of the last
  `range_n` bars < `compress_frac` × baseline ATR (defaults 20 / 0.6).
  Range high = max(high) and range low = min(low) over those `range_n` bars;
  emitted as `swing.range.high.{n}` / `swing.range.low.{n}` while defined.
- Sweep-high at bar s: `high[s] > range_high + sweep_atr_mult × baseline_ATR`
  AND `vol[s] >= vol_mult × mean(vol over the range window)` (wick-volume
  filter, defaults 0.1 / 1.5). Sweep-low mirrors.
- Reclaim confirmed iff within `reclaim_z` bars after the sweep bar (default
  2) a bar CLOSES back inside `[range_low, range_high]`.
- On confirmation, exactly two emissions occur on the confirming bar:
  - `swing.sweep.low.{n}` (or `.high.{n}`) — value = the sweep wick extreme;
  - `swing.sweep.low.stop.{n}` (or `.high.stop.{n}`) — value = invalidation
    price = extreme −/+ `sweep_stop_buffer_atr` × current ATR. The stop
    travels WITH the event (self-contained, order-independent); strategies
    consume it directly rather than recomputing it from a separate ATR
    reading.
- The buffer is feature-family configuration (`sweep_stop_buffer_atr`,
  default 0.5), NOT a strategy parameter — the strategy grid varies only
  position-management knobs (§3).
- One pending sweep at a time; a new sweep before confirmation replaces it.

## 3. Strategy: `swing-range-reclaim-v1` (SLQ-S)

Conforms to the spec 006 `Strategy` trait; single position state machine
`Idle → EntrySignaled → Entered → ExitSignaled → Idle` (liq-fade-v1 shape);
market intents only; risk units per the crate-wide convention
(`risk_pct / PER_RISK_UNIT_PCT`, clamped); the risk gate owns contracts.

- Cadence: `RebalanceCadence::Daily`, `holding_period_bars() = [2, 60]`
  (SWG-3 metadata; sim dispatches on daily bar close only, SWG-7).
- Subscriptions (prefix forms): `swing.sweep.` , `swing.range.` , `swing.atr.` ,
  `swing.profile.`.
- Entry long: fresh `swing.sweep.low` event while Idle. Entry short: mirror on
  `swing.sweep.high`. One entry, one defined risk — no scaling, no ladder.
  Optional confluence filters (POC-flip within W bars) are NOT in the v1
  entry rule (deferred, §7).
- Invalidation: evaluated on closed bars of the entry timeframe ONLY — if the
  latest close is beyond the stop price, emit a full market exit. No intrabar
  stops on HTF setups (draft §4.2 verbatim). Because every entry carries this
  close-evaluated invalidation level, each trade has a bounded, defined max
  loss at entry time — the §6 hard gate is satisfied structurally.
- Targets (draft §4.3, conservative bias):
  - T1 = min(distance-to-opposite-range-boundary, distance-to-nearest LVN
    beyond it) for longs from the entry fill reference; mirror for shorts.
    When no LVN level is known, T1 = opposite boundary.
  - At T1: exit HALF the position; stop moves to breakeven; the remainder
    trails at `trail_atr` × ATR (default 2.0) behind the best close since T1,
    toward the next LVN/HVN. Trail exit on close through the trail stop.
  - Time stop: force exit after `max_hold_bars` (default 60) held bars.
- Params grid (SIM-9): `trail_atr` ∈ {1.5, 2.0, 3.0}. (The stop buffer is
  feature-family config, §2.3 — not a strategy parameter.)

### Hypothesis & honest data gate

`strategies/swing-range-reclaim-v1/hypothesis.md` states the falsifiable
edge claim and records the data gate honestly: the daily-bar corpus is young;
until ≥30 OOS trades exist the verdict is "no data" — recorded as such, never
faked (same posture as liq-fade-v1).

## 4. Config surface

New `[swing]` keys (serde-validated; unknown keys rejected, FEA-7):
`sweep_range_n` (20), `sweep_compress_frac` (0.6), `sweep_atr_mult` (0.1),
`sweep_reclaim_z` (2), `sweep_vol_mult` (1.5), `sweep_stop_buffer_atr` (0.5),
`sweep_atr_n` (20), `profile_window` (90), `profile_hvn_frac` (0.7),
`profile_lvn_frac` (0.3). Registered by `engine_from_config`; existing
`swing.value_area.*` IDs and outputs remain byte-identical.

## 5. Testing (requirement prefix `swl_`)

- Pure fns: ATR correctness incl. first-bar TR; HVN/LVN threshold behavior +
  nearest-level selection; range compression gating; sweep detection both
  directions incl. volume-filter rejection; reclaim-window timing (confirms
  inside Z, does not confirm after); fail-closed degenerate inputs.
- Adapters: IDs embed parameters; warmup honored; sweep emissions occur ONLY
  on the confirming bar; companion-stop consistency.
- Strategy: long/short entries from events; stale-event refusal; hard
  invalidation on close-through; half-exit at T1 + breakeven; trail exit;
  time stop; ignores non-subscribed features (C1 defense-in-depth pattern);
  deterministic replays produce identical intents (MOD-10 posture).

## 6. EDGE / promotion alignment

Unchanged from the existing pipeline (spec 025 signal catalog + funnel G-gates
+ SIM-9 walk-forward): minimum 30 OOS trades before promotion review, positive
post-cost expectancy, DD within budget, no single trade dominating total PnL.
Hard gate: bounded defined max loss per trade at entry — satisfied here by the
close-evaluated invalidation level carried with every entry.

## 7. Deferred work (status updated 2026-08-22)

- ~~POC-flip state machine~~ — **IMPLEMENTED** (2026-08-22): `swing.poc_flip.{up|down}.{window}` adapters over a shared detector; acceptance rule operationalized as "trailing N closes contain ≥ N−1 on the far side of the CURRENT POC and the latest close is on the far side" (the draft's "N consecutive with ≤1 crossing back", made deterministic against the rolling POC). Optional strategy confluence wired: `require_poc_flip` config + grid key (0/1), freshness-gated (`poc_flip_max_age_ns`).
- ~~Accumulation/distribution classifier~~ — **IMPLEMENTED** (2026-08-22): `swing.ad.{accumulating|distributing}`; near-level volume = bars closing in the bottom/top tercile of the CURRENT value-area band, vr = mean(last K)/mean(prior K), cr = ATR(n) now vs K bars ago; fires iff vr>1 && cr<1, confidence = mean of clamped strengths. Historical positions judged against the current band (documented approximation — the band moves slowly vs K daily bars).
- Manual `regime_tag` plumbing — still deferred (ops/config-only input; strategies must define Unknown behavior).
- Any tick-derived volume-at-price path (would require revisiting the SWG-2
  bar-only contract explicitly; approx/tick families must never be blended).
