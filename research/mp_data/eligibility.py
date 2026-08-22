"""Data-eligibility selection over the scorecard archive (spec 024 / RES-1;
research-lab roadmap Phases 2.3-2.4).

A research run may only consume days that pass the daily integrity gate, and the
run RECORD must list every excluded day and *why* - a green collector status is
not enough if a study becomes biased by exclusions (roadmap 2.4). This module
reads the scorecard archive (``data/scorecards/{date}.json``) into
per-(venue, symbol) date series, applies an :class:`EligibilityRule`, and
produces an :class:`EligibilityReport` whose ``summary()`` / ``embed()`` give a
small JSON-serializable blob a run can append to ``runs/index.jsonl``.

Pure stdlib (no Polars/DuckDB) so it is deterministic and unit-testable, in the
same style as ``coverage.py``. Research-only - never on a live decision path
(CONV-2).

Honesty rule (spec 024, roadmap 2.3): a day with no scorecard is UNKNOWN, and
an unknown day is treated as excluded with reason "no scorecard" - never
silently admitted. A corrupt/unparseable scorecard file is likewise treated as
no grade (a trust failure), never a guess.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path

_DATE_RE = re.compile(r"^(?P<date>\d{4}-\d{2}-\d{2})\.json$")
# Files present in data/scorecards that are not per-day gate scores.
_NON_DAY = {".scorecard_sources.json", "backfill.log", "pipeline.log"}
_IGNORE_SUFFIXES = ("_bursts.json", ".determinism.json")


@dataclass(frozen=True)
class Grade:
    """One (venue, symbol, date) quality record from the daily integrity gate."""

    venue: str
    symbol: str
    date: str
    clean: bool
    coverage: float
    event_count: int = 0
    findings: int = 0
    blocking_findings: int = 0
    worst_gap_ns: int | None = None
    stale_bursts: int | None = None


@dataclass(frozen=True)
class EligibilityRule:
    """Quality thresholds a day must pass to be selected.

    Defaults are the Phase-0 promotion bar (ROADMAP / spec 024): coverage >=
    0.995, gate-clean, and no stale bursts in the qualifying window.
    ``max_worst_gap_ns`` is ``None`` by default = not enforced; set it to a
    nanosecond budget (e.g. a few minutes) to also reject long single gaps.
    """

    min_coverage: float = 0.995
    require_clean: bool = True
    max_stale_bursts: int = 0
    max_worst_gap_ns: int | None = None

    def reasons(self, g: Grade) -> list[str]:
        """Why this day fails the rule; empty means eligible."""
        out: list[str] = []
        if g.coverage < self.min_coverage:
            out.append(f"coverage {g.coverage:.4f} < {self.min_coverage:.4f}")
        if self.require_clean and not g.clean:
            out.append(f"clean=false (blocking_findings={g.blocking_findings})")
        if (
            self.max_stale_bursts >= 0
            and g.stale_bursts
            and g.stale_bursts > self.max_stale_bursts
        ):
            out.append(f"stale_bursts={g.stale_bursts} > {self.max_stale_bursts}")
        if (
            self.max_worst_gap_ns is not None
            and g.worst_gap_ns
            and g.worst_gap_ns > self.max_worst_gap_ns
        ):
            out.append(f"worst_gap_ns={g.worst_gap_ns} > {self.max_worst_gap_ns}")
        return out


@dataclass(frozen=True)
class Exclusion:
    """A (venue, symbol, date) that did NOT qualify, and why."""

    venue: str
    symbol: str
    date: str
    reasons: tuple[str, ...]
    present: bool  # False = no scorecard existed (ungraded/blind day)

    def as_dict(self) -> dict:
        return {
            "venue": self.venue,
            "symbol": self.symbol,
            "date": self.date,
            "reasons": list(self.reasons),
            "present": self.present,
        }


@dataclass(frozen=True)
class EligibilityReport:
    rule: EligibilityRule
    universe: tuple[tuple[str, str], ...]
    dates: tuple[str, ...]
    eligible: dict[tuple[str, str], list[Grade]] = field(default_factory=dict)
    excluded: list[Exclusion] = field(default_factory=list)

    def eligible_count(self) -> int:
        return sum(len(g) for g in self.eligible.values())

    def excluded_count(self) -> int:
        return len(self.excluded)

    def summary(self) -> dict:
        """Compact, JSON-serializable summary for a run record or report."""
        missing = [e.as_dict() for e in self.excluded if not e.present]
        return {
            "rule": {
                "min_coverage": self.rule.min_coverage,
                "require_clean": self.rule.require_clean,
                "max_stale_bursts": self.rule.max_stale_bursts,
                "max_worst_gap_ns": self.rule.max_worst_gap_ns,
            },
            "universe": [f"{v}:{s}" for v, s in self.universe],
            "date_range": (self.dates[0], self.dates[-1]) if self.dates else None,
            "eligible_days": self.eligible_count(),
            "excluded_days": self.excluded_count(),
            "excluded": [e.as_dict() for e in self.excluded],
            "missing_scorecard_days": missing,
        }

    def embed(
        self,
        *,
        run_id: str,
        git_sha: str | None = None,
        source: str = "data/scorecards",
    ) -> dict:
        """Fragments for a ``runs/index.jsonl`` record: the run's data
        eligibility gate, with every excluded day and why (roadmap 2.3)."""
        s = self.summary()
        return {
            "run_id": run_id,
            "git_sha": git_sha,
            "data_eligibility_source": source,
            "eligible_days": s["eligible_days"],
            "excluded_days": s["excluded_days"],
            "excluded": s["excluded"],
            "missing_scorecard_days": s["missing_scorecard_days"],
        }


def _opt_int(v) -> int | None:
    if v is None or v == "":
        return None
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def load_scorecards(
    scorecards_dir: Path | str,
) -> dict[tuple[str, str], dict[str, Grade]]:
    """Parse every ``{date}.json`` in the archive into per-(venue,symbol) series.

    Tolerates the older scorecard shapes (pre-``worst_gap_ns``/``stale_bursts``,
    e.g. 2026-07-18) by defaulting missing fields. A corrupt file is skipped so
    the caller sees "no grade" for that day (a trust failure, not a guess).
    """
    d = Path(scorecards_dir)
    out: dict[tuple[str, str], dict[str, Grade]] = {}
    if not d.is_dir():
        return out
    for p in sorted(d.iterdir()):
        name = p.name
        if (
            p.is_dir()
            or name in _NON_DAY
            or name.startswith(".")
            or name.endswith(_IGNORE_SUFFIXES)
        ):
            continue
        m = _DATE_RE.match(name)
        if m is None:
            continue
        date = m.group("date")
        try:
            doc = json.loads(p.read_text(encoding="utf-8-sig"))
        except (OSError, json.JSONDecodeError):
            # Corrupt scorecard = ungraded day; do not fabricate a grade.
            continue
        for r in doc.get("recordings", []):
            venue = r.get("venue", "")
            symbol = r.get("symbol", "")
            if not venue or not symbol:
                continue
            g = Grade(
                venue=venue,
                symbol=symbol,
                date=date,
                clean=bool(r.get("clean", False)),
                coverage=float(r.get("coverage", 0.0) or 0.0),
                event_count=int(r.get("event_count", 0) or 0),
                findings=int(r.get("findings", 0) or 0),
                blocking_findings=int(r.get("blocking_findings", 0) or 0),
                worst_gap_ns=_opt_int(r.get("worst_gap_ns")),
                stale_bursts=_opt_int(r.get("stale_bursts")),
            )
            out.setdefault((venue, symbol), {})[date] = g
    return out


def all_universe(
    scorecards: dict[tuple[str, str], dict[str, Grade]],
) -> tuple[tuple[str, str], ...]:
    """Every (venue, symbol) present in the archive, sorted."""
    return tuple(sorted(scorecards))


def all_dates(scorecards: dict[tuple[str, str], dict[str, Grade]]) -> tuple[str, ...]:
    """Sorted union of every graded date across all series."""
    dts: set[str] = set()
    for series in scorecards.values():
        dts.update(series)
    return tuple(sorted(dts))


def select(
    scorecards: dict[tuple[str, str], dict[str, Grade]],
    universe: list[tuple[str, str]] | tuple[tuple[str, str], ...],
    dates: list[str] | tuple[str, ...],
    rule: EligibilityRule = EligibilityRule(),
) -> EligibilityReport:
    """Classify every (venue, symbol, date) in the window as eligible/excluded.

    A date with no grade for a required series is excluded with reason
    "no scorecard" (an ungraded day is an untrusted day, spec 024).
    """
    eligible: dict[tuple[str, str], list[Grade]] = {}
    excluded: list[Exclusion] = []
    for key in universe:
        series = scorecards.get(key, {})
        for date in dates:
            day = series.get(date)
            if day is None:
                excluded.append(
                    Exclusion(
                        key[0],
                        key[1],
                        date,
                        ("no scorecard for date (ungraded/blind)",),
                        False,
                    )
                )
                continue
            reasons = rule.reasons(day)
            if reasons:
                excluded.append(Exclusion(key[0], key[1], date, tuple(reasons), True))
            else:
                eligible.setdefault(key, []).append(day)
    return EligibilityReport(
        rule=rule,
        universe=tuple(universe),
        dates=tuple(dates),
        eligible=eligible,
        excluded=excluded,
    )
