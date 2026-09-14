# 051 — Daily Paper Rehearsal

## Purpose

ROADMAP Phase 4 (“2 weeks paper, zero faults, paper ≈ sim within G3”) has
a CLI (`sim paper` / `sim paper-tail`) but no **scheduled product**. This
spec makes paper a nightly rehearsal so the owner learns operational
faults before any live discussion.

## Scope

In: scheduled paper session over Hyperliquid BTC (and ETH if cheap), sim
fills, risk gate, kill-latch, decision-log identity vs closed-day
backtest, Telegram summary, run journal. Out: real orders (PD-1), live
venue adapter, WASM, new strategies.

## Design

One binary path already exists:

- `sim paper` — one-shot over a completed log
- `sim paper-tail` — poll a growing log, idle-stop

v1 rehearsal uses **closed-day paper**: after the day file is drained
(or locally complete), run the same engine as backtest with
`TradingMode::Paper` semantics (live timestamps in the log, sim fills).
This is stricter and more deterministic than tailing. Tailing is a later
slice (PAP-8) once closed-day G3 is green.

Strategy: **one** id from config `paper.strategy` (default
`swing-range-reclaim-v1`). If that strategy is not registered, fail-closed
rather than silently running `NullStrategy` (Null is allowed only when
config explicitly sets `paper.strategy = "null"` for a plumbing drill).

## Requirements

- **PAP-1** A scheduled task `MoneyPrinterPaper` MUST run after a
  successful-or-refused scorecard for UTC yesterday (pipeline may have
  failed compact; paper still runs if the raw log decodes).
- **PAP-2** Session MUST use sim fills only (`uses_sim_fills`). No OMS
  venue trait object is constructed. Test: `rg` / crate graph — paper
  binary path does not link a networked oms adapter (there is none).
- **PAP-3** Risk gate MUST be on with finite RG budgets from
  `risk.example.toml` copied to off-repo `paper-risk.toml`.
  `reconciler_clean` MUST be **false** unless a real reconciler ran
  (today: paper has no broker; set `reconciler_clean=true` only with a
  comment that paper has nothing to reconcile — and a test that live
  mode cannot use this hardcoded true; pin M-5).
- **PAP-4** Kill-latch file MUST be loaded fail-closed (08-28 H-2). A
  tripped GLOBAL latch ⇒ paper still **runs** but emits zero intents and
  journals `latched`.
- **PAP-5** For a closed day, paper decision-log hash MUST equal
  backtest decision-log hash at the same seed and config (G3 / SIM-14
  shape). Mismatch is P1 `paper-sim-divergence`.
- **PAP-6** Journal a `runs/index.jsonl` record `kind=paper` with
  expectancy, trade count, faults, hashes. Append-only.
- **PAP-7** Telegram `id=daily-paper`: trades, expectancy, faults,
  divergence bool. Quiet-hours MAY batch P3; faults are P2 unbatched.
- **PAP-8** (slice B, not v1-blocking) `paper-tail` on the live VPS log
  with idle stop; identity vs later closed-day paper within documented
  tolerance (duplicate-frame count).
- **PAP-9** Fourteen consecutive paper sessions with `faults=0` and
  PAP-5 pass ⇒ ROADMAP Phase 4 evidence row may be checked. A single
  fault resets the consecutive count (same spirit as promotion streak).
- **PAP-10** Paper MUST refuse `TradingMode::Live` config. Env
  `MONEY_PRINTER_MODE=live` during paper task ⇒ Sleep + P1, no session.
- **PAP-11** (2026-09-09, spec 054 REL-32 cross-ref) Every closed-day paper
  rehearsal MUST also run the venue-generic noise control `coinflip-any`
  over the SAME log and seed, with recording enabled (`--params-hash
  pap11-noise-baseline --obs-dir <obs-dir>`) so the spec 054 gates grade it.
  Expected: control fires on the venue'd log and is REFUSED at every
  horizon. Verdicts: control `GATE PASS` at ANY horizon ⇒ the gate chain is
  broken ⇒ session FAULT (exit 1) + P1 (noise must never survive the net
  gate). Control fires 0 ⇒ P3 WARN (baseline void on this log — nothing
  certified). Control leg crash ⇒ session fault, journaled. The control leg
  never gates the primary strategy's result; it certifies the evaluation
  pipeline itself (R-8). Journaled as `kind=paper-noise-baseline`,
  `run_id=<primary>-noise`.
