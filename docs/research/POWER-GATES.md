# POWER GATES — Money Printer

**Status:** Living Document
**Last Updated:** 2026-09-11 (§6 capital pre-commitment written in, owner-accepted; orderflow-v1 perturbation/venue retest executed — see §4)
**Purpose:** Define what must be true before this system can be considered actually powerful for trading.

This is a strict gate checklist. Until the majority of these items are solidly green **and the load-bearing sections (§1.3 history, §4 edge, §7 maturity) are green**, Money-Printer remains a research prototype, not a powerful trading system.

Every checkbox below carries an evidence pointer. Nothing is green on vibes.

---

## Scorecard

| Section | Green | Honest state |
|---|---|---|
| 1. Data Foundation (mandatory) | 4/5 | machinery green; history depth is time-bound |
| 2. Research Integrity (mandatory) | 5/5 | **complete** — the last two days' work |
| 3. Statistical Honesty (mandatory) | 5/5 | **complete** — and empirically merciless |
| 4. Proven Edge (the real gate) | 1/5 | zero standing candidates — the last was killed by the 09-11 retest; correctly red |
| 5. Paper Trading Realism | 4/4 | paper ≡ research, hash-proven |
| 6. Risk & Safety | 4/4 | PD-1 intact; gates structural |
| 7. Operational Maturity | 1/3 | decay alerting now scheduled (kill panel); duration/decisions outstanding |
| **Total** | **24/31** | |

**Verdict: Serious Research Lab (upper band).** All buildable machinery is green. Every remaining red is either time-bound (history depth), evidence-bound (proven edge), or duration-bound (unattended operation) — no code sprint can buy them, only running the lab honestly can.

**Score history:** 2026-09-09 estimate pre-audit: 4–6/20 → post-audit: **15–16/20 equivalent**. The jump is the research-integrity and statistical-honesty machinery (spec 054 REL-1..32), not any claimed edge. No signal has been promoted. Zero. That is the correct outcome so far.

---

## 1. Data Foundation (Mandatory)

- [x] **Stable daily data pipeline (no multi-day scorecard blindness)** — 07:30 Windows gate verified scheduled and firing; 09-05 gate completed end-to-end after the whale.delta symbols_hash fix (fix verified in worktree, commit pending in the materialize workstream). One caveat: multi-day unblemished streak post-fix not yet long.
- [x] **Reliable VPS → PC drain + verified backups** — drain re-run cleared the 14 ssh_failed releases with sha256 verification against local masters; 00:07 DataBackup verified under re-pinned boundaries with the retry loop (missing=0, size_delta ~0%, plain robocopy exit); offhost 36h limit raise for the 53 GiB verify (commit `3402780`).
- [ ] **At least 6–12 months of clean, usable history on core symbols (BTC/ETH)** — ❌ **the biggest red, and time-bound.** Clean hyperliquid history ≈ 1 month (07-19 → 08-22 across two tapes). The 21.6 GB binance era cannot pass INT-4 and is parked behind an owner decision (`docs/OWNER-DECISION-binance-era.md`). Do NOT backfill with data that fails integrity gates — earliest green ≈ 2027-01 if collection holds. Nothing to do but keep collecting; this red is honest.
- [x] **Explicit data quality states working (no silent bad data)** — `DataQualityState = VALID | INVALID | MISSING | STALE | INSUFFICIENT_HISTORY | GAP` (spec 054 REL-27), golden dirty fixture frozen (REL-28, hash `17618162958652494096`), unknown never treated as neutral (R-1).
- [x] **Gaps are detected and never fabricated** — gap-heal semantics tested; open outcome windows are omitted, never imputed (verified live: the tiny-log footprint run honestly reported n=0 closed outcomes).

## 2. Research Integrity (Mandatory) — complete

- [x] **Every signal fire creates an immutable observation** — identity-stamped `SignalObservation` → append-only date-partitioned Parquet under the W-6 guard; recorder wired on backtest, paper, and paper-tail paths (REL-30). Caveat: the recorder runs in the sim/replay process; a collector-process recorder remains a follow-up.
- [x] **Forward outcomes (returns + MFE/MAE) calculated with no lookahead** — outcomes attach only after `close()`, from the recorded mark series; REL-14 measurement-shaped entry convention, no-lookahead regression tests; GRD-4's execution-shaped convention adjudicated for fill studies (spec 017 amendment).
- [x] **Signal identity (params + feature version + cost model) strictly enforced** — `SignalResearchIdentity` fingerprint (signal_id, feature_version, params_hash, data_schema_version, cost_model_hash); identical inputs → identical hash, tested across all four dimensions.
- [x] **Old evidence automatically invalidated when identity changes** — identity change → new fingerprint directory; the W-6 guard's refusal of the 2026-09-04 overwrite (symbols_hash drift, identical params_hash) is a live incident proving the invalidation boundary works.
- [x] **All evaluations use net (cost-adjusted) results, never gross only** — REL-16 net gate; empirically proven on the 4d control: **positive raw journal expectancy (+26.4/trade) still refused** because net ≤ 0. Gross-only grading would have passed coin flips.

