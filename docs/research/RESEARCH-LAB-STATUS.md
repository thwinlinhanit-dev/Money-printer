# RESEARCH-LAB-STATUS — capability status (2026-09-07; initial 2026-09-04,
# Phase 7 anti-randomness gates closed 2026-09-07 — see plan §10;
# REL-30 paper-mode recording closed same day)

Spec: `specs/054-research-lab-hardening.md` (incl. REL-24..29) · Plan:
`docs/implementation/RESEARCH-LAB-HARDENING-PLAN.md` · Branch:
`feature/signal-catalog-footprint`.

## PASS / FAIL per capability

| Capability | Status | Evidence |
|---|---|---|
| Data integrity — explicit quality state (REL-1..3, REL-27/R-1) | **PASS** | `features/src/data_quality.rs` — `DataQualityState` (Healthy / InsufficientHistory / Stale / Invalid / **Missing / Gap**); never neutral/0.5; screener "never observed" ⇒ Missing, gap through the staleness window ⇒ Gap until fresh min_samples heal it; Parquet codes 0–5, schema ver bumped |
| Identity — canonical + invalidation (REL-4..7) | **PASS** | `features/src/signal_identity.rs` + `signal_catalog.rs`; `apply_grade` refuses mismatched/pre-hardening grades; tests prove params / feature-version / schema / cost-model changes invalidate old evidence |
| Observations — immutable + gated (REL-8..11) | **PASS** | `features/src/observation.rs`; quality-gated recording with blocked-fire counter; deterministic ids; recorder proven write-only (decision-log hash unchanged) |
| Forward outcomes — horizons/gross/net/MFE/MAE (REL-12..14) | **PASS** | `features/src/outcome.rs`; series-coverage guard (REL-13) + no-lookahead tests; integration slice raw → feature → signal → observation → outcome |
| Evaluation & promotion (REL-15..19) | **PASS** | `features/src/evaluation.rs`; sample gate, gross-vs-net, p25/p50/p75, structured `PromotionDecision`, `grade_from_report` bridge |
| Sample-size tiers (REL-24/R-5) | **PASS** | `SampleTier { Insufficient, Preliminary, Research }`, promotion floor `RESEARCH_MIN_N = 100`; `SAMPLE_TIER_PRELIMINARY` refusal tested at 30/99/100 |
| Regime tagging & refusal (REL-25/R-6) | **PASS** | buckets from the fire snapshot's `regime.trend`; `ONLY_WORKS_IN_*` refusal, `REGIME_COVERAGE_SINGLE_*` / `REGIME_SAMPLE_TOO_SMALL_*` flags (`REGIME_MIN_TAGGED = 10`). 2026-09-07: the sim recorder captures last-seen regime per symbol into fire snapshots (`rel_25_recorder_captures_regime_context_into_snapshots`); kill board re-run with context — no `ONLY_WORKS_IN_*` because NO regime is net-positive (orderflow's gross edge concentrates in TREND: win 0.534 vs 0.498 CHOP at 15m — but net-negative in both) |
| Sustained-decay detection (REL-26/R-7) | **PASS** | chronological-window net expectancy (3 windows, ≥5 per window); two consecutive non-positive recent windows ⇒ `DECAY_SUSPECT` refusal |
| Machine-readable reject codes + JSON decision (REL-29/R-8) | **PASS** | every reject carries a `CODE: detail` reason; `PromotionDecision::to_json` = `{"decision":"REJECT","reasons":["INSUFFICIENT_SAMPLE",…]}` |
| Golden dirty fixture (gaps/invalid/insufficient) | **PASS** | `rel_28_golden_dirty_fixture_gaps_invalid_insufficient_history` — fixed dirty feed walks Missing→Insufficient→Healthy→Gap→Healthy→Invalid with frozen FNV hash `17618162958652494096` |
| Paper-mode observation recording (REL-30) | **PASS** | `sim paper` / `sim paper-tail` accept `--params-hash/--obs-dir/--horizons` — the SAME write-only recorder as backtest; `rel_30_paper_recording_is_write_only_and_deterministic` proves the batched hash is unchanged with recording on; live proof 2026-09-07: paper-vs-backtest decision-log hash identical (`17053129571326659044`), 10,740 obs → 4 Parquet files |
| Hit-journal → observation migration (REL-31) | **PASS** | `hit_to_observation` bridges ScreenerHits onto the identity-stamped flow (venue stamped at fire, venue-less hits REFUSED per R-1, Direction::Long default disclosed in params-hash); `footprint` study binary relocated to mp-storage and migrated off the gross-only JSONL backfill — outcomes (gross+net, MFE/MAE) from the Phase-4 engine, Parquet persistence, per-rule tiered reports; legacy JSONL still loads (serde defaults); live proof 2026-09-07 on the 07-19 bybit log (hit → observation → Parquet under identity `f765b9b3…`) |
| Parquet observation store (REL-23) | **PASS** | `storage/src/observation_store.rs`; round-trip, W-6 no-overwrite, deterministic content hash, partitioned write |
| Footprint/accumulation cleanup (REL-20..22) | **PASS** | market-profile approximation documented; `total_cmp` POC determinism tested for POC/VAH/VAL; accumulation evidence fields in hit snapshots, strict AND untouched |
| Determinism / reproducibility | **PASS** | golden observation hash + golden dirty-fixture hash frozen in `sim/tests/observation_engine.rs`; full workspace `cargo test` green (2026-09-07); sim decision-log golden hash (`sim_14`) unchanged |
| Live trading disabled (PD-1) | **PASS** | trading mode `sleep`; no OMS/venue wiring added; observation code is research-only |
| $0 budget / no new database | **PASS** | zero new deps outside the workspace; Parquet only |