- **PAP-12** (2026-09-09, spec 054 REL-30 cross-ref) The PRIMARY paper leg
  records observations nightly: the rehearsal invokes `sim paper` with
  `--params-hash pap1-primary --obs-dir <root>/data/observations`, so the
  research corpus grows from the daily schedule (identity-stamped,
  date-partitioned Parquet under one stable fingerprint — the fingerprint is
  tape/date-independent by design; per-day partitions accumulate beneath it).
  Recording is write-only (REL-30): it MUST NOT alter the decision path or
  the primary leg's verdict. Latched (`--zero-intents`) sessions record
  nothing (0 fires) — harmless. Journal row gains `observations=<n> recorded`.
  Re-run semantics follow the W-6 guard: identical re-run for the same date
  is a byte-identical no-op; a re-run over a CHANGED log for the same date
  W-6-refuses and faults the leg BY DESIGN (divergent evidence must surface,
  never silently overwrite).

## Acceptance criteria

- [ ] `pap_1_task_invokes_closed_day` — script unit or ops test.
- [ ] `pap_2_no_network_oms` — cargo tree / compile cfg.
- [ ] `pap_3_gate_on` — intents above cap rejected.
- [ ] `pap_4_latch_blocks_intents` — fixture latch ⇒ 0 intents.
- [ ] `pap_5_paper_equals_backtest_hash` — golden day fixture.
- [ ] `pap_6_journal_kind_paper` — index.jsonl row.
- [ ] `pap_7_telegram_payload_shape` — snapshot test.
- [ ] `pap_9_fault_resets_streak` — counter fixture.
- [ ] `pap_10_live_env_refused` — reuse mode.rs MOD-12.
- [ ] `pap_11_noise_baseline_leg` — paper-layer test: control fires on a
  Hyperliquid-venued log via the real resolver (legacy `coinflip` starves);
  rehearsal journal carries the control row; control `GATE PASS` ⇒ exit 1.

## Decisions

| Date | Decision |
|---|---|
| 2026-08-31 | Closed-day paper is v1; live tail is slice B. |
| 2026-08-31 | Default strategy swing-range-reclaim-v1 (multi-hour horizon, uses book). Owner may switch to null for a plumbing week. |
| 2026-08-31 | liq-fade-v1 is frozen (SWG-8) and MUST NOT be the paper default. |
| 2026-08-31 | PAP-1: `daily_paper.ps1` scheduled after compact (even if compact failed) per spec. |
| 2026-08-31 | PAP-4: kill-latch is fail-closed; latched sessions journal `latched` and emit 0 intents. |
| 2026-08-31 | PAP-8 (live tail) deferred to slice B — not v1-blocking. |
| 2026-08-31 | PAP-9: streak counter stored in `paper_streak_count.txt` beside journal. |
| 2026-08-31 | PAP-10: reuse MOD-12 test; live env → Sleep, no paper session. |
| 2026-09-09 | PAP-11: `coinflip-any` (spec 054 REL-32) is the paper noise baseline; fires+refused is the expected daily outcome, `GATE PASS` is a pipeline fault (P1, exit 1). Fixed params-hash keeps one control identity across days (append-only, W-6-safe: same venue/symbols). Control leg does not enter the PAP-9 streak; its own crash does count as a session fault in its journal row. |
| 2026-09-09 | PAP-12: primary leg records nightly under fixed params-hash `pap1-primary` (distinct from the control's `pap11-noise-baseline`). Same-date divergent re-runs W-6-fault by design; identical re-runs no-op. Recording never gates the primary verdict (write-only, REL-30). |

## Open questions

- Seed: fixed `42` (determinism) vs date-derived. Recommend fixed seed
  recorded in the journal.