## 3. Statistical Honesty (Mandatory) — complete

- [x] **Minimum sample size gates enforced** — REL-15 (30 closed outcomes minimum, tiered Insufficient/Preliminary/Research); exercised honestly at n=0 on both tapes for liq-fade and swing-range-reclaim.
- [x] **Promotion requires structured evidence and explicit reject reasons** — REL-29 machine-readable decision JSON with coded reasons (`NET_EXPECTANCY_NEGATIVE`, `DECAY_SUSPECT`, `INSUFFICIENT_SAMPLE`, …); every run prints per-horizon `GATE PASS` / `GATE REFUSED` with the codes inline.
- [x] **Simple sustained-decay detection exists** — REL-26/R-7 three-window decay gate; fired on 8/8 eligible rejections in kill-panel round 2 and on the control on both tapes.
- [x] **Performance reported by regime** — REL-25 regime buckets (Trend/Chop) from fire-time `regime.trend` snapshots; armed with real context and printed on every run. Binary taxonomy — acceptable per "at least Trend/Range"; multi-regime models explicitly out of scope.
- [x] **Most tested ideas get killed (high rejection rate)** — cross-tape kill panel 2026-09-09: 6 strategies × 2 tapes, **5/6 strategies rejected at essentially every horizon; zero promotions**. The one pass (orderflow 1h/4h, one tape) is flagged a retest mandate, not a promotion. The process is exactly as merciless as the philosophy demands.

## 4. Proven Edge (The Real Gate) — honestly red

- [ ] **At least 1–2 signals show positive net expectancy** — ❌ zero candidates. orderflow-v1 (the one tape-dependent candidate) was KILLED by the owner-approved retest 2026-09-11: net-negative on every binance day that trades, OOS-negative in every walk-forward window. See `docs/research/RETEST-orderflow-v1-2026-09-11.md`.
- [ ] **Edge survives out-of-sample / walk-forward periods** — ❌ conclusively no: the 09-11 purged/embargoed walk-forward found OOS expectancy NEGATIVE in every selectable window on both tapes, with the overfit signature (in-sample +12.04 → OOS −12.46). The 09-09 flip between PASS and REFUSED across adjacent tapes was the early warning; the grid confirmed it.
- [ ] **Edge not concentrated in only one market regime** — moot for the candidate: net-positive in BOTH regimes on the passing horizons (TREND +0.000228 / CHOP +0.000396 at 1h), but the edge itself failed replication, so this sub-check cannot be credited yet.
- [ ] **Edge remains positive after realistic costs and slippage** — on the swing tape yes, on the 4d tape no → ❌ until stable.
- [x] **Results are fully reproducible** — bit-identical reruns (4d control journaled identical twice: 136,277 trades), paper↔backtest decision-log hash equality (REL-30 proof), golden fixtures, journaled run configs (`data/runs/index.jsonl`).

**Path to green:** the orderflow-v1 retest (2026-09-11) executed this doc's prescribed study and the candidate died — 3/3 of its own falsification criteria met, zero positive binance days, OOS-negative everywhere. §4 now waits on the NEXT hypothesis to enter the funnel and survive it. The nightly kill panel (§7.3) grades whatever is registered next.

## 5. Paper Trading Realism — complete

- [x] **Paper trading uses the same signal logic and cost model as research** — proven byte-identical: decision-log hash `17053129571326659044` with recording ON vs OFF, equal to the backtest arm's; one resolver, one strategy set, one cost model.
- [x] **Paper results tracked with the same observation → outcome system** — REL-30: paper writes the same identity-stamped Parquet and prints the same tiered gate reports (`print_research_reports` shared by all three arms).
- [x] **Slippage and fees modeled honestly** — conservative flat round-trip cost (taker+maker) hashed into `cost_model_hash`; book-impact modeling is a known approximation, disclosed rather than hidden.
- [x] **Paper performance reviewed across multiple regimes** — regime buckets print on every paper run since regime context was armed.

## 6. Risk & Safety — complete (for the current stage)

- [x] **Live trading remains fully disabled** — PD-1 constant across every workstream, including all observation/evaluation changes. Nothing in this effort touched a live order path.
- [x] **Clear kill criteria exist** — signal-level: enforced, structured, machine-readable (the REJECT codes ARE kill criteria, and they fired 8+ times today). Capital-level: N/A while PD-1 holds; must be written before any live experiment is even proposed.
- [x] **Position sizing and maximum risk defined before any real capital** — sizing framework exists in the harness (`RiskUnits`, allocated equity); vacuously satisfied while no real capital exists. Must be re-opened as a hard gate before any capital decision.
- [x] **System cannot override safety gates** — the gates are structural, not advisory: identity + W-6 append-only cannot be bypassed from the CLI (the 09-04 refusal incident proved a real overwrite attempt fails), sample-size floors have no bypass flag, and the net gate cannot be asked to grade gross.

