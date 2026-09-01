# Completion Master Plan — what “done” means for this app

**Date:** 2026-08-31
**Status:** BINDING for agents until the owner amends it
**Supersedes:** “implement all remaining specs” as the work queue
**Does not supersede:** Prime Directives, `ROADMAP.md` capital gates, `docs/OWNER_POLICY.md`

This file is the execution plan to **finish a usable product**. It is not a
promise of trading returns. The 2026-08-31 audit is the evidence baseline:
the lab is strong engineering with a dark daily gate, $0 at risk, and zero
promoted strategies.

---

## 1. Product definition (v1 Lab)

v1 is a **trustworthy research-and-rehearsal machine** that a single owner
uses every day. It is complete when all six exit criteria below are true
**in writing**, with evidence links.

| ID | Exit criterion | Evidence |
|---|---|---|
| **D1 Continuity** | Every UTC day produces a scorecard or a loud P1 that names *why* (missing drain, all-zero, guardrail, DST skip). No silent blind weeks. | `data/scorecards/` + `pipeline.log` for 7 consecutive calendar days with no unexplained hole |
| **D2 Corpus home** | Recorded data lives off `Downloads`, under the 40 GB cap or with a raised budget + prune that refuses uncompacted tails, backups succeeding | path in config (not repo), last backup manifest |
| **D3 Phase-0 honesty** | Either `PROMOTED` (7 consecutive clean **and** burst-free window) **or** a dated owner waiver recorded in OWNER_POLICY §5 that names the remaining defect | `mp-ops promote` JSON |
| **D4 Paper rehearsal** | One strategy runs paper (live feed, sim fills) for 14 calendar days; faults = 0; paper vs sim identity within G3 | spec 051 artifacts |
| **D5 Operator surface** | One screen shows: mode, last scorecard, streak, paper PnL, kill-latch, pipeline stale. No WASM required. | spec 052 |
| **D6 Alpha honesty** | Registry has exactly one *active* candidate (or none). Every killed idea has an autopsy. Pine numbers are not cited as lab evidence. | spec 053 + `research/registry.jsonl` |

**Live trading is not in v1.** PD-1 and OWNER_POLICY §2 keep capital at $0
until Phase 5 of `ROADMAP.md` is checked. Completing v1 does **not** unlock
live. Phase 5 is a separate owner decision after D1–D6.

---

## 2. What we will not finish in v1 (cut list)

These specs stay in the index. Agents must not treat them as blockers for D1–D6.

| Spec | Why it waits |
|---|---|
| 011 WASM terminal | Fun, not compounding. History protocol can wait. |
| 012 zero-copy pipeline | Premature; ring buffer is already sound post-08-28. |
| 016 remaining MAT polish | Materialize already runs; hash-collision overwrite is a 050 bug, not a new engine. |
| 019 systemd completeness | VPS units exist; Windows/VPS split is the real bug (050). |
| 020 Binance REST snapshot | Geo-blocked from this network; not Phase-0 required. |
| 023 string interning | Perf, not product. |
| 032 multi-symbol fan-out | Nice; HL BTC+ETH is enough for D3. |
| 007 live venue adapter | PD-1. Paper path only (051). |
| 037–040 options stack as product | Recorders/features exist; they are not the bottleneck. Do not build a vol desk UI. |
| More collectors (listings, quarterlies) | Only if 053’s active candidate is blocked on that data. |

Draft specs 012–023 that already have code get **verification slices** only
when they are on the critical path of D1–D6. CONV-21 tests for unused drafts
are not a reason to stall the gate.

---

## 3. Sequence (do in this order)

```
P0  Repair the lab          specs 050          ← NOW; blocks everything
P1  One honest corpus week  spec 024 (as-is)   ← after P0
P2  Daily paper rehearsal   spec 051           ← can start on unclean days
P3  Operator console        spec 052 / 041-A   ← after P0 so the screen is true
P4  Alpha program           spec 053           ← parallel with P2, not before P0
P5  Owner-only live-small   ROADMAP Phase 5    ← forbidden until D1–D6 + owner sign-off
```

**P2 may overlap P1** (paper does not require PROMOTED). **P4 must not add
strategy crates until P0 is green** — more code while the gate is blind
repeats 08-13 / 08-22 / 08-25.

Calendar guess (not a promise): P0 is 2–5 owner-attended days (SSH, disk
move). P1 is 7+ clean burst-free UTC days of wall time. P2 is 14 days of
wall time. P3 is a few engineering days. P4 is ongoing.

---

## 4. Work packages

### P0 — Repair the lab (spec 050)

1. Restore missing scorecards 2026-08-25..present from VPS raw **or** mark
   those days `recording_missing` (never invent CLEAN).
2. Fix VPS drain `ssh_failed` (14 held files as of 08-25 pipeline.log).
3. Stop DST/hour-window skips: pipeline must run on the UTC date, not “if
   local hour ∈ [0,9]”.
4. Guardrails must not abort scorecard emission for metadata-only CONV-5
   hits in tests; allowlist or inject clocks. Fail-closed on real PD-3.
5. Compact must not emit 0-row trade parquet while the raw log has trades;
   refuse and P1.
6. Move `data/` off Downloads (W-6). Parameterized path. Backup drill.
7. Disk: 41.7 GB / 40 GB — raise budget **or** prune only after hash verify
   (C-2). Never delete raw to “make room” without compact proof.

### P1 — Burst-free week (existing spec 024)

Keep Hyperliquid BTC+ETH as the required set. Diagnose ~3h WS rotation
bursts on the VPS path (already hypothesized). Do not widen the symbol set
to make coverage look better.

### P2 — Paper (spec 051)

Nightly `sim paper-tail` (or `mp run --mode paper` when that binary exists)
on yesterday’s growing HL log for **one** strategy: `swing-range-reclaim-v1`
(swing horizon, matches recorded book) **or** a frozen null strategy if the
swing idea is not ready. Journal PnL. Compare to a same-seed backtest of
the closed day (G3). Kill-latch and risk gate on. No orders leave the box.

### P3 — Console (spec 052)

Ship 041 slice A only: read-only health. Reuse `termd.py` + `mp-ops status`.
Do not start egui/WASM (011). Do not add Auth0.

### P4 — Alpha (spec 053)

Active queue (max one): **funding-arb-v1** once ≥3 overlap days exist.
Everything else: killed, held, or frozen. Port Pine **only** through the
Rust funnel with costs; until then the Pine README is labelled non-evidence.

---

## 5. Owner decisions (do not guess)

Record answers in this file’s Decisions when the owner replies.

1. Canonical recorder host: **VPS-only** (recommended) vs keep Windows as
   master via drain.
2. Data disk path for W-6 (must not be Downloads).
3. Active paper strategy: swing-range-reclaim-v1 vs null vs wait.
4. Whether 08-25..08-30 are unrecoverable (waiver) or VPS still has the bytes.

---

## 6. Ideas that are *after* v1

See `docs/IDEAS-2026-08-31.md`. None of them jump this queue.

---

## Decisions

| Date | Decision |
|---|---|
| 2026-08-31 | v1 = D1–D6. Remaining specs are not the definition of done. |
| 2026-08-31 | Live adapter stays PD-1 blocked. Completing v1 ≠ permission to trade. |
