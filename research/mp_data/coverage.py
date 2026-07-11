"""Coverage checks over the dataset/feature/manifest layout (RES-1).

Mirrors the sim's refuse-or-warn semantics (SIM-6): a research query over a
window with recording gaps must never *silently* return partial data. Default
mode is ``warn`` in interactive research and ``refuse`` in scheduled jobs, so a
weekly grading run fails loudly rather than grading a half-empty week.

Pure stdlib: the math here is independent of Polars/DuckDB so it is
deterministic and unit-testable. Production readers (``mp_data.events`` etc.)
consult this before handing back frames.
"""

from __future__ import annotations

from dataclasses import dataclass


class CoverageGap(Exception):
    """Raised in ``refuse`` mode when the requested window has gaps."""


@dataclass(frozen=True)
class Interval:
    """A closed-open nanosecond interval ``[start, end)`` (CONV time units)."""

    start: int
    end: int

    def __post_init__(self) -> None:
        if self.end < self.start:
            raise ValueError(f"interval end {self.end} < start {self.start}")


def _merge(intervals: list[Interval]) -> list[Interval]:
    """Sort and coalesce overlapping/adjacent intervals (deterministic)."""
    if not intervals:
        return []
    ordered = sorted(intervals, key=lambda i: (i.start, i.end))
    merged = [ordered[0]]
    for iv in ordered[1:]:
        last = merged[-1]
        if iv.start <= last.end:  # overlap or touch
            merged[-1] = Interval(last.start, max(last.end, iv.end))
        else:
            merged.append(iv)
    return merged


class Coverage:
    """Known-covered ranges for one dataset, from its quality manifest (STO-4)."""

    def __init__(self, covered: list[Interval]):
        self._covered = _merge(covered)

    @property
    def covered(self) -> list[Interval]:
        return list(self._covered)

    def gaps(self, start: int, end: int) -> list[Interval]:
        """Uncovered sub-intervals of ``[start, end)``, in order."""
        if end < start:
            raise ValueError("query end < start")
        cursor = start
        out: list[Interval] = []
        for iv in self._covered:
            if iv.end <= start or iv.start >= end:
                continue  # outside the query window
            if iv.start > cursor:
                out.append(Interval(cursor, min(iv.start, end)))
            cursor = max(cursor, iv.end)
            if cursor >= end:
                break
        if cursor < end:
            out.append(Interval(cursor, end))
        return out

    def is_complete(self, start: int, end: int) -> bool:
        return not self.gaps(start, end)

    def check(self, start: int, end: int, mode: str = "warn") -> list[Interval]:
        """Verify coverage of ``[start, end)``.

        ``refuse`` raises :class:`CoverageGap` on any gap (scheduled jobs);
        ``warn`` returns the gaps for the caller to log (interactive research).
        Returns the gap list (empty when complete).
        """
        gaps = self.gaps(start, end)
        if gaps and mode == "refuse":
            raise CoverageGap(
                f"{len(gaps)} gap(s) in [{start}, {end}): "
                + ", ".join(f"[{g.start},{g.end})" for g in gaps)
            )
        if mode not in ("warn", "refuse"):
            raise ValueError(f"unknown coverage mode: {mode!r}")
        return gaps