**Pre-committed capital trigger (2026-09-11, owner-accepted):** §6 reopens for capital decisions only when **§4 = 5/5 AND 6+ months of clean paper history** exist. Until both hold, PD-1 stands and no live experiment — not even a proposal — is in scope. Written while edge = zero and capital at stake = zero, the cheapest moment to be strict; future-you does not get to renegotiate this at 2am.

## 7. Operational Maturity — the honest reds

- [ ] **Can run for weeks with minimal manual intervention** — ❌ not yet demonstrated. The scheduled tasks (07:30 gate, 00:07/03:35 backups, retention watchdog) are verified firing with audit-trail logging, but the recent history includes restarts, registration races, and manual fixes — all hardened, none yet aged. A multi-week unattended stretch with zero manual patching is the test.
- [ ] **Storage stays within free-tier limits without constant firefighting** — ❌ the firefighting *tooling* is green (retention checks with Telegram P2/P1, staging-exclusion watchdogs, prune switch, 40 GB budget alerts), but the binance-era owner decision is open and C: pressure has been episodic (19.1 GB free incident).
- [x] **Clear monitoring and alerting for data health and signal decay** — data-health alerting was already solid; signal-decay monitoring is now SCHEDULED: `MoneyPrinterKillPanel` runs nightly at 09:15 UTC (registered 2026-09-11) — 5 strategies + the coinflip-any control through the observation engine on the freshest closed HL day, Telegram P2 on any strategy GATE PASS or DECAY_SUSPECT, P1 if the noise control passes (R-8). Smoke-tested live on day 2026-09-08 (control refused 4/4, two P2s delivered). Caveat: one manual night so far; the §7.1 multi-week proof is still the calendar's job.

---

## Scoring Guide (re-normalized for 31 items)

The original 0–19 banding assumed a ~20-item checklist. With 31 items, raw counts alone would award "Actually Powerful" to a system with one month of history and zero proven edge — absurd. Bands now carry riders:

| Green items | Riders | Verdict |
|---|---|---|
| < 50% | — | Early Prototype |
| 50–65% | — | Serious Research Lab |
| 65–80% | **AND** §4 ≥ 3/5 **AND** §1.3 green | Potentially Useful System |
| > 80% | **AND** §4 = 5/5 **AND** §7 all green **AND** 6+ months of clean paper history | Actually Powerful (rare) |

**Current: 77% green, but §4 = 1/5 (zero candidates) and §7 = 1/3 (duration outstanding) → Serious Research Lab, upper band.** The riders exist so that no amount of machinery can substitute for the two things that actually make a trading system powerful: time-tested edge and unattended reliability.

---

## What changes the verdict (in order of leverage)

1. **Schedule the nightly kill panel** (§7.3 → feeds §7.1): evaluate_full on the freshest tape every night, alert on PASS or new DECAY. Capability exists today; only scheduling is missing.
2. ~~orderflow-v1 perturbation + venue-generalization study~~ **DONE 2026-09-11 — the candidate was killed** (report: `docs/research/RETEST-orderflow-v1-2026-09-11.md`). Next: a new hypothesis through the funnel; the process is proven to kill what deserves killing.
3. **Close the binance-era owner decision** (§7.2): accept / migrate / raise-cap, then let the retention automation hold the line.
4. **Keep collecting** (§1.3): the only path to 6–12 months is through the calendar. Do not manufacture history.
5. **Commit the outstanding workstreams** (materialize fix, ops hardening) so §1.1's caveat ("fix verified but uncommitted") clears.

## Next Review

Trigger-based, not calendar-based: whichever comes first of
(a) nightly kill panel running for 14 consecutive nights (scheduled 2026-09-11; streak counter in `data/killpanel/streak.json`),
(b) ~~the orderflow-v1 perturbation study concludes~~ **CONCLUDED 2026-09-11 — candidate killed** (`docs/research/RETEST-orderflow-v1-2026-09-11.md`),
(c) +30 days (2026-10-09).

---

> Powerful trading systems are not defined by how many features or signals they have.
> They are defined by how many bad ideas they successfully kill, and whether the surviving ideas still work after costs, time, and regime changes.

*Audit basis: commits `c279c34`, `01cc22e`; kill-panel runs `eval-20260909-*` (both tapes); spec 054 REL-1..32; spec 017 amendment; verified backup/drain/gate incidents from 2026-09-04 → 09-09. Full technical detail: `docs/research/RESEARCH-LAB-STATUS.md`.*
