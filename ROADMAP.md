# Roadmap — the single execution plan

One page tying phases → specs → deliverables → validation gates. This
consolidates SYSTEM_BLUEPRINT §10 and ARCHITECTURE_BRAINSTORM §8; if they
disagree, this file wins. Capital at risk may only increase after the phase's
validation criterion is met **in writing** (checked row in this table, with
evidence links).

| Phase | Build (specs) | Deliverable | Validation gate (written evidence) | Capital at risk |
|---|---|---|---|---|
| **0 — Record** | 000, 001, 002, 003 | Binance collector 24/7 on a VPS; trades+book+funding+OI+liq for ~50 symbols → Parquet + manifests | 7 consecutive days with manifest coverage ≥ 0.995 on core symbols, and zero `stale_bursts` across the qualifying window (Phase-0 promotion condition, spec 024, amendment 2026-08-12) | $0 |
| **1 — See** | 009 (partial: opsd, Telegram, dead-man) + Grafana | Funding/OI/liq dashboards; cascade + funding-extreme alerts to phone | One alert you acted on; dead-man fires in a kill-drill | $0 |
| **2 — Perceive** | 004 | Feature engine live + materialized store; screener with hit journal | 30 days of graded screener hits; online/offline golden test green | $0 |
| **3 — Judge** | 005, 006 (funnel CLI) | Backtester L0/L1, walk-forward, MC, experiment tracker; funnel operating | One idea honestly killed with autopsy; golden determinism fixture in CI | $0 |
| **3.5 — Research lab hardening** | 054 | Deterministic observation/outcome/evaluation stack: identity-stamped immutable observations (Parquet), forward outcomes, evaluation gates, market-profile cleanup | `cargo test` green incl. golden observation hash; no-lookahead + identity-invalidation tests; live still disabled (spec 054, 2026-09-04) | $0 |
| **4 — Rehearse** | 007 (paper/shadow path), 008 | Paper mode on live feeds; sizing engine; chaos drills passed | 2 weeks paper, zero faults, paper ≈ sim within G3 tolerance | $0 |
| **5 — Execute small** | 007 (live path), 009 (complete) | Binance testnet → live-small; carry-v1 through G3 | 4 weeks live-small: clean reconciliation, live ≈ paper within G4 tolerance | fixed min risk (owner sets $) |
| **6 — Portfolio** | 006 (strategies 2–3), 008 (allocator) | trend-breadth-v1 + liq-fade-v1 through funnel; allocator + monthly report | Blended curve smoother than best component; first monthly report generated | allocator-managed, ≤ quarter-Kelly |
| **7 — Compound & extend** | 010 (LLM agents), 011 (terminal), BACKLOG picks | Daily brief; more venues; scale within capacity | Rolling 6-month expectancy > 0 after all costs vs benchmark row | scales with evidence only |

**Standing rules across all phases**
- Any Prime Directive conflict halts the phase (CLAUDE.md).
- Demotion/de-risking is automatic; promotion/re-risking needs the human click.
- Every phase ships something used daily even if the project stops there.
- New ideas do not jump the queue: they enter `docs/BACKLOG.md`, get a spec,
  then get built. (This file changes rarely; the backlog changes often.)

## Zero-Cost Mode (2026-08-31)

Under $0 budget / free-tier constraints, Phase-0 is redefined as
**Zero-Cost Mode** (see `docs/ZERO_COST_MODE.md`):

- **Venue:** Hyperliquid only (permissionless, no geo-blocks, no KYC)
- **Symbols:** BTC + ETH only
- **Streams:** trades + funding + OI + mark_price (no full L2 book)
- **Storage:** < 530 MB/day compressed (Phase-0 ~215 MB + swing ~311 MB); strict 3-tier retention
- **Promotion:** 14 consecutive clean days, coverage >= 0.95, stale bursts
  are warnings only, full book absence is expected

The swing collector path (`swing_collectors.ps1` / `deploy_swing.sh`) adds
cross-asset research context (Bybit BTC/ETH/SOL, whale positions, FRED macro)
alongside the Phase-0 Hyperliquid collectors. These are **not gate-required**
but provide correlation breadth for signal research.

The original Phase-0 gate (7 consecutive days, coverage >= 0.995, zero
stale bursts, full book required) is **deferred indefinitely** under current
constraints. It remains the target if the system moves to a paid VPS or the
owner obtains Oracle Always Free.

Full L2 book recording, multi-symbol expansion beyond BTC+ETH+SOL, options,
heavy whale census, and cross-asset analytics are all deferred until storage
is proven stable for months on free-tier. Bybit recordings are NOT deferred
— they are part of the swing research path.

**v1 completion (2026-08-31):** [`docs/COMPLETION-MASTER-PLAN.md`](docs/COMPLETION-MASTER-PLAN.md)
defines D1–D6. Specs 050–053. Capital remains $0. Live is not in v1.

