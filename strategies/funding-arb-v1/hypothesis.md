# funding-arb-v1 — Hypothesis

## Edge: what inefficiency, who pays us and why do they accept the loss?
The same underlying funds at materially different annualized rates on
different venues. Perp funding is set by each venue's own mechanism — its
index vs its mark, its cadence, its caps, its local order flow — so the
crowd that pays funding on one venue is NOT the same crowd on another: a
leveraged long on hyperliquid paid its +1095 bps/yr cap while the same
BTCUSDT long on bybit swung −1.2%..+0.75%/yr (day mean ≈ +0.15%/yr, 08-14,
recorded). The payer accepts the loss knowingly
— funding is their cost of leverage conviction, priced locally; they are not
arbitraging the cross-venue differential any more than the guy at one
exchange cares about the other exchange's book. We collect the differential
by going SHORT the high-funding venue and LONG the low-funding one — a
perp-perp hedge, net-flat, carrying funding-differential risk instead of
price risk. Counterparty: the leveraged directional crowd on the
over-funded venue. This is carry-v1's sibling: same payer, one venue's
absolute funding extreme (carry-v1) vs two venues' funding DIFFERENTIAL
(this). Capacity is small at the extremes, which is why size skips it and a
fund-of-one can eat.

## Signal decomposition (features)
- `funding.spread_{a}_{b}` — annualized cross-venue funding spread, bps/yr,
  on the SAME underlying (BTC vs BTCUSDT, ETH vs ETHUSDT). Annualization is
  cadence-aware PER VENUE (units never mixed, spec 003): hyperliquid funds
  hourly (×8760), bybit/binance fund every 8h (×1095). The spread is
  `funding_a_annualized − funding_b_annualized` at each hour BOTH venues
  emit a funding row.

Entry:
- `|funding.spread_{a}_{b}|` ≥ `entry_thresh_bps_yr` (event-study
  thresholds: 500 and 1000 bps/yr). The spread must be material enough to
  clear the two-venue cost leg (fees + two perp spreads) — see the cost
  gate below. Direction: short the positive-funding venue, long the
  negative-funding one (net-flat, hedge-first: enter BOTH legs as one
  intent pair so a gap between legs cannot leave us naked).

Exit:
- `|funding.spread_{a}_{b}|` < `exit_thresh_bps_yr` — the differential
  normalized (the carry window closed), OR
