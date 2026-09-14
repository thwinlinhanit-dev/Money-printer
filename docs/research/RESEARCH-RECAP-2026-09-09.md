# Research recap — 2026-09-09

**Scope:** the cross-tape kill board, the observation-pipeline wiring that made
it possible (REL-30/31, plus the REL-33/34 fixes the wiring forced), and the
recommended next experiment: the **bybit hot days**.

Companion living documents: `RESEARCH-LAB-STATUS.md` (capability matrix),
`POWER-GATES.md` (23/31 green). This doc is the narrative between them —
what was run, what died, what survived, and what to attack next.

---

## 1. The kill board

**Method.** Five catalog strategies + a venue-generic noise control
(`coinflip-any`, REL-32), graded by `evaluate_full` at the engine's four
horizons (15m/1h/4h/1d), seed 1, `params-hash rel32-swing-baseline`, every
run journaled (`data/runs/index.jsonl`) and every observation written as
identity-stamped Parquet. Two real tapes:

- **4d tape** — `merged_hyperliquid_BTC_4d.log`, Aug 18–22 (the round-2 tape).
- **swing tape** — `swing_hyperliquid_BTC.log`, Jul 19 → Aug 18, ~31.6 days,
  789 MB (the daily-paper rehearsal window).

Panel: 6 sequential runs, exit 0 across the board, ~37 min wall time.

### The board

| Strategy | 4d tape | swing tape | Replicates? |
|---|---|---|---|
| coinflip-any (control) | REJECT ×4 — NET_NEG + DECAY, **despite a positive raw journal (+26.4/trade)** | REJECT ×4, n≈23k obs, win ≈0.49–0.50 | ✅ YES |
| carry-v1 | REJECT ×4 (NET_NEG + DECAY) | REJECT ×4, n=19.2k; 1d gross turns positive (+0.000194) but net −0.000556 | ✅ YES |
| orderflow-v1 | REJECT ×4 (NET_NEG + DECAY) | REJECT at 15m/1d — **GATE PASS at 1h (+0.000281) and 4h (+0.000566)**, positive in both regimes | ❌ NO — the pass is tape-dependent |
| liq-fade-v1 | n=0 → `INSUFFICIENT_SAMPLE` | n=0 → `INSUFFICIENT_SAMPLE` | ✅ honest refusal |
| swing-range-reclaim-v1 | n=0 → `INSUFFICIENT_SAMPLE` | n=0 → `INSUFFICIENT_SAMPLE` | ✅ honest refusal |

**5/6 rejected, zero promotions.** A research process that rarely kills is
broken (R-8); this one kills at exactly the advertised rate.

### The three findings that matter

1. **The noise control earned its keep twice.** On the 4d tape,
   `coinflip-any`'s raw run journal showed **+26.4 expectancy per trade —
   positive**. Gross-only grading, or anyone eyeballing the journal, would
   have promoted coin flips. The gate chain killed it anyway: costs flip net
   negative at every horizon, and decay is confirmed across windows (R-4,
   R-7). The answer to "what if noise looks profitable?" is now empirical.
2. **The one non-replication is the interesting one.** orderflow-v1 passes
   1h/4h on the swing window — net-positive in TREND *and* CHOP — then the
   same identity refuses with `DECAY_SUSPECT` on the adjacent 4-day tape.
   That flip is not a bug; it is R-7 doing its job. Two tapes disagree ⇒ the
   honest state is **unproven, unstable across windows — a retest mandate**,
   not a promotion candidate. (Round-2 context: its win-rate edge was already
   TREND-concentrated, 0.556 vs 0.545.)
3. **Honest refusals at n=0 are a feature.** liq-fade and swing-range-reclaim
   never fire on hyperliquid tapes; both boards refuse them at n=0 with no
   fabrication (R-1). liq-fade in particular has *never been graded on real
   data* — see §3: bybit may be its natural habitat.

**Determinism held everywhere it could be checked:** the 4d control rerun
journaled bit-identical to its first pass (136,277 trades, same `log_hash`);
carry-v1 on the swing tape reproduced the earlier REL-30 paper run
bit-for-bit (12,006 trades, expectancy −8.0816) — backtest arm vs paper arm,
same tape, two code paths, one answer. Both control tapes share one identity
(`8e415159…`): the fingerprint is tape-independent by design, date partitions
separate the tapes, W-6 accepted both writes.

