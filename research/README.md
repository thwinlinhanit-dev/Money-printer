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
prompts/         versioned LLM prompt templates (RES-8); *-v1 header bumps on change
grades/          weekly grading outputs (journaled, append-only)
tests/           pytest fixtures with hand-verified numbers
```

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
the daily-brief prompt template. Deferred (compose these pieces): the scheduled
brief/anomaly/report *jobs* and the `mp_data` Polars/DuckDB readers — see spec
010 Decisions.