- hard time stop (default 14 days, matching carry-v1's `max_hold_ns`), OR
- the spread WIDENS past a kill level (divergence risk — see Risks).

Persistence is the whole edge: the event study measures whether the spread
mean-reverts (windows close fast — the carry is NOT harvestable) or PERSISTS
(harvestable carry). The 08-14 evidence (corrected 2026-08-17 — the batch-2
read claimed the spread "persisted at ~1100 bps/yr for all 17 hours"; it did
not): the spread peaked +2278 bps/yr at 08:00 and decayed monotonically to
+341 bps/yr by 23:00 (day mean +1080); CAR[+12h] = −1278 bps/yr (CI95
[−1715, −693], excludes 0) is the spread COLLAPSING as bybit catches up, not
"spike decay above a plateau". On 08-15 the level did not persist either
(range −486..+1005, closed +295). The DIRECTION is consistent (HL ≥ bybit on
BTC both days); the LEVEL converges within the day. A harvestable carry must
therefore be captured in the early-window hours, before the lagging venue
catches up — and the honest cost bar is the spread available at the exit,
not the peak.

## Regime dependency: declared_regime + why
`regime.vol ∈ {Mid, High}` × any trend state — same shape as carry-v1.
Funding differentials that clear the ~1000 bps/yr cost bar concentrate in
volatile, crowded windows (one-sided positioning pays). Dead-calm regimes
have thin, sub-threshold spreads. Not directional — profits in chop are the
point (portfolio role: pays when trend-breadth bleeds).

## Falsification (written BEFORE any backtest)
Kill if, over the recorded two-venue history with full costs (entry+exit
taker fees on BOTH legs, funding actually accrued from BOTH venues' Funding
events, perp spreads on both legs):
- expectancy ≤ 0 in the 2×-cost column at G1, OR
- the edge concentrates in < 3 distinct calendar windows (not a harvest, a
  fluke), OR
- walk-forward OOS flips sign vs in-sample in ≥ 2 of 3 windows (curve fit,
  not an edge).
The strategy must also prove determinism: two identical-seed runs over the
same logs must produce byte-identical decision logs (spec 018 discipline),
same as every funnel strategy.

## Two-venue data requirement (spec, written BEFORE implementation)

The event-study gate (RES-4 batch 2, 2026-08-16) confirmed the PRECONDITION
(venues do fund the same underlying at materially different annualized
rates, persistently) but the corpus could not grade a verdict (n=4 complete
windows on a single day). This spec is the exact data that unlocks the next
gate. Requirements numbered **FARB-n** (this strategy's prefix, per spec
conventions).

- **FARB-1** Two venues MUST each record a `Funding` event stream on the
  SAME underlying (perp vs perp). Venues in scope today: hyperliquid
  (hourly funding), bybit and binance (8h funding). No cross-asset pairings
  (BTC vs ETH is NOT a funding-arb leg — the edge is same-underlying
  differential only).
- **FARB-2** The two legs MUST overlap for ≥ 24 contiguous hours on ≥ 3
  DISTINCT calendar days. Rationale: the +24h event-study horizon needs a
  25-bar contiguous window; the single 08-14 bybit day (17h overlap) could
  only fill +12h (n=4 windows) and the ≥2-of-3-windows falsification rule
  needs multi-day windows. 3 days is the minimum the corpus must be able to
  distinguish harvest from fluke (same rule shape as liq-fade-v1's C2).
- **FARB-3** Funding rates MUST be annualized per venue cadence before any
  comparison (hyperliquid ×8760, bybit/binance ×1095) — units never mixed.
  A raw 8h rate compared against a raw hourly rate is a unit bug, not a
  spread.
- **FARB-4** The spread series MUST be computed at hourly granularity from
  BOTH venues' funding rows for the overlap hours, and the same-underlying
  pair must be resolvable by the existing symbol resolver
  (`core_symbols.txt`, e.g. hyperliquid BTC ↔ bybit BTCUSDT).
- **FARB-5** The cost leg MUST be quantified before the strategy is a
  candidate: two venues' taker fees + both perp spreads must clear the spread
  available at ENTRY AND EXIT — and with the spread mean-reverting within
  the day (08-14: +2278 peak → +341 by day end), the honest bar is the
  post-entry level, not the peak. If the cost leg is ≥ the spread, the edge
  is dead regardless of persistence — this is checked BEFORE any backtest,
  as part of the gate, not after.
- **FARB-6** Until FARB-2 is met, the strategy MUST be recorded as
  NOT-GRADABLE (data too thin), never as a silent pass and never with
  synthetic data invented to fake a verdict (PD-6/INT-1: the audit judges
  provenance). The re-gate is the existing harness:
  `research/run_backlog_event_studies.py` (study D, funding-arb) — one
  command, no new machinery.

**What unlocks FARB-2 today** (the corpus-side path, already in flight):
the bybit ETHUSDT/SOLUSDT deploy (spec 032 interim path,
`deploy_bybit_multi.sh`) plus the 08-15.. bybit days landing in the nightly
drain. Each drained bybit day adds ~24h of same-underlying overlap with
hyperliquid; binance's July/August days add BTCUSDT/ETHUSDT legs where
overlap exists. The re-gate fires automatically when ≥ 3 overlap days exist
in `data/raw` — no code change, the harness is pair-driven.

## Expected characteristics
Horizon: hours–days (entry at |spread| ≥ threshold, exit on normalization
or the 14-day time stop; funding accrues at each venue's cadence). Trade
rate: low, episodic — a few per week once multi-day overlap exists. Hit-rate
shape: high hit rate on the carry collection, small wins; the loser is
divergence — the spread WIDENS against the position (see Risks), bounded by
the kill level and time stop. Costs dominate: the two-venue fee + spread
leg is the first gate, not an afterthought.

## Risks: what breaks it
- **Convergence, not divergence**: the 08-14 spread collapsed within the day
  (bybit caught up from −1183 to +754 bps/yr; +2278 → +341) — the risk is
  that the carry window is too short to harvest net of the two-leg cost, not
  that the spread widens. That said, a spread that widens against us IS the
  adverse excursion — both perp books can move against the position even
  though we are delta-flat (cross-venue basis risk; the perp-perp basis
  itself widened from ~0 to the spread in the 08-08 binance leg: mean spread
  −113 bps/yr on a day the event study flagged).
- Funding regime change by venue (formula/cadence/cap changes break the
  annualization assumption and can flip the spread sign overnight).
- Execution gap between legs (enter hedge-first, both legs one intent
  pair); venue solvency on exactly the events that pay us (the
  over-funded venue is where the crowd is leveraged).
- Crowding: other carry harvesters compressing the differential once the
  data exists for them to see it — the ~1100 bps/yr is an information
  asymmetry (recording + compute-on-read), and it decays as the corpus
  grows.

## Honest scope for v1
Two venues, same underlying, market intents only, no position scaling, no
venue-arbitrage execution machinery (inventory/transfer management is the
[v2] cross-venue divergence arb item and is OUT of v1 — v1 hedges both legs
on the two venues it has data for, no transfers). The `funding.spread`
feature is new (spec 003 §Analytics, carry study 2026-08-13) and already
computed by `mp-query carry` + the event-study harness; this hypothesis is
its first strategy consumer in the funnel.

## Edge results (first real data, 2026-08-16 event study)
The RES-4 batch-2 gate (record `backlog-event-studies-2026-08-16`,
`runs/index.jsonl`) — the evidence this hypothesis is written on:

| Overlap (venue pair, day) | hours | mean spread bps/yr | mean \|spread\| bps/yr |
|---|---|---|---|
| HL BTC vs binance BTCUSDT 07-19 | 1 | +958 | 958 |
| HL BTC vs binance BTCUSDT 08-08 | 15 | −113 | 370 |
| HL ETH vs binance ETHUSDT 08-08 | 1 | +322 | 322 |
| HL BTC vs bybit BTCUSDT 08-14 | 17 | **+1080** | **1080** |
| HL BTC vs bybit BTCUSDT 08-15 | 21 | +440 | **512** |

The 08-14 bybit day is the one real multi-hour overlap: hyperliquid BTC
funded at its +1095 bps/yr cap all 17 overlap hours vs bybit BTCUSDT moving
−1183 → +754 bps/yr — spread peak +2278 (08:00), end-of-day +341, day mean
+1080. Event study (|spread| ≥ 500/1000 bps/yr, CAR of Δ|spread| to +12h;
+24h honestly reports n=0 — the 17h overlap cannot fill a 25-bar window):
n=4 complete windows on that single day, CAR[+12h] −1278, CI95
[−1715, −693], mean |spread| over +12h ≈ 1103 bps/yr (a conditioned mean
over the early-day complete-window events only).

Read (corrected 2026-08-17): the negative CAR is the spread COLLAPSING —
bybit funding caught up to HL's cap within ~10 hours. The level did not
persist on 08-14 or 08-15; the DIRECTION (HL ≥ bybit on BTC) did. A carry
harvest must capture the early-window spread before it converges, and the
cost leg must clear the post-entry level.

RE-GATE (2026-08-16, record `backlog-event-studies-2026-08-16-r2`): the
08-15 bybit day drained — second distinct overlap day, same direction (HL
≥ bybit, HL pinned at its +1095 bps/yr cap 15 of 21 hours while bybit
lagged 90–1086), mean |spread| **512 bps/yr**, 12 hours ≥ the 500 bps/yr
entry threshold, but the level did not persist (range −486..+1005, closed
+295). n=0 complete windows on 08-15 (HL funding gap 11:00–13:00 UTC breaks
every event window). Verdict: direction RECONFIRMED on 2 days, persistence
NOT confirmed, still NOT GRADABLE — FARB-2 (≥ 3 distinct overlap days)
unmet; the gate re-opens at FARB-2 via the bybit ETHUSDT/SOLUSDT deploy +
further 08-16.. drain days.

RE-GATE (2026-08-18, record `backlog-event-studies-2026-08-18-r2`): the
08-16 bybit days drained (BTCUSDT/ETHUSDT/SOLUSDT, 2026-08-17) and were
added to the pair list. **FARB-2 is now MET**: three distinct bybit overlap
days on BTC (08-14: 17h, mean +1080; 08-15: 21h, mean +440; 08-16: 11h,
mean +927 — all positive, direction consistent across all three). First
bybit ETH day: 08-16, mean +146 (far smaller). BUT the CAR evidence is
still single-day: complete-window events exist only on 08-14 (n=4,
n_days=1, CAR[+12h] −1277.6, CI unreliable); 08-15 and 08-16 produced
n=0 complete windows (08-15: HL funding gap; 08-16: only 11 overlap hours
< the 12h window). Persistence remains UNCONFIRMED and leans negative —
every day so far shows intra-day convergence of the spread, not a plateau.
Verdict: NOT GRADABLE, with the blocker now event breadth (one
complete-window day), not corpus breadth. The next corpus milestone that
can change this: full multi-day bybit coverage of the same symbol
(08-17 bybit days drain tonight; the 08-16..08-17 host recording gap means
the 08-17 HL side is partial and will collide — the local copy wins per
drain policy).

BACKTEST VERDICT (2026-09-13, record `farb2-backtest-2026-09-13`,
`runs/index.jsonl`): FARB-2 cleared 2026-09-12 (14 same-day bybit<->HL pairs
per symbol) and the pre-registered full-cost backtest killed the strategy on
both registered configs. 58 episodes over 15 overlap day-pairs: expectancy
−28.13 bps (500entry/250exit) / −28.16 bps (1000entry/500exit) at BASE
costs, −57.1/−57.2 at the 2× kill column; win rate 0/58; carry collected
0.1–4.3 bps (max 4.265) against the 29 bps RT two-leg cost. Mechanism: the
spread normalizes within hours (median hold 1–6h), so annualized bps/yr
never convert to collectible carry — FARB-5's cost bar ("the honest bar is
the post-entry level, not the peak") is the binding kill, exactly as
written. Edge breadth was NOT the problem: 6–8 distinct calendar windows,
WF OOS sign-flips 0/3 (consistently negative, not curve-fit). Registry row
→ killed. Report: `docs/research/BACKTEST-funding-arb-v1-2026-09-13.md`;
deterministic artifact `research/funding_arb_backtest_farb2-backtest-2026-09-13.json`.
