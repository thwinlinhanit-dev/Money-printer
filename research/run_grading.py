"""Executable weekly screener-grading job (RES-2).

The caller supplies a fully materialized JSON input bundle.  This intentionally
does not synthesize prices or hits: unavailable market data is a failed job,
not an optimistic grade.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from grading import Hit
from grading_job import run_weekly_grading


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="grade a week of screener hits")
    parser.add_argument("--input", required=True, type=Path, help="grading JSON bundle")
    parser.add_argument("--out-dir", type=Path, default=Path("research/grades"))
    args = parser.parse_args(argv)
    try:
        bundle = json.loads(args.input.read_text(encoding="utf-8"))
        week = str(bundle["week"])
        horizon_ns = int(bundle["horizon_ns"])
        hits = [
            Hit(str(hit["rule"]), str(hit["symbol"]), int(hit["ts_ns"]))
            for hit in bundle["hits"]
        ]
        prices = {
            str(symbol): [(int(ts), float(price)) for ts, price in series]
            for symbol, series in bundle["prices"].items()
        }
        baseline = {
            str(symbol): float(value) for symbol, value in bundle["baseline"].items()
        }
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
        print(f"P3: grading input unavailable or invalid: {error}", file=sys.stderr)
        return 2
    path, ran = run_weekly_grading(
        week, hits, prices, horizon_ns, baseline, args.out_dir
    )
    print(
        json.dumps(
            {
                "path": str(path),
                "report": str(path.with_suffix(".md")),
                "ran": ran,
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":  # pragma: no cover - CLI boundary
    raise SystemExit(main())
