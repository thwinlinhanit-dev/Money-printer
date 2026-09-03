**STATUS: FROZEN — docs moratorium (2026-09-03), see CLAUDE.md §Docs moratorium.** Superseded by `docs/COMPLETION-MASTER-PLAN.md` (D1–D6) and `docs/STATUS.md`. Read-only history: do not update, extend, or act on this file until D1–D6 land.

# Remaining Spec Work — Assessment & Execution Plan (2026-08-28)

Status of the "implement all remaining specs" initiative. The sub-agent fleet
is rate-capped until ~2026-08-29 22:00 UTC, so execution continues next
session; this file is the mechanical plan. Model completion: **spec 014**
(commit `586049c`) — verify every requirement against code, close gaps, add
acceptance-criterion tests named after the requirement IDs (W-2/W-3), append
dated Decisions for every spec/code deviation (W-5), update the status row
(W-7).

## Lesson from the 014 verification (applies to every spec below)

The mechanics of most "draft"/"implementing" specs are already built; the
systematic gaps are: (1) zero acceptance-criterion tests (CONV-21 violation),
(2) config plumbing never wired, (3) spec text drift vs implementation
(stale paths, sync-vs-async design) needing Decisions entries, (4) signal/
integration wiring that crosses crate boundaries. Expect the same shape.

## Cluster 1 — collectors (owns `collectors/**`, `ops/systemd/*`, watchdog)

| Spec | Known present | Expected gaps |
|---|---|---|
| 013 WS Backpressure (BKP) | `backpressure.rs`, `ws.rs` | bkp_* tests; policy wiring verification |
| 019 Collector Binary & Systemd | `mp-collector`, systemd units | `[fsync]` TOML parse → `set_fsync_policy`; unit-file vs spec diff |
| 020 Binance REST Snapshot | `binance.rs` snapshot + reseed (COL-21/23/24) | verification pass; missing col_* tests |
| 032 Multi-Symbol Fan-Out (MSC) | per-symbol watchdog spawns | msc_* tests; config edge cases |
| 014 collector side | `mp-collector` shutdown fsync wired | SIGTERM (`SignalKind::terminate`) + same hook in `mp-coinalyze`/`mp-defillama`/`mp-ibit` |

## Cluster 2 — core (owns `core/**`)

| Spec | Known present | Expected gaps |
|---|---|---|
| 012 Zero-Copy Pipeline (ZCP) | `ring.rs` (post-audit sound handoff), `arena.rs`, `codec.rs`, bench | zcp_* tests; scope Decisions for anything demanding a wire redesign |

014 core side: **done** (`586049c`).

## Cluster 3 — intelligence/ops drafts (owns `storage/materialize*`, `ops/bot.rs`, `ops/journal.rs`, `features/screener.rs`, `features/hit_journal.rs`, `core/symbol.rs`)

| Spec | Known present | Expected gaps |
|---|---|---|
| 016 Materialization (MAT) | `materialize.rs`, `mp-materialize`, MAT-5 determinism | mat_* tests |
| 017 Screener Grading (GRD) | `hit_journal.rs`, `signal_catalog.rs` | grd_* tests |
| 021 Bot Command Journal | `bot.rs`, `journal.rs` | journaling/idempotency tests |
| 022 Screener Cadence | `screener.rs`, `config.rs` | cadence tests |
| 023 String Interning | `symbol.rs` | interning tests |

## Cluster 4 — strategies/mode (owns `strategies/**`, `core/mode.rs`, `sim/**`)

| Spec | Known present | Expected gaps |
|---|---|---|
| 015 carry-v1 | `carry_v1.rs` + funnel + hypothesis | requirement-by-requirement verification; str tests |
| 018 Mode Switch (MOD) | `mode.rs` (live refused → Sleep), paper/shadow in sim | mod_* tests; live requirements tested AS GUARDS (PD-1) |

## Cluster 5 — verification-only (blocked or needs owner input)

| Spec | Blocker / note |
|---|---|
| 000 Conventions | meta-spec; stays "implementing" by design |
| 002 / 003 / 004 | broad; largely built — per-requirement ID audit to find untested IDs |
| 007 Execution | live venue adapter + real credentials are **PD-1-blocked**; paper-path requirements may be verifiable now |
| 009 Ops | opsd/telegram/deadman/bot built; run remaining OPS-* audit |
| 011 WASM Terminal | large UI surface beyond 041's `termd.py`; needs scope decision (S/M/L = L) |
| 041 Analytics Terminal | `termd.py` live; Python-side tests + remaining REST surfaces |
| 046 / 047 | "cold-store write + watermark tests pending" — those tests are offline-fixture-testable; live integration needs real API data |
| 048 Correlation | cor_1..cor_7 pass; verify regime wiring end-to-end |

## Hard blockers (owner decisions, not implementable by agents)

1. **PD-1 items** in 007/018 (live adapter, live activation) — owner-only.
2. **SCHEMA_VER 5→6 golden freeze** (`bdc_1/2/8` failing): the schema bump in
   `8f56443` needs the proper re-pin/migration procedure or a revert — this
   blocks `mp-determinism` and therefore the Phase-0 promotion gate.
3. **ops promotion tests ×3** failing (verdict no longer contains
   `PROMOTED`/`burst_days`): Phase-0 gate verdict must be restored.
4. Any spec whose **Open questions** section is unresolved — do not guess.
