"""Executable leverage-tier calibration job (spec 029 LIQ-11).

Shells out to ``whale_study --leverage-calibration`` over recorded spec 028
positions logs and journals the calibrated ``liq.est_bands`` tier weights.
Mirrors ``run_grading.py``: unavailable market data is a failed job (exit 2),
not an optimistic override. With ``--print-toml`` stdout carries ONLY the
``[liq_est_bands]`` section for pasting into (or replacing the section of)
``features.toml``; by default it prints a JSON summary.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from calibrate_leverage import render_toml
from calibrate_leverage_job import CalibrationError, run_leverage_calibration


def default_binary() -> Path:
    """The repo's debug build of ``whale_study`` (Windows: ``.exe``)."""
    root = Path(__file__).resolve().parent.parent
    for name in ("whale_study.exe", "whale_study"):
        candidate = root / "target" / "debug" / name
        if candidate.exists():
            return candidate
    return root / "target" / "debug" / "whale_study"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="calibrate liq.est_bands leverage-tier weights (RES-4/LIQ-11)"
    )
    parser.add_argument(
        "--log",
        action="append",
        required=True,
        type=Path,
        help="mp raw event log with spec 028 positions (repeatable)",
    )
    parser.add_argument(
        "--config",
        type=Path,
        help="current features.toml (its tier set is calibrated; defaults if omitted)",
    )
    parser.add_argument("--out-dir", type=Path, default=Path("research/calibration"))
    parser.add_argument("--git-sha", help="commit under which the logs were recorded")
    parser.add_argument("--run-id", help="ULID run id (defaults to a fresh ULID)")
    parser.add_argument(
        "--runs-dir",
        type=Path,
        default=None,
        help="where whale_study journals its SIM-10 record (default <out-dir>/runs)",
    )
    parser.add_argument(
        "--whale-study", type=Path, default=None, help="path to the whale_study binary"
    )
    parser.add_argument(
        "--print-toml",
        action="store_true",
        help="print only the [liq_est_bands] TOML section to stdout",
    )
    args = parser.parse_args(argv)

    binary = args.whale_study or default_binary()
    try:
        path, run = run_leverage_calibration(
            args.log,
            args.out_dir,
            binary,
            config=args.config,
            git_sha=args.git_sha,
            run_id=args.run_id,
            runs_dir=args.runs_dir,
        )
    except CalibrationError as error:
        print(
            f"P4: leverage calibration inputs unavailable or invalid: {error}",
            file=sys.stderr,
        )
        return 2

    if args.print_toml:
        print(render_toml(run), end="")
    else:
        print(
            json.dumps(
                {
                    "path": str(path),
                    "run_id": run.run_id,
                    "n": run.n,
                    "positions_seen": run.positions_seen,
                    "total_notional": run.total_notional,
                    "sum_weights": run.sum_weights,
                    "config_hash": run.config_hash,
                },
                sort_keys=True,
            )
        )
    return 0


if __name__ == "__main__":  # pragma: no cover - CLI boundary
    raise SystemExit(main())
