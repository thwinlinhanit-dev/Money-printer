# research/ — the intelligence layer (spec 010)

Python 3.12 + Polars/DuckDB over the cold + feature stores (CONV-2:
research-only; **no Python on any live decision path**). This turns recorded
data and journals into *understanding*: screener grading, event studies, and
the LLM agents that draft briefs grounded in this system's own data.

## Layout

```
mp_data/         dataset/feature/manifest access with coverage checks (RES-1)
grading.py       screener grading (RES-2) + edge-decay detection (RES-3)
event_study.py   CAR harness with seeded bootstrap CIs + regime slicing (RES-4)
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
