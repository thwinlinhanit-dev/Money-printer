# research

## Purpose

Python research package for offline analysis: daily brief generation via LLM, strategy grading/screening, event studies, market coverage analysis, and data reader utilities.

## Ownership

- `brief.py` — daily brief generation pipeline
- `grading.py` — strategy grading logic
- `grading_job.py` — automated grading job
- `event_study.py` — event study framework
- `band_accuracy.py` — RES-4 whale band-accuracy parsing + weekly trend math
- `band_accuracy_job.py` — weekly band-accuracy job (shells out to `whale_study`, idempotent)
- `calibrate_leverage.py` — LIQ-11 leverage-tier calibration parse + TOML rendering
- `calibrate_leverage_job.py` — calibration job (shells out to `whale_study --leverage-calibration`)
- `archive_data.py` — data archiving
- `coverage.py` (in `mp_data/`) — market coverage analysis
- `reader.py` (in `mp_data/`) — event log reader
- `run_brief.py`, `run_grading.py`, `run_band_accuracy.py`, `run_calibrate_leverage.py` — CLI entry points
- `conftest.py`, `tests/` — pytest test suite
- `prompts/daily-brief.md` — LLM prompt template

## Local Contracts

- Python 3.12+ with pandas, pyarrow
- Research output feeds into `runs/index.jsonl`
- Grading results consumed by ops report pipeline

## Verification

- `cd research && python -m pytest`

## Child DOX Index

None.