**Takeaway: gross metrics lie, single windows lie harder, and the gates
caught both lies in one afternoon.**

---

## 2. The wiring that made it (REL-30/31)

The kill board is only as honest as the observation pipeline underneath it.

**REL-30 — paper-mode observation recording.** The paper path (`sim paper`,
`sim paper-tail`) exposes the SAME write-only `ObservationRecorder` the
backtest path uses; recording is enabled before the feed and never perturbs
the decision path (the batched paper stream hash with recording on equals the
hash with it off — G3/SIM-15). Outcomes attach only after `close()` from the
recorded mark series — no lookahead by construction (REL-14). The tail path
is proven **incremental** on a growing log (obs 0→9→139→146 mid-stream) with
a kept negative control: a non-growing log idles out at zero observations
and refuses honestly — a tail that sees no data fabricates nothing (R-1).

**REL-31 — hit-journal migration.** The legacy JSONL gross-only backfill
(spec 017 GRD-2/GRD-4) is retired as a research artifact: hits bridge onto
the identity-stamped observation flow (venue refusal per R-1 when the tape's
venue doesn't match the identity; `regime.trend` passthrough into fire
snapshots). The `footprint` study binary relocated `features → storage/src`
(Parquet writes live in mp-storage; no dependency cycle), and spec 017's
execution-shaped entry convention (GRD-4) was adjudicated against REL-14's
measurement-shaped window in spec 054's decision log.

**What the wiring forced (and got fixed):**

- **REL-33 — W-6 idempotency across the Parquet roundtrip.** The PAP-12 smoke
  found that identical observation re-runs hard-faulted: snapshot f64s ride a
  JSON text column, and the f64→text→f64 cycle drifts 1 ULP, so read-back
  content hashes never matched in-memory ones. Fixed by storage-canonical
  hashing + a content-based sibling scan (divergent content on the same
  identity+date now faults loudly instead of landing a duplicate file).
- **REL-34 — one store, two consumers.** The Python grading arm
  (`research/observation_store.py`) reads the same Parquet with the identity
  fingerprint re-verified **cross-language** (Python FNV-1a re-derives every
  on-disk directory name — verified against all 10 real identities) and
  net-primary grading (R-4). On the tail-live2 identity it independently
  reproduced the Rust verdict (n=63, win 0.508, net −0.000771).
- **PAP-11/12 — the schedule grows the corpus.** Every nightly paper
  rehearsal records the primary leg under `pap1-primary` and runs the
  `coinflip-any` baseline under `pap11-noise-baseline`; control GATE PASS at
  any horizon ⇒ evaluation pipeline broken ⇒ P1 + session fault (exit 1).

Paper ≡ research is proven, not asserted: byte-identical decision-log hash
across arms, and carry-v1's bit-for-bit cross-arm reproduction above.

---

## 3. Recommended next experiment: the bybit hot days

### Why bybit, why now

The kill board has run on exactly one venue. orderflow-v1's swing pass is a
sighting; the fastest way to know if it (or anything) is real is a **third
tape on a different venue** — out-of-sample in time *and* cross-venue. That
is precisely what POWER-GATES §4 demands and what nothing has tested yet.

The bybit corpus is the biggest untouched surface in the repo:

| Property | Value |
|---|---|
| Total | **12 GB across 50 day-files** (BTC/ETH/SOL) |
| Coverage | 07-19, 08-22, then continuous **08-25 → 09-08** |
| Gap | 08-23/24 missing (the ssh_failed drain era — known, not fabricated) |
| Hot days (by activity) | **08-22** (ETH 737 MB + BTC 536 MB + SOL 523 MB — the biggest day), **08-25** (BTC 449 MB), **08-28** (BTC 427 MB), **09-03** (BTC 417 MB) |

Frame census on `20260825_bybit_BTCUSDT.log` (ground truth, not guesswork):
**2.94M trade, 1,873 liquidation, 569 funding, 0 depth.**

### What each strategy should do there (pre-registered expectations)

