# Owner Decision — the binance-futures raw era (never INT-4-gateable)

**Owner:** thwin
**Date drafted:** 2026-09-07
**Status:** **SIGNED 2026-09-11 — Option B1** (grilling session, "all recs").
Executed same day: mirror sha256-verified 38/38 (19,975,043,408 bytes,
`data/retest/mirror_verify_report.txt`), off-host verify-green confirmed,
38 `action=owner_approved` entries appended to
`data/retention_delete_manifest.jsonl`, era deleted from `data/raw`.
The era is now mirror + off-host only (restorable via the drill patterns).
Implementation note: `offhost_backup.ps1` gained an owner-approved-deletion
exemption in its staging prune — sources deleted by a journaled owner
decision KEEP their off-host artifacts, otherwise the nightly prune would
have destroyed the only remaining copy of exactly the data this decision
preserves.

## 1. The thing being decided

`data/raw` contains **38 binance-futures day-logs, 19.98 GB**, the only
binance-futures coverage this project ever recorded (binance recording
stopped at the 2026-08-18 §6 VPS handoff; the VPS era records hyperliquid +
bybit). The era spans **2026-07-18 → 2026-08-08** (BTCUSDT + ETHUSDT;
SOLUSDT present on 07-22 only; 08-01/08-02 never recorded; 07-20 and
07-22/23-ETH already pruned with proofs).

The problem: **the automated retention chain can never reclaim this block.**
The chain is compact (INT-4 audit gate) → cold proof → `mp-ops prune`
(hash-verified, journaled). The INT-4 gate refuses every one of these days,
so they can never gain a compaction proof, so the W-6 guard will refuse the
delete forever. Measured at 2026-09-07:

| Evidence | Finding |
|---|---|
| Representative day (binance 07-19 BTCUSDT) | `low_coverage` 0.7193 < 0.995, `coverage_gap` 10 gaps, `missing_provenance` |
| Era-wide audit (sampled 08-06) | coverage 0.978 < 0.995, 91 stale streams, `sequence_gap` findings |
| Legacy mixed-venue era (07-22/23 BTCUSDT) | `venue_mismatch: event has Hyperliquid` — pre-split collector frames |

The decisive one is `missing_provenance`: the legacy schema does not carry
live-attribution metadata, so **no re-processing can ever make these days
pass** — the provenance is structurally absent, not repairable. The audit
gate (spec 024, 08-03 decision) is correctness-fail-closed by design, and
this is exactly the case it exists for.

## 2. What is NOT on the table

- **Bypassing the gate.** The W-6 guard (A-1/A-12) refuses without proof; a
  manual delete outside the journaled gate is the violation class the
  retention journal exists to expose (it is already surfaced as a WARN in
  `bybit_retention_check.ps1` for unjournaled absences). Any delete here
  must be an **owner decision, journaled** (§6).
- **Re-recording.** The era is one-off coverage; nothing records binance
  anymore. Re-pulling from Binance's historical dumps would be a new ingest
  pipeline for ~3 weeks of 98%-coverage data that already exists locally.
  Not cost-effective.

## 3. Option A — ACCEPT (recommended default)

Keep the 19.98 GB in place, permanently, as a historical archive. The era
is already cold (never referenced by features — only promoted days
materialize; un-gateable days are excluded from scorecards, features, and
cross-venue research by construction).

**Tradeoffs**

- **Honest, zero-risk, zero-tooling.** Nothing to build, nothing to lose.
  The data survives in the master plus two mirrors (`C:\mp-backup\data\raw`
  holds all 41 binance files incl. the pruned 3; off-host rclone tier holds
  the corpus).
- **Permanent P2.** The corpus floors at ~34–35 GB with this block (≈ 20 GB
  binance + 1.8 GB bybit-08-22 + hot-14-days ≈ 10 GB + HL/other ≈ 2–3 GB).
  At ~0.71 GB/day growth that is ~5–8 days to the 40 GB cap — the
  `storage-budget` P2 (projection-based, 14-day horizon) fires on most
  nights **forever**. The alert is a warning channel, not a breaker, and the
  projection is self-limiting (hot days age out at the same rate they grow),
  but the noise is real and permanent.
- **Disk is NOT the binding constraint.** C: has 60 GB free (88% used); the
  20 GB block is comfortably inside disk capacity. Only the corpus *cap*
  (config, 40 GB) is tight.
- **Mitigation pair:** ACCEPT + raise-cap (§5) clears the permanent P2 with
  full honesty; ACCEPT alone keeps the P2 as standing noise. ACCEPT +
  reclassify (treat the floor as by-design and widen the alert horizon) is
  the soft variant.

## 4. Option B — MIGRATE (shrink or relocate, then delete from master)

Three sub-options, in increasing order of effort:

