"""Data-eligibility CLI (research-lab roadmap 2.3-2.4, spec 024 / RES-1).

Prints which (venue, symbol, date) days are eligible for a research run and
which are excluded and *why*, from the scorecard archive. With ``--run-id`` it
also appends the eligibility summary (with every excluded day + reason) to
``runs/index.jsonl`` so a study's record lists its data gate, reproducible from
the same scorecards (roadmap 2.4 exit criterion).

Deterministic (no wall clock); pure stdlib. Fail-closed by default: if the
requested universe has zero eligible days, exit 2 (scheduled jobs never
silently run on data that fails the gate). Pass ``--no-require-eligible`` to
relax that for interactive exploration.

Examples:
  py -3 research/run_eligibility.py --universe hyperliquid:BTC hyperliquid:ETH \\
      --from 2026-08-13 --to 2026-08-17
  py -3 research/run_eligibility.py --universe hyperliquid:BTC \\
      --min-coverage 0.98 --no-require-clean --json
  py -3 research/run_eligibility.py --all --run-id elig-2026-08-17
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mp_data.eligibility import (  # noqa: E402
    EligibilityRule,
    all_dates,
    all_universe,
    load_scorecards,
    select,
)


def _parse_universe(items: list[str] | None) -> list[tuple[str, str]] | None:
    if items is None:
        return None
    pairs: list[tuple[str, str]] = []
    for it in items:
        if ":" not in it:
            raise SystemExit(f"universe entry must be 'venue:symbol', got {it!r}")
        v, s = it.split(":", 1)
        pairs.append((v, s))
    return pairs


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Scorecard data-eligibility selection")
    ap.add_argument("--scorecards-dir", default="data/scorecards")
    ap.add_argument(
        "--universe",
        nargs="+",
        default=None,
        help="venue:symbol pairs; default = every venue/symbol in the archive",
    )
    ap.add_argument(
        "--all", action="store_true", help="select every venue/symbol in the archive"
    )
    ap.add_argument("--from", dest="date_from", default=None)
    ap.add_argument("--to", dest="date_to", default=None)
    ap.add_argument("--min-coverage", type=float, default=0.995)
    ap.add_argument(
        "--require-clean",
        dest="require_clean",
        action=argparse.BooleanOptionalAction,
        default=True,
    )
    ap.add_argument("--max-stale-bursts", type=int, default=0)
    ap.add_argument("--max-worst-gap-ns", type=int, default=None)
    ap.add_argument(
        "--require-eligible",
        dest="require_eligible",
        action=argparse.BooleanOptionalAction,
        default=True,
    )
    ap.add_argument(
        "--run-id",
        default=None,
        help="append an eligibility run record to runs/index.jsonl",
    )
    ap.add_argument("--runs-dir", default="runs")
    ap.add_argument(
        "--json", action="store_true", help="print the JSON summary instead of prose"
    )
    args = ap.parse_args(argv)

    scorecards = load_scorecards(args.scorecards_dir)
    if not scorecards:
        print(
            "error: no scorecards found (is --scorecards-dir correct?)", file=sys.stderr
        )
        return 2

    if args.universe is not None:
        universe = _parse_universe(args.universe)
    else:
        universe = list(all_universe(scorecards))

    dates = all_dates(scorecards)
    if args.date_from:
        dates = tuple(d for d in dates if d >= args.date_from)
    if args.date_to:
        dates = tuple(d for d in dates if d <= args.date_to)
    if not dates:
        print("error: no graded dates in the requested range", file=sys.stderr)
        return 2

    rule = EligibilityRule(
        min_coverage=args.min_coverage,
        require_clean=args.require_clean,
        max_stale_bursts=args.max_stale_bursts,
        max_worst_gap_ns=args.max_worst_gap_ns,
    )
    report = select(scorecards, universe, dates, rule)

    if args.json:
        print(json.dumps(report.summary(), sort_keys=True))
    else:
        print(f"universe: {', '.join(f'{v}:{s}' for v, s in report.universe)}")
        print(
            f"range: {report.dates[0] if report.dates else '-'} .. "
            f"{report.dates[-1] if report.dates else '-'}"
        )
        print(f"eligible days: {report.eligible_count()}")
        print(f"excluded days: {report.excluded_count()}")
        for key, grades in report.eligible.items():
            print(f"  eligible {key[0]}:{key[1]} -> {len(grades)} day(s)")
        for e in report.excluded:
            tag = "missing" if not e.present else "excluded"
            print(f"  {tag} {e.venue}:{e.symbol} {e.date} :: {'; '.join(e.reasons)}")

    if args.run_id:
        if args.json:
            print(
                "note: --run-id is ignored with --json (summary printed, not journaled)",
                file=sys.stderr,
            )
        else:
            runs_dir = Path(args.runs_dir)
            index = runs_dir / "index.jsonl"
            if index.exists():
                existing = {
                    rec.get("run_id")
                    for rec in (
                        json.loads(line)
                        for line in index.read_text(encoding="utf-8").splitlines()
                        if line.strip()
                    )
                    if isinstance(rec, dict) and isinstance(rec.get("run_id"), str)
                }
                if args.run_id in existing:
                    print(
                        f"error: run-id '{args.run_id}' already exists in {index}",
                        file=sys.stderr,
                    )
                    return 2
            record = report.embed(run_id=args.run_id, source=args.scorecards_dir)
            record["kind"] = "data_eligibility"
            runs_dir.mkdir(parents=True, exist_ok=True)
            with index.open("a", encoding="utf-8") as f:
                f.write(json.dumps(record, sort_keys=True) + "\n")
            print(f"journaled -> {index} ({args.run_id})")

    if args.require_eligible and report.eligible_count() == 0:
        print(
            "NO eligible days under this rule (fail-closed) — pass "
            "--no-require-eligible to inspect the exclusions.",
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