## How to run a research evaluation

```bash
# Replay a recorded log through the production stack with observations on
# (recording is enabled by passing --params-hash); attach forward outcomes,
# persist Parquet to --obs-dir, and print per-horizon evaluation reports:
cargo run -p mp-sim --bin sim -- backtest \
    --log data/merged_hyperliquid_BTC_4d.log \
    --strategy carry-v1 --seed 1 --run-id eval-2026-09-04 --runs-dir data/runs \
    --params-hash <features.toml params hash> \
    --obs-dir data/observations --horizons "15m,1h,4h,1d"
# → per-horizon lines: n / gross_exp / net_exp / win / p25/p50/p75 and
#   "GATE PASS …" or "GATE REFUSED — <explicit reasons>" (REL-15..18)
# Verified 2026-09-04 on the 4-day hyperliquid BTC log: 10,740 observations,
# 4 date-partitioned Parquet files, both horizons honestly refused
# (no positive net edge after costs — REL-16).
```

Programmatic path (libraries):
1. `mp_sim::Backtester` + `enable_observations(params_hash, feature_version,
   created_at_ns)` → run the replay.
2. `bt.attach_outcomes(&[15m, 1h, 4h, 1d].map(…))`.
3. `mp_features::evaluate(bt.observations(), horizon, min_n)` →
   `EvaluationReport` → `decide()` → `PromotionDecision`.
4. `mp_features::grade_from_report(fp, run_id, now, &report)` → catalog
   `GradeSnapshot` → `signals --file … grade …` (identity-stamped).

## Known limitations (honest)

- **Hit journal** (`hit_journal.rs`) JSONL is now the WRITE-side fire log
  only (REL-31): grading/evaluation reads the identity-stamped observation
  store. The legacy gross-only backfill is retired; old journal files still
  parse (serde defaults) and never block. GRD-4's execution-shaped entry
  convention remains binding for fill studies (spec 017 amendment
  2026-09-07); research artifacts use REL-14's measurement-shaped entry.
- The backtester's observation marks are event-sampled (trades/marks/mid), so
  MFE/MAE are sampled excursions, not continuous-path excursions. Deterministic
  and honest, but documented as an approximation.
- Bar-based market profile is an approximation (REL-20); trade-level footprint
  requires full L2 and is out of scope.
- Bootstrap CIs / FDR correction and multi-regime taxonomy: out of scope
  (deferred).
- Paper-mode recording (REL-30) covers the REPLAYED/tailed feed path. A
  true real-time collector-integrated recorder (in the collector process,
  not the sim binary) remains a follow-up; the write-only pattern and
  artifacts are identical.

## Cross-tape kill panel (2026-09-09) — do refusals replicate?

Round 3: all 5 strategies on `data/swing_hyperliquid_BTC.log` (789 MB,
2026-07-19 → 2026-08-18, ~31.6 days — the daily-paper rehearsal tape) plus a
`coinflip-any` re-run on the 4-day tape. params-hash `rel32-swing-baseline`;
runs journaled as `eval-20260909-*-swing` and `eval-20260909-control-4d-rerun`;
script archived at `data/panel-swing-r2.sh`.

| Strategy | 4d tape (08-18→22) | swing tape (07-19→08-18) | Replicates? |
|---|---|---|---|
| coinflip-any (REL-32 control) | REJECT ×4 (NET_NEG+DECAY) — despite a POSITIVE raw journal expectancy (+26.4/trade) | REJECT ×4 (NET_NEG+DECAY), n≈23k | YES — noise killed on both tapes |
| carry-v1 | REJECT ×4 (NET_NEG+DECAY) | REJECT ×4 (NET_NEG+DECAY), n≈19k; 1d gross +0.000194 but net −0.000556 | YES |
| orderflow-v1 | REJECT ×4 (NET_NEG+DECAY) | REJECT at 15m/1d; **GATE PASS at 1h (net +0.000281) and 4h (+0.000566)**, net-positive in BOTH regimes | **NO — the pass is tape-dependent** |
| liq-fade-v1 | n=0 → INSUFFICIENT_SAMPLE | n=0 → INSUFFICIENT_SAMPLE | YES (honest refusal) |
| swing-range-reclaim-v1 | n=0 → INSUFFICIENT_SAMPLE | n=0 → INSUFFICIENT_SAMPLE | YES (honest refusal) |

Findings:
- The R-8 control loop is closed: `coinflip-any` fires on hyperliquid
  (prefix `cvd.` subscription) and the pipeline refuses it at every horizon on
  every tape — even when the raw run journal looks profitable. The net gate
  (REL-16) and decay gate (REL-26) are what kill noise; gross-only grading
  would have PASSED the 4d control.
- orderflow-v1's swing-window 1h/4h PASS reproduces the regime structure from
  round 2 (TREND and CHOP both net-positive at those horizons) but does NOT
  survive the 4-day window (REFUSED + DECAY_SUSPECT there). Per R-7 this is a
  retest mandate, not a promotion: one passing window is preliminary evidence;
  the same identity flipping between PASS and REFUSED across adjacent tapes is
  exactly the instability the multi-window process exists to catch.
- Zero-fire strategies are refused honestly at n=0 on both tapes (no
  fabrication, R-1).
- Determinism cross-checks: carry-v1 on the swing tape reproduces the REL-30
  paper run bit-for-bit (12,006 trades, identical expectancy −8.0816); the 4d
  control rerun journaled identical to its first pass (136,277 trades).
- Artifacts: 3 new identity dirs, 41 new Parquet files. Both control tapes
  share ONE identity (`8e415159…`) — the fingerprint is tape-independent by
  design; date partitions separate the tapes and the W-6 guard accepted both
  writes (no symbols_hash drift: same venue/symbol universe).