**Current status:** Phase 0 is **Zero-Cost Mode** (see `docs/ZERO_COST_MODE.md`).
Live **Hyperliquid** recorder (BTC, ETH) since 2026-08-08. Last archived
scorecard **2026-08-24**; gate blind 08-25..08-30. Zero-Cost Mode is the
**active Phase-0 path** under $0 budget constraints:

- **Venue:** Hyperliquid only (permissionless, no geo-blocks, no KYC)
- **Symbols:** BTC + ETH only
- **Streams:** trades + funding + OI + mark_price (no full L2 book)
- **Promotion:** 14 consecutive clean days, coverage ≥ 0.95, stale bursts
  are warnings only, full book absence is expected
- **Retention:** 14-day hot-tier pruning (raw logs), ZSTD ≥ 6 compression
- **Daily pipeline:** `daily_maintenance.sh` (VPS, `ZERO_COST=1` default)
  and `daily_pipeline.ps1` (Windows, `$env:ZERO_COST=1` default)

The original Phase-0 gate (7 consecutive days, coverage ≥ 0.995, zero
stale bursts, full book required) is **deferred indefinitely** under current
constraints. It remains the target if the system moves to a paid VPS.

Workspace + collectors + sim/research stack built. Binance futures WS
non-book streams are egress-filtered from this network (spec 024 —
Hyperliquid's permissionless API is not geo-blocked; see
`ops/core_symbols.txt`). The daily integrity gate is automated
(`MoneyPrinterDailyPipeline` / `daily_maintenance.sh`). The Phase-0 core
symbol set is a single source of truth (`ops/core_symbols.txt`,
`hyperliquid:BTC`/`hyperliquid:ETH`).

The ~3h20m-periodic WS burst on both symbols is a real data hole on the
VPN/Wi-Fi path, NOT a collector defect (see `ops/runbooks/daily-pipeline.md`;
keepalive pings added 2026-08-12 and VPS A-B in progress per
`ops/runbooks/vps-phase0-bringup.md` §6, profiler
`ops/scripts/audit_bursts.py`). 2026-08-12: the Phase-0 promotion now
additionally requires zero `stale_bursts` across the qualifying 7-day
window (spec 024, amendment 2026-08-12 — enforced in `mp-ops promote`;
a burst day still counts toward the streak, but `PROMOTED` needs a
burst-free window, and a held-back verdict names the burst days via
`burst_days`) — the egress hypothesis is an enforced condition, so a
promoted streak is also a confirmed root-cause proof. Capital at risk: $0.

2026-08-13 (ops/data self-verification batch): the system now proves itself
daily and loudly in one place — `mp-ops status` (OPS-16) aggregates mode,
promotion verdict, latest scorecard margins, coverage trend, last backup
manifest entry, pipeline log, and kill-latch state into one JSON document;
`mp-ops pipeline-stale` (OPS-17, P1 runbook `ops/runbooks/pipeline-stale.md`)
watches the gate itself (no scorecard by 00:15 UTC ⇒ P1), wired hourly on the
VPS cron and as `MoneyPrinterPipelineStale` on Windows; spec 026 now also
classifies *value* — the whole-day veracity pass (CVG-13..15) flags
`price_divergence`/`trade_drought` per window even when presence looks
perfect; and `scorecard --reuse-unchanged` makes historical re-scoring cheap
(source size+mtime+config fingerprint sidecar, spec 024 sidecar). Spec 018
upgraded draft → ready and its proof implemented 2026-08-13: `mp-determinism`
replays yesterday's session through the production runtime (same loader as the
materializer) and requires a byte-identical decision log; the PASS artifact is
a promotion-gate condition, wired fail-closed into both daily pipelines and
verified byte-identical on the real 08-10..08-13 corpus.

**Phase-0 audit baseline (2026-08-04):** `mp-audit --data-dir data-migrated
--venue binance --symbol BTCUSDT --date 20260729` → DIRTY (`sequence_gap` +
`coverage_gap` findings), promotion gate reports **0 consecutive clean days
(required 7)** — gate not yet met, honestly. The recv-monotonic write-boundary
fix (spec 024, INT-6/7) is in place to stop `recv_time_reversal` from dirtying
clean recordings; remaining gaps are real venue-side `sequence_gap`s to
diagnose per `ops/runbooks/stream-gap.md`. Full multi-day audit pending
(unabridged scan over the multi-GB corpus is slow; run per-day or on a slice
— 2026-08-13: `mp-ops scorecard --reuse-unchanged` now skips re-auditing
days whose raw source is byte-unchanged (size+mtime+config fingerprint
sidecar), so bulk historical re-scoring is O(changed days) instead of
O(corpus)).
