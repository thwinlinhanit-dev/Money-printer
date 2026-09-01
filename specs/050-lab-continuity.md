# 050 — Lab Continuity (gate, drain, clock, storage home)

## Purpose

Make the daily evidence loop **unskippable**. A research lab that cannot
score yesterday is not incomplete — it is lying by omission. This spec
closes the failure modes that blinded the gate on 2026-08-13..16,
2026-08-19..21 (incident), and 2026-08-25..30.

## Scope

In: daily_pipeline scheduling, guardrail vs scorecard ordering, VPS drain
release, scorecard plausibility, compact non-empty proof, data directory
location, storage budget vs prune. Out: strategy logic, live OMS, WASM UI,
new venues.

## Design

The loop is:

```
VPS collectors (canonical writers)
    → UTC day close
    → drain byte-verified files to Windows data home (or score on VPS)
    → guardrails (warn vs fail)
    → mp-ops scorecard (refuse all-zero)
    → mp-determinism
    → promote streak
    → materialize / compact (refuse 0-row when raw has trades)
    → telegram + pipeline-stale dead-man
```

**Principle:** scoring yesterday is more important than a green guardrail
on an unrelated test file. Guardrails still fail the *build/push* path
(W-8). The **pipeline** distinguishes:

- **Hard stop (no scorecard):** PD-1 live wiring, PD-2 secret in tree,
  missing `mp-ops`, all-zero events, missing required raw files.
- **Soft fail (scorecard still emitted, P2):** CONV-5 in `#[cfg(test)]`
  code, clippy, draft-spec CONV-21 gaps.

DST: the pipeline is keyed by **UTC date of the closed session**, not by
the host’s local hour. If the scheduled task wakes late, it still scores
the missing UTC day (catch-up), then today’s.

## Requirements

- **LAB-1** The daily pipeline MUST attempt a scorecard for every UTC
  calendar day after collectors are in production. A skip because
  `hour ∉ [0,9]` is a defect. Catch-up MUST score every missing date in
  `[last_archived+1, utc_yesterday]`.
- **LAB-2** Pipeline MUST NOT treat `SystemTime` / `temp_dir` in
  `#[cfg(test)]` modules as a PD-3 violation that aborts scoring.
  Guardrails.ps1/.sh MUST either ignore test modules or allowlist
  `storage/src/audit.rs` tests the way `core/src/wall_clock.rs` is
  allowlisted. Decision-path `now()` outside tests remains a hard fail.
- **LAB-3** If required raw logs are absent, scorecard MUST refuse with
  the 08-22 all-zero / `recording_missing` path (exit ≠ 0) and fire
  pipeline-stale P1. It MUST NOT write `promotable: true`.
- **LAB-4** Drain MUST confirm remote delete only after local
  size+checksum match. `ssh_failed` after a successful local land MUST
  remain `held` and P2 — never silent. Fourteen-file hold (08-25) is the
  regression fixture shape.
- **LAB-5** Compact of `trade` parquet for a venue/symbol/day MUST fail
  (exit ≠ 0, P1) if the raw log for that day decoded `n_trades > 0` and
  the parquet row count is 0. “1 file written (0 rows)” on a live HL day
  is a bug, not success.
- **LAB-6** Materialize MUST NOT fail the whole pipeline on
  `symbols_hash` mismatch by aborting the scorecard (scorecard already
  archived). Overwrite refusal (W-6) stays; pipeline continues and
  emits P2 `materialize-hash-conflict` with both hashes.
- **LAB-7** `MP_DATA_HOME` (env) or `--data-dir` MUST be the unique data
  root. Default MUST NOT be a path containing `Downloads`. Missing env
  on the scheduled task is a hard stop. README W-6 is the rationale.
- **LAB-8** Storage budget alert remains P2 at cap. Prune MUST keep
  spec 003 / C-2 `source_log_hash` verify. Crossing cap MUST NOT delete
  raw.
- **LAB-9** `mp-ops status` MUST expose: last scorecard date, days_since,
  drain held count, data_home path, compact_zero_row incidents (7d).
- **LAB-10** A catch-up of N missing days MUST be idempotent
  (`--reuse-unchanged`) and MUST not invent CLEAN for days whose raw is
  missing.

## Acceptance criteria

Each item is an automated test named after the ID (CONV-21).

- [ ] `lab_1_catchup_scores_gap_days` — fixture with cards for D and D+3
      missing D+1,D+2; catch-up writes two refusals or two real cards,
      never skips for hour-of-day.
- [ ] `lab_2_test_module_now_does_not_fail_pipeline_guard` — a synthetic
      `#[cfg(test)]` `SystemTime::now` does not trip the pipeline-hard
      path; the same call in `features/src` still trips hard.
- [ ] `lab_3_missing_raw_no_promotable_card` — already partially in
      audit tests; pin pipeline exit.
- [ ] `lab_4_held_after_ssh_fail` — drain fixture: local match + ssh
      fail ⇒ held list non-empty, no remote delete.
- [ ] `lab_5_compact_refuses_zero_row_when_trades_exist` — raw log with
      ≥1 trade ⇒ compact error if parquet rows == 0.
- [ ] `lab_6_materialize_hash_conflict_is_p2_not_pipeline_abort` —
      scorecard remains; pipeline exit 0 or 2 documented; P2 journaled.
- [ ] `lab_7_downloads_path_fails_check_config` — `--check-config` exit
      2 if data-dir contains `Downloads` (case-insensitive).
- [ ] `lab_8_prune_still_requires_hash` — reuse prune regression from
      08-28 C-2.
- [ ] `lab_9_status_includes_days_since` — JSON field present.
- [ ] `lab_10_catchup_idempotent` — second catch-up no duplicate cards.

## Decisions

| Date | Decision |
|---|---|
| 2026-08-31 | Scoring yesterday outranks green guardrails on test-only CONV-5. Push/CI still fail-closed. |
| 2026-08-31 | Canonical writers stay on VPS until the owner picks otherwise; Windows is the research corpus home. |
| 2026-08-31 | LAB-6: hash-conflict journal exit code 0 (pipeline continues); spec allows exit 0 or 2. |
| 2026-08-31 | LAB-7: `mp-ops check-config` is a subcommand, not a `--data-dir` flag; cleaner CLI ergonomics, same semantics. |
| 2026-08-31 | LAB-9: `compact_zero_row_incidents` scans pipeline log lines for "0 rows" pattern in the past 7 days. |
| 2026-08-31 | LAB-1: hour-skip removal verified by regex guardrail test `lab_1_pipeline_no_hour_skip`. |

## Open questions

- Exact `MP_DATA_HOME` path (owner). Do not invent a drive letter.
- Whether to score on the VPS and copy JSON only (eliminates drain from
  the critical path). Recommended if SSH keeps failing; owner must say.
