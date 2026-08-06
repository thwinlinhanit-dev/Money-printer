"""Executable weekly band-accuracy job (RES-4, spec 029 LIQ-6).

Shells out to the ``whale_study`` binary (``mp-features``) over recorded
Hyperliquid raw logs and journals the run record + the weekly trend row.
Mirrors ``run_grading.py``: unavailable market data is a failed job (exit 2),
not an optimistic row. The week defaults to the run's data range; pass
``--week`` to pin it (and to short-circuit a re-fired week before any
shell-out).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from band_accuracy_job import BandJobError, run_weekly_band_accuracy


def default_binary() -> Path:
    """The repo's debug build of ``whale_study`` (Windows: ``.exe``)."""
    root = Path(__file__).resolve().parent.parent
    for name in ("whale_study.exe", "whale_study"):
        candidate = root / "target" / "debug" / name
        if candidate.exists():
            return candidate
    return root / "target" / "debug" / "whale_study"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="grade a week of recorded liq data (RES-4)")
    parser.add_argument("--log", action="append", required=True, type=Path, help="mp raw event log (repeatable)")
    parser.add_argument("--config", type=Path, help="features.toml with liq.est_bands (defaults if omitted)")
    parser.add_argument("--out-dir", type=Path, default=Path("research/band_accuracy"))
    parser.add_argument("--week", help="ISO week (YYYY-Www); defaults to the run's data range")
    parser.add_argument("--git-sha", help="commit under which the logs were recorded")
    parser.add_argument("--run-id", help="ULID run id (defaults to a fresh ULID)")
    parser.add_argument("--whale-study", type=Path, default=None, help="path to the whale_study binary")
    parser.add_argument(
        "--runs-dir",
        type=Path,
        default=None,
        help="where whale_study journals its SIM-10 record (default <out-dir>/runs)",
    )
    args = parser.parse_args(argv)

    binary = args.whale_study or default_binary()
    try:
        path, ran, payload = run_weekly_band_accuracy(
            args.log,
            args.out_dir,
            binary,
            week=args.week,
            config=args.config,
            git_sha=args.git_sha,
            run_id=args.run_id,
            runs_dir=args.runs_dir,
        )
    except BandJobError as error:
        print(f"P4: band-accuracy inputs unavailable or invalid: {error}", file=sys.stderr)
        return 2
    week = payload["week"] if payload else args.week or "?"
    print(json.dumps({"path": str(path), "week": week, "ran": ran}, sort_keys=True))
    return 0


if __name__ == "__main__":  # pragma: no cover - CLI boundary
    raise SystemExit(main())
