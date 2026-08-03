# research

## Purpose

Python research package for offline analysis: daily brief generation via LLM, strategy grading/screening, event studies, market coverage analysis, and data reader utilities.

## Ownership

- `brief.py` — daily brief generation pipeline
- `grading.py` — strategy grading logic
- `grading_job.py` — automated grading job
- `event_study.py` — event study framework
- `archive_data.py` — data archiving
- `coverage.py` (in `mp_data/`) — market coverage analysis
- `reader.py` (in `mp_data/`) — event log reader
- `run_brief.py`, `run_grading.py` — CLI entry points
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
