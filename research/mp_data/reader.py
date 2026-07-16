"""Dataset access wrappers (RES-1): notebooks never hand-roll paths, and no
read silently crosses a recording gap.

Layout mirrors the Rust cold store (spec 003, `storage/src/layout.rs`):
  {root}/trades/venue={v}/symbol={s}/date={d}/part-000.parquet
  {root}/manifests/venue={v}/date={d}.json          (quality manifests, STO-2)

``events()`` consults the manifest coverage BEFORE returning a frame:
``refuse`` raises on any gap (scheduled jobs), ``warn`` returns the gaps for
the caller to log (interactive research). Same semantics as SIM-6.
"""

from __future__ import annotations

import json
from pathlib import Path

import polars as pl

from .coverage import Coverage, CoverageGap, Interval


def _manifest_path(root: Path, venue: str, date: str) -> Path:
    return root / "manifests" / f"venue={venue}" / f"date={date}.json"


def _trades_path(root: Path, venue: str, symbol: str, date: str) -> Path:
    return (
        root
        / "trades"
        / f"venue={venue}"
        / f"symbol={symbol}"
        / f"date={date}"
        / "part-000.parquet"
    )


def coverage(root: Path | str, venue: str, date: str, stream: str) -> Coverage | None:
    """Covered intervals for one stream from its quality manifest, or ``None``
    when no manifest exists (which ``refuse`` mode treats as a gap — an
    unmanifested day is an untrusted day)."""
    p = _manifest_path(Path(root), venue, date)
    if not p.exists():
        return None
    m = json.loads(p.read_text(encoding="utf-8"))
    st = m.get("streams", {}).get(stream)
    if st is None:
        return None
    # Normative field names are day_start_ns / day_end_ns (spec 003 / Rust
    # QualityManifest).  Reject manifests that use the old from_ns/to_ns names
    # loudly so a stale fixture never silently feeds bad numbers.
    if "day_start_ns" not in m or "day_end_ns" not in m:
        raise KeyError(
            f"manifest {p} is missing day_start_ns / day_end_ns "
            f"(found keys: {list(m.keys())}). "
            "Use the Rust compactor to regenerate manifests — old from_ns/to_ns "
            "names are no longer accepted (spec 003 schema contract)."
        )
    day_from = int(m["day_start_ns"])
    day_to = int(m["day_end_ns"])
    covered = [Interval(day_from, day_to)]
    # Subtract each recorded gap from the day window.
    for g in st.get("gaps", []):
        covered = _subtract(covered, Interval(int(g["from_ns"]), int(g["to_ns"])))
    return Coverage(covered)


def _subtract(intervals: list[Interval], gap: Interval) -> list[Interval]:
    out: list[Interval] = []
    for iv in intervals:
        if gap.end <= iv.start or gap.start >= iv.end:
            out.append(iv)
            continue
        if gap.start > iv.start:
            out.append(Interval(iv.start, gap.start))
        if gap.end < iv.end:
            out.append(Interval(gap.end, iv.end))
    return out


def events(
    root: Path | str,
    venue: str,
    symbol: str,
    date: str,
    *,
    mode: str = "warn",
) -> tuple[pl.DataFrame, list[Interval]]:
    """Read one day of trades with the coverage gate applied (RES-1).

    Returns ``(frame, gaps)``. ``refuse`` raises :class:`CoverageGap` on any
    gap or missing manifest; ``warn`` returns the gaps so the caller logs them.
    """
    root = Path(root)
    cov = coverage(root, venue, date, f"trades:{symbol}")
    if cov is None:
        if mode == "refuse":
            raise CoverageGap(f"no manifest for {venue}/{date} — unmanifested data is untrusted")
        gaps: list[Interval] = []
    else:
        # The manifest's own day bounds define the query window.
        first = cov.covered[0].start if cov.covered else 0
        last = cov.covered[-1].end if cov.covered else 0
        gaps = cov.check(first, last, mode=mode)

    path = _trades_path(root, venue, symbol, date)
    if not path.exists():
        if mode == "refuse":
            raise CoverageGap(f"no trades file at {path}")
        return pl.DataFrame(), gaps
    return pl.read_parquet(path), gaps
