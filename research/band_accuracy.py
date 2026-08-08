"""Band-accuracy parsing + weekly trend (RES-4, spec 029 LIQ-6).

Consumes the ``whale_study --json`` report (the Rust binary that grades the
estimated ``liq.est_bands`` against spec 028 Hyperliquid real liq prices) and
rolls individual runs into a per-week trend. Pure stdlib and deterministic —
the math is unit-tested on hand-verified fixtures (same contract as
``grading.py`` for RES-2).

The weekly bucket is derived from the run's *data* range (``data_from_ns``),
not the wall clock, so historical logs bucket by when the market data was
recorded. Parsing is fail-closed: a malformed report raises ``ValueError``
rather than silently feeding bad numbers into the trend.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any


@dataclass(frozen=True)
class SideMetrics:
    """One side's band-accuracy summary (mirrors the Rust report keys)."""

    n: int
    mean_relative_error: float
    coverage: float


@dataclass(frozen=True)
class BandRun:
    """One parsed ``whale_study --json`` report (a RES-4 run record)."""

    run_id: str | None
    git_sha: str | None
    config_hash: str | None
    data_from_ns: int
    data_to_ns: int
    observations: int
    long: SideMetrics
    short: SideMetrics
    total: SideMetrics
    events: dict[str, int]

    @property
    def week(self) -> str:
        """ISO year-week (UTC) of the run's first data sample: ``YYYY-Www``."""
        return week_from_ns(self.data_from_ns)


@dataclass(frozen=True)
class WeekTrend:
    """One trend row: a week's runs merged side-by-side, weighted by ``n``."""

    week: str
    n_runs: int
    observations: int
    long: SideMetrics
    short: SideMetrics
    total: SideMetrics


def parse_report(obj: Any) -> BandRun:
    """Validate and parse a ``whale_study --json`` report dict.

    Fail-closed (CONV-8): the metrics keys must exist and be well-typed, or a
    ``ValueError`` is raised — a truncated/corrupt report must never feed the
    trend. Provenance fields (``run_id``/``git_sha``/``config_hash``/events)
    are echoes and optional (the report omits them when not journaled).
    """
    if not isinstance(obj, dict):
        raise ValueError(f"whale_study report is not an object: {type(obj).__name__}")
    if obj.get("study") != "whale_study":
        raise ValueError(f"not a whale_study report: study={obj.get('study')!r}")

    def side(key: str) -> SideMetrics:
        s = obj.get(key)
        if not isinstance(s, dict):
            raise ValueError(f"report missing {key!r} metrics")
        try:
            return SideMetrics(
                n=_as_int(s["n"], f"{key}.n"),
                mean_relative_error=_as_float(
                    s["mean_relative_error"], f"{key}.mean_relative_error"
                ),
                coverage=_as_float(s["coverage"], f"{key}.coverage"),
            )
        except KeyError as e:
            raise ValueError(f"report {key!r} missing {e.args[0]}") from None

    observations = _as_int(obj.get("observations", 0), "observations")
    data_from = _as_int(obj.get("data_from_ns", 0), "data_from_ns")
    data_to = _as_int(obj.get("data_to_ns", 0), "data_to_ns")
    events = obj.get("events")
    if events is not None and not isinstance(events, dict):
        raise ValueError("report 'events' is not an object")

    return BandRun(
        run_id=_as_optional_str(obj.get("run_id")),
        git_sha=_as_optional_str(obj.get("git_sha")),
        config_hash=_as_optional_str(obj.get("config_hash")),
        data_from_ns=data_from,
        data_to_ns=data_to,
        observations=observations,
        long=side("long"),
        short=side("short"),
        total=side("total"),
        events={str(k): _as_int(v, f"events.{k}") for k, v in events.items()}
        if events
        else {},
    )


def week_from_ns(ts_ns: int) -> str:
    """ISO year-week of ``ts_ns`` in UTC: ``2026-W31`` (sortable by week)."""
    dt = datetime.fromtimestamp(ts_ns / 1_000_000_000, tz=timezone.utc)
    iso = dt.isocalendar()
    return f"{iso.year:04d}-W{iso.week:02d}"


def weekly_trend(runs: list[BandRun]) -> list[WeekTrend]:
    """Merge runs into one row per ISO week, ordered chronologically.

    Runs sharing a week (e.g. several collector logs graded separately) merge
    with ``n``-weighted MRE/coverage and summed observation counts — a week
    with more real liq prices weighs more. Deterministic iteration order
    (sorted by week, runs in input order within a week).
    """
    by_week: dict[str, list[BandRun]] = {}
    for run in runs:
        by_week.setdefault(run.week, []).append(run)
    out = []
    for week in sorted(by_week):
        merged = by_week[week]
        long = _merge_sides([r.long for r in merged])
        short = _merge_sides([r.short for r in merged])
        total = _merge_sides([r.total for r in merged])
        out.append(
            WeekTrend(
                week=week,
                n_runs=len(merged),
                observations=sum(r.observations for r in merged),
                long=long,
                short=short,
                total=total,
            )
        )
    return out


def _merge_sides(sides: list[SideMetrics]) -> SideMetrics:
    """``n``-weighted merge: sum ``n``, weight MRE/coverage by each run's ``n``."""
    n = sum(s.n for s in sides)
    if n == 0:
        return SideMetrics(0, 0.0, 0.0)
    mre = sum(s.mean_relative_error * s.n for s in sides) / n
    cov = sum(s.coverage * s.n for s in sides) / n
    return SideMetrics(n, mre, cov)


def _as_int(value: Any, key: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"report {key!r} is not an int: {value!r}")
    return value


def _as_float(value: Any, key: str) -> float:
    if isinstance(value, bool):
        raise ValueError(f"report {key!r} is not a number: {value!r}")
    if not isinstance(value, (int, float)):
        raise ValueError(f"report {key!r} is not a number: {value!r}")
    return float(value)


def _as_optional_str(value: Any) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise ValueError(f"report provenance field is not a string: {value!r}")
    return value