1. **Owner-approved delete from the master only** (data stays off-host).
   The era is already byte-verified in `C:\mp-backup\data\raw` (41 files)
   and the off-host tier. Reclaim 19.98 GB from `data/raw`; nothing is
   lost. Requires a journaled owner entry (§6) since the automated gate
   will not bless it. This is the cheapest real relief and the only one
   that permanently removes the block from the budget math.
2. **Wire-format re-encode (varint slimming, ~29% payload reduction, spec
   001 appendix).** Shrinks ~19.98 → ~14.2 GB. Requires building a legacy
   re-encode pass + corpus re-verify, and does NOT change gate-ability
   (coverage/provenance findings persist — the events are the events). Only
   worthwhile if the varint flip is being adopted for future recording
   anyway and the tooling is shared.
3. **Full repair/backfill.** Impossible for coverage gaps (missing frames
   cannot be invented) and for `missing_provenance` (absent by schema).
   Dead on arrival; listed only to close the option.

**Tradeoffs**

- Reclaims space for real (19.98 GB, or ~5.8 GB for the varint variant).
- Deletes must be journaled as owner decisions — W-6-clean but off the
  automated path; the era becomes mirror-only (still restorable via
  `offhost_restore_drill.ps1` / `vps_restore_drill.ps1` drill patterns).
- The 07-22/23 BTCUSDT raws (0.91 GB) with valid parquet proofs but
  clobbered day-manifests (pre-B-2 era) are the one sub-case where the
  PROOF exists — a journaled owner delete there is extra-defensible (the
  cold data is already migrated; the raw is redundant).
- ~0 bytes of junk can go regardless of the option: `20260718_binance_BTCUSDT.log`,
  `20260722_binance_SOLUSDT.log`, `trace_20260806_binance_BTCUSDT.log`,
  `trace_20260806_binance_ETHUSDT.log` (all zero-byte; the trace_ files also
  violate the `{YYYYMMDD}_` naming the budget parse expects).

## 5. Option C — RAISE THE CAP

`MP_STORAGE_BUDGET_BYTES` is explicit operator config (Windows: 40 GB).
Raising it to 50 GB clears the projection-based P2 (floor 34.6 GB →
15.4 GB headroom ≈ 21 days > 14-day horizon).

**Tradeoffs**

- **Does not fix anything** — the block stays in the corpus; the alert
  horizon just grows. It is only legitimate **paired with ACCEPT** (cap
  matches the permanent floor + normal growth), never as a way to hide the
  block (PD-5: no silencing by config games).
- Disk headroom is real but finite: 60 GB free at 88% used. A 50 GB corpus
  cap is safe today; it assumes the mirrors (`C:\mp-backup`, tens of GB)
  keep living on C: — revisit if disk-high (OPS-7, <5% free) ever fires.
- The runbook's own guardrail: "Do NOT silence the alert by lowering the
  budget or deleting data to make the number look better." Raising the cap
  to *match measured reality* is the opposite of that and is fine when
  written down.

## 6. Execution mechanics (whichever option is signed)

- **Accept:** no action. Optional: this note cross-referenced from
  `ops/runbooks/storage-budget.md` so the standing P2 has a known owner
  decision behind it instead of being an open loop.
- **Migrate (delete from master):** append owner entries to
  `data/retention_delete_manifest.jsonl` with an `action: owner_approved`
  field (the journal is append-only JSONL; the automated reader ignores
  unknown fields), after `C:\mp-backup\data\raw` + off-host verification is
  re-confirmed the night of the delete. Then delete the files. The
  bybit_retention_check absent-day WARN will correctly stop complaining
  once the journal entry exists.
- **Raise cap:** `setx MP_STORAGE_BUDGET_BYTES 50000000000` (machine-wide,
  takes effect for new processes), and record the amendment here.
- Zero-byte junk files may be deleted at any time without ceremony — they
  carry no data (W-6 is about data; four 0-byte stubs are not).

## 7. Sign-off

Pick one (delete the others, date + initial):- [ ] **A — ACCEPT.** The binance era stays in `data/raw` permanently as a
      historical archive; the standing storage-budget P2 is accepted as
      by-design noise.  ____ (date / initial)
- [x] **B1 — MIGRATE (delete from master, keep off-host).** Owner-approved,
      journaled deletion of the 19.98 GB era from `data/raw` after
      re-verifying the mirrors.  **2026-09-11 / thwin** (executed via
      `data/retest/owner_delete_binance.ps1`, fail-closed on both
      verification gates)
- [ ] **B2 — MIGRATE (varint re-encode).** Build the legacy re-encode pass and
      shrink the era ~29% before deciding further.  ____ (date / initial)
- [ ] **C — RAISE CAP** (to ___ GB) **paired with A or B1.**  ____ (date / initial)

**Default if unsigned:** Option A — the era stays untouched (W-6
fail-closed), the P2 keeps firing, and nothing is lost.