# RESEARCH-LAB-STATUS — capability status (2026-09-04)

Spec: `specs/054-research-lab-hardening.md` · Plan:
`docs/implementation/RESEARCH-LAB-HARDENING-PLAN.md` · Branch:
`feature/signal-catalog-footprint`.

## PASS / FAIL per capability

| Capability | Status | Evidence |
|---|---|---|
| Data integrity — explicit quality state (REL-1..3) | **PASS** | `features/src/data_quality.rs` — `DataQualityState` (Healthy / InsufficientHistory / Stale / Invalid); tests `rel_1..rel_4`; accumulation blocks on short history (no 0.5 fallback), screener exposes per-rule state |
| Identity — canonical + invalidation (REL-4..7) | **PASS** | `features/src/signal_identity.rs` + `signal_catalog.rs`; `apply_grade` refuses mismatched/pre-hardening grades; tests prove params / feature-version / schema / cost-model changes invalidate old evidence |
| Observations — immutable + gated (REL-8..11) | **PASS** | `features/src/observation.rs`; quality-gated recording with blocked-fire counter; deterministic ids; recorder proven write-only (decision-log hash unchanged) |
| Forward outcomes — horizons/gross/net/MFE/MAE (REL-12..14) | **PASS** | `features/src/outcome.rs`; series-coverage guard (REL-13) + no-lookahead tests; integration slice raw → feature → signal → observation → outcome |
| Evaluation & promotion (REL-15..19) | **PASS** | `features/src/evaluation.rs`; sample gate, gross-vs-net, p25/p50/p75, structured `PromotionDecision`, `grade_from_report` bridge |
| Parquet observation store (REL-23) | **PASS** | `storage/src/observation_store.rs`; round-trip, W-6 no-overwrite, deterministic content hash, partitioned write |
| Footprint/accumulation cleanup (REL-20..22) | **PASS** | market-profile approximation documented; `total_cmp` POC determinism tested for POC/VAH/VAL; accumulation evidence fields in hit snapshots, strict AND untouched |
| Determinism / reproducibility | **PASS** | golden observation hash frozen in `sim/tests/observation_engine.rs`; `cargo test` green; sim decision-log golden hash (`sim_14`) unchanged |
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

- **Hit journal** (`hit_journal.rs`) remains JSONL with the legacy 1h/4h/24h
  gross-only backfill — retained for backward compatibility; NEW research
  artifacts (observations/outcomes/evaluations) are Parquet with identity,
  quality, direction, and net returns. Migration is a follow-up, not part of
  this spec.
- The backtester's observation marks are event-sampled (trades/marks/mid), so
  MFE/MAE are sampled excursions, not continuous-path excursions. Deterministic
  and honest, but documented as an approximation.
- Bar-based market profile is an approximation (REL-20); trade-level footprint
  requires full L2 and is out of scope.
- Bootstrap CIs / FDR correction and multi-regime taxonomy: out of scope
  (deferred).
- The sim path emits observations only when recording is ENABLED; the live
  feed path (paper mode) can use the same `ObservationRecorder` via the paper
  session — wiring is the same write-only pattern (follow-up to expose the
  flag through `sim --paper`).