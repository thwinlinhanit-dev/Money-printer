# Owner Trading Policy (roadmap item 1.1)

**Owner:** thwin
**Effective:** 2026-08-25
**Status:** BINDING — confirmed by the owner in writing on 2026-08-25.
Values were drafted by the coding agent at the owner's explicit request and
are owner-owned from this point; any change goes through a §6 amendment.

Percentages are of **allocated capital**, whose absolute value the owner
records OFF-repo (PD-2 hygiene: no account values committed).

## 1. Objective

Produce validated evidence first; returns follow only from evidence.
No live order may exist unless every gate between here and Phase 5 of
`ROADMAP.md` has its written checkmark.

## 2. Capital policy

| Item | Decision |
|---|---|
| Total capital allocated to this system | Recorded off-repo by owner; referenced below only as **100% baseline** |
| Capital at risk during Phases 0–4 | **$0** (fixed by ROADMAP; not an owner variable) |
| Capital at risk at Phase 5 (live-small) | **Fixed $200 notional** — venue-minimum scale; one position per signal, revised only by amendment (expected to be owner-adjusted to reality at promotion time) |
| Maximum capital ever deployed simultaneously | **25% of baseline** — reachable only post-Phase-6 under the allocator, ≤ quarter-Kelly (ROADMAP cap) |
| Funding source rule | Profits compound only after **two consecutive profitable months**; each compounding step adds ≤ 25% of realized profits and requires a dated sign-off in §5 |

## 3. Benchmark definition

Feeds the `## Benchmark` section of `research/run_weekly_review.py` reports
(currently rendered as `unset (1.1 pending)`).

| Item | Decision |
|---|---|
| Primary benchmark | **Buy-and-hold BTC** over identical evaluation windows (dominant recorded asset; simple, unforgiving) |
| Comparison window | **Trailing 90 days** for weekly/monthly reviews; full strategy-lifetime for the annual report |
| Acceptance rule | Rolling 6-month expectancy **> 0 after all costs AND ≥ benchmark** (verbatim ROADMAP Phase-7 gate); a strategy beating neither is killed or redesigned per funnel rules |
| Costs included | always: fees + funding + estimated slippage per `research/feasibility.py` cost model |

## 4. Maximum-loss policy (hard stops)

These are one-way ratchets: tightening them is allowed anytime; loosening
requires a dated amendment signed below (PD-1 spirit: risk-off only).

| Item | Limit |
|---|---|
| Max drawdown from equity high-water mark that halts ALL new entries | **15%** |
| Max single-trade risk (% of deployed capital) | **1%** |
| Max daily loss that flattens and latches the kill switch | **3%** of deployed capital |
| Max consecutive losing trades before mandatory strategy autopsy | **8** |
| Cooldown after any halt before re-arm | **7 calendar days minimum**; re-arm is ALWAYS a human action (kill latch is one-way) |

## 5. Promotion & demotion protocol

- Demotion/de-risking: automatic, no permission needed.
- Promotion/re-risking: requires (a) the ROADMAP phase gate met with written
  evidence, (b) this policy's limits unchanged, (c) the owner's explicit,
  dated sign-off recorded here:

| Date | Phase promoted into | Evidence link | Owner initials |
|---|---|---|---|

## 6. Amendments

Append-only. Each amendment: date, what changed, why, owner initials.

| Date | Change | Reason |
|---|---|---|
| 2026-08-25 | Initial values drafted by coding agent at owner's explicit request; conservative defaults chosen where owner preference unknown ($200 live-small floor, 15%/1%/3% loss ladder, BTC benchmark) | Fill roadmap item 1.1; owner confirmation pending |
| 2026-08-25 | Owner confirmed all initial values; status PROPOSED → BINDING | Owner reply "ok confirm"; roadmap item 1.1 closed |
