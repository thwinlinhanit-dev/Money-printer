"""CLI for the point-in-time instrument master (roadmap Phase 3.3).

Usage:
    py research/run_instrument_master.py resolve --venue <v> --symbol <s> --at <iso>
    py research/run_instrument_master.py check [--raw <dir>]
    py research/run_instrument_master.py render

- ``resolve``  - prints the instrument definition in force at the timestamp;
  exit 2 (fail-closed) when the symbol does not resolve.
- ``check``    - every market log in the raw corpus resolves to a definition
  at its day start; ANY unresolved file exits 2 (a run must block on
  unresolved metadata), problems listed otherwise.
- ``render``   - markdown table of every definition and its observed window.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import instrument_master

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MASTER = REPO_ROOT / "research" / "instrument_master.jsonl"
DEFAULT_RAW = REPO_ROOT / "data" / "raw"


def _fail2(message: str) -> None:
    print(f"exit 2: {message}", file=sys.stderr)
    raise SystemExit(2)


def _cmd_resolve(args) -> int:
    master = instrument_master.load_master(args.master)
    try:
        inst = instrument_master.resolve(
            master, args.venue, args.symbol, instrument_master.parse_ts_ns(args.at)
        )
    except instrument_master.InstrumentUnknown as exc:
        _fail2(str(exc))
    print(json.dumps(inst.__dict__, sort_keys=True, indent=2))
    return 0


def _cmd_check(args) -> int:
    master = instrument_master.load_master(args.master)
    logs = instrument_master.corpus_logs(args.raw)
    problems = instrument_master.check(master, logs)
    print(f"{len(logs)} market log files, {len(problems)} unresolved")
    for problem in problems:
        print(f"BLOCKED: {problem}")
    if problems:
        return 2
    print("every corpus symbol resolves to a point-in-time instrument definition")
    return 0


def _cmd_render(args) -> int:
    print(instrument_master.render(instrument_master.load_master(args.master)), end="")
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="point-in-time instrument master (3.3)")
    p.add_argument("--master", default=str(DEFAULT_MASTER))
    sub = p.add_subparsers(dest="command", required=True)
    r = sub.add_parser("resolve")
    r.add_argument("--venue", required=True)
    r.add_argument("--symbol", required=True)
    r.add_argument("--at", required=True, help="ISO-8601 UTC timestamp")
    r.set_defaults(fn=_cmd_resolve)
    c = sub.add_parser("check")
    c.add_argument("--raw", default=str(DEFAULT_RAW))
    c.set_defaults(fn=_cmd_check)
    sub.add_parser("render").set_defaults(fn=_cmd_render)
    args = p.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
