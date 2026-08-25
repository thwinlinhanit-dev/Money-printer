# research/ — the intelligence layer (spec 010)

Python 3.12 + Polars/DuckDB over the cold + feature stores (CONV-2:
research-only; **no Python on any live decision path**). This turns recorded
data and journals into *understanding*: screener grading, event studies, and
the LLM agents that draft briefs grounded in this system's own data.

## Layout

```
mp_data/         dataset/feature/manifest access with coverage checks (RES-1) + `eligibility.py`: scorecard-grade selection (research-lab roadmap Phases 2.3–2.4)
run_eligibility.py  CLI for the eligibility gate: prints eligible days + every excluded day and why; optional append-only run record (exit 2 fail-closed when nothing is eligible)
registry.py      research idea registry (roadmap Phase 1.3): one record per candidate, funnel-state enum, run_id agreement with runs/index.jsonl
run_registry.py  registry CLI: check / seed / render (check exits 1 on any problem)
registry.jsonl   the ledger (14 records seeded 2026-08-19; states/reasons curated, evidence-linked)
feasibility.py   economic feasibility gate (roadmap Phase 4.1): break-even holding period + min move from actual legs, base + stressed; funding needs a settlement cadence
run_feasibility.py  feasibility CLI: computes + journals a kind=feasibility run record (E7) and updates the registry record
panel.py         historical panel (roadmap 3.1): universe + manifest + membership from data.binance.vision daily klines; fidelity label `external_archive:binance-um-klines-1m-v1`
panel_universe.json  versioned panel universe (binance-um-v1: BTCUSDT/ETHUSDT/SOLUSDT, 2026-01-01..08-18, train/validation/test partitions)
run_panel.py     panel CLI: download (idempotent, missing-on-source recorded), check (coverage gaps), verify (seeded byte-identical rebuild)
panel/           panel manifest ledger (append-only, per-universe)
instrument_master.py  point-in-time instrument master (roadmap 3.3): versioned resolves, fail-closed on unresolved symbols; corpus filename regex + census log skip rule
instrument_master.jsonl  the master (9 instruments, timestamp-corrected; last-match-wins by observed_from_ns)
run_instrument_master.py  instrument-master CLI: resolve / check / render (check exits 1 on any unresolved symbol)
autopsy.py       strategy autopsy (roadmap 4.5): P&L by day, cost share, classification (economic failure / data limitation / execution-model failure); zero-trade runs are absence of evidence, never data
run_autopsy.py   autopsy CLI: writes `autopsies/<id>-<date>.md` append-only + links it in the registry evidence (exit 2 on duplicate day or missing run_id)
autopsies/       generated autopsy reports (append-only)
run_weekly_review.py  weekly research review (roadmap 1.2): ISO-week windowing of journaled runs, registry table, autopsies, benchmark grounded on docs/OWNER_POLICY.md §3 (fail-closed `unset` when absent); append-only per week
reviews/         weekly review reports
grading.py       screener grading (RES-2) + edge-decay detection (RES-3); spec 017 recommendations
grading_job.py   weekly grading job — emits machine `{week}.json` + human `{week}.md` with "Next stage recommendations" (spec 017 GRD-3/5)
event_study.py   CAR harness with seeded bootstrap CIs + regime slicing (RES-4); `n_days`/`ci_reliable` disclosure
run_backlog_event_studies.py  backlog-idea event studies (RES-4): same-UTC-day session gating on oi-purge, append-only `--run-id` rejection
band_accuracy.py RES-4 whale band-accuracy parsing + weekly trend math
band_accuracy_job.py  weekly band-accuracy job (shells out to `whale_study`)
run_band_accuracy.py  CLI for the above (exit 2 on unavailable data)
calibrate_leverage.py  LIQ-11 leverage-tier calibration parse + TOML render
calibrate_leverage_job.py  calibration job (shells out to --leverage-calibration)
run_calibrate_leverage.py  CLI: --print-toml emits the [liq_est_bands] section
prompts/         versioned LLM prompt templates (RES-8); *-v1 header bumps on change
grades/          weekly grading outputs (journaled, append-only)
band_accuracy/   weekly band-accuracy ledger (`{week}.json` + `band_accuracy.jsonl`)
tests/           pytest fixtures with hand-verified numbers
```

The RES-4 event-study family has two shapes: the CAR harness above (forward
returns around events) and the **band-accuracy study** (spec 029 LIQ-6) that
grades the estimated `liq.est_bands` against spec 028 Hyperliquid real liq
prices. The latter is the Rust binary `mp-features` ships as `whale_study`
(`cargo run -p mp-features --bin whale_study -- --log <hl.log> --log
<positions.log> --json`); with `--run-id` + `--runs-dir` it journals a
SIM-10-style run record into `runs/index.jsonl` (append-only, RES-4), the same
tracker contract the sim backtester uses.

The Python side turns those run records into a **weekly band-accuracy trend**:
`run_band_accuracy.py --log <hl.log> --log <positions.log> [--config
<features.toml>] [--week YYYY-Www]` shells out to `whale_study --json` (one
run per invocation, journaled to the SIM-10 tracker), parses the report
fail-closed (`band_accuracy.py`, pure stdlib), and writes the idempotent
weekly ledger `band_accuracy/{week}.json` + the append-only
`band_accuracy/band_accuracy.jsonl` trend (W-6, RES-2 pattern), consumed by
ops: the monthly report grounds its RES-4 section on the journal (OPS-6) and
watches it for drift/decay (OPS-13, `band-accuracy-decay` P3). With no
`--week` the bucket is derived from the run's data range; with `--week` an
already-graded week short-circuits before any shell-out.

The **leverage-tier calibration** (spec 029 LIQ-11) closes the documented
assumption behind `liq.est_bands`: `run_calibrate_leverage.py --log
<positions.log> [--print-toml]` shells out to `whale_study
--leverage-calibration` (recorded spec 028 real leverage, notional-weighted
onto the configured tier set), and either prints the `[liq_est_bands]` TOML
section to paste into `features.toml` or a JSON summary. Runs are journaled
append-only (`calibrations.jsonl` + a SIM-10 record in `runs/index.jsonl`)
and fail closed (exit 2) on an empty census or weights that don't sum to ≈1.


The LLM *providers* live in the Rust `mp-llm` crate (nine providers, grounding
contract in code); the research jobs here compose them. Grounding is normative:
every brief archives its input-bundle hash + prompt version + model id + output
under `journal/briefs/` (RES-6), and LLM output is human-read only (RES-7).

## Running the tests

```sh
cd research && python3 -m pytest -q
```

The grading/study math is pure stdlib and deterministic, so the tests run
without Polars/DuckDB installed. Production readers add those for the heavy
frames; the coverage/grading/CAR logic they call is what the tests pin.

## Status

Implemented: coverage checks, grading + decay math, event-study CAR + CIs,
the daily-brief prompt template, and the whale band-accuracy study
(`whale_study`, RES-4/LIQ-6) with SIM-10 run-record journaling. Deferred
(compose these pieces): the scheduled brief/anomaly/report *jobs* and the
`mp_data` Polars/DuckDB readers — see spec 010 Decisions.
