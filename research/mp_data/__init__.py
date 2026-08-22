"""``mp_data`` — the research data access layer (spec 010, RES-1).

Wraps the Dataset/feature/manifest layout so notebooks and scheduled jobs never
hand-roll paths and never read across recording gaps silently. The heavy
readers (Polars/DuckDB over the cold + feature stores) live behind
``events()`` / ``features()`` and are optional at import time; the coverage
logic they rely on is pure stdlib and always importable.

No Python on any live decision path (CONV-2): this package is research-only.
"""

from __future__ import annotations

from .coverage import Coverage, CoverageGap, Interval
from .eligibility import (
    EligibilityReport,
    EligibilityRule,
    Exclusion,
    Grade,
    all_dates,
    all_universe,
    load_scorecards,
    select,
)

__all__ = [
    "Coverage",
    "CoverageGap",
    "Interval",
    "Grade",
    "EligibilityRule",
    "EligibilityReport",
    "Exclusion",
    "load_scorecards",
    "select",
    "all_universe",
    "all_dates",
]