| Strategy | Bybit expectation | Why |
|---|---|---|
| coinflip-any | fires, must be REFUSED | trades ⇒ `cvd.bybit`; the control certifies the pipeline (R-8) |
| carry-v1 | fires on real funding events for the first time | 569 funding frames/day ⇒ `funding.*`; its two refusals were on hyperliquid tapes |
| **liq-fade-v1** | **its natural habitat — first real grading ever** | 1,873 liquidation frames/day ⇒ `liq.vol_*`; starved at n=0 on BOTH hyperliquid tapes |
| orderflow-v1 | may starve honestly at n=0 (no depth frames) | subscribes `book.depth.*`/`tape.bps_delta`; zero depth on bybit — an honest refusal would itself be informative |
| swing-range-reclaim | unknown; bar/feature dependent | never graded anywhere yet |

If liq-fade finally fires and gets refused, that is the process working. If
it fires and passes a horizon on panic-heavy 08-22, that is the first live
hypothesis the lab has ever carried to a real test.

### Step 0 — plumbing (honest about it)

The sim engine registers `Cvd::new(Venue::Hyperliquid)` explicitly; features
are venue-parametric (`cvd.{venue_slug}`, `funding.{venue_slug}`), so a
bybit run needs a small engine-side registration change (or a `--venue` flag)
so `cvd.bybit` exists on the tape. `FundingRate` is already venue-less
(`funding.rate`). Verify `liq.vol_*` / `tape.bps_delta` name wiring for bybit
frames before the panel — a silent starvation would masquerade as an honest
refusal, which is exactly the failure mode the pre-registered expectations
table exists to catch (compare expected-vs-actual fire counts per strategy).

### Protocol

1. **Preserve first.** 08-22 is the biggest hot day AND the oldest big bybit
   day — it (and the small 07-19 pair) predates the retention checker's
   08-25..09-06 watch window, so nothing currently protects it from pruning.
   Add it to the watch or copy it aside **before** any corpus cleanup.
2. Merge the four hot days (08-22, 08-25, 08-28, 09-03) into
   `bybit_hotdays.log`; record the merge recipe in the run journal.
3. Fix the step-0 venue registration; smoke-test that `coinflip-any` fires
   on a 20 MB slice (fire count > 0 proves the tape feeds the engine).
4. Run the kill panel: 5 strategies + control, seed 1, fresh params-hash
   (`bybit-hotdays-baseline`), all four horizons, journaled
   `eval-<date>-bybit-hotdays`.
5. Cost model: use **bybit-realistic fee params** ⇒ a new `cost_model_hash`
   ⇒ new identities. Cross-tape comparisons stay qualitative (per R-3,
   identities are never merged); if a bybit run must be compared
   quantitatively to the hyperliquid identity, re-run the hyperliquid tape
   under the same cost model instead of mixing identities.
6. Grade with `evaluate_full` (authoritative) and the Python arm (parity
   check, REL-34) — both verdicts journaled.

### Success criteria (decided before running)

- **Control refused everywhere** — else the evaluation pipeline is broken;
  stop and fix, do not interpret anything else.
- **carry-v1 refusal replicates** on a third tape with real funding ⇒ the
  kill generalizes across venues.
- **liq-fade graded on a real sample for the first time** — any verdict is a
  win over n=0 forever.
- **orderflow's 1h/4h swing pass either replicates on bybit** (first genuine
  §4 edge candidate: survives window AND venue change) **or fails** (the
  sighting was window-luck; stays dead per R-7). Both outcomes are progress;
  only ambiguity is not.

### What this buys on the POWER-GATES board

- Control + replicating refusals ⇒ §3 stays fully green with cross-venue
  evidence.
- liq-fade/orderflow verdicts on bybit ⇒ §4's "edge survives
  out-of-sample/walk-forward" gets its first real test — the difference
  between "Serious Research Lab" and "Potentially Useful System" is exactly
  this experiment's outcome.
- The 12 GB bybit corpus stops being dead weight the storage budget fights
  and starts being the most informative tape in the repo.

---

## 4. Summary

The lab now has a closed loop: fires → immutable identity-stamped
observations (R-2/R-3) → net outcomes with no lookahead (R-4) → gates that
kill noise even when it looks profitable (R-5/R-7) → paper and Python arms
reading the same artifacts (REL-30/33/34). The kill board killed 5/6 and
handed the one survivor a retest mandate instead of a promotion. The next
question is already pointed at real data: **do any of these verdicts survive
a different venue's hot days?** Run the panel before 08-22 gets pruned.
