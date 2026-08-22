"""CLI for the research idea registry (serious-research-lab roadmap Phase 1.3).

Usage:
    py research/run_registry.py check [--registry <file>]
    py research/run_registry.py seed  [--registry <file>]
    py research/run_registry.py render [--registry <file>]

- ``check``  - one record per discovered candidate (strategies +
  ``docs/BACKLOG.md`` alpha section), valid funnel state, every listed
  ``run_id`` present in ``runs/index.jsonl``; exit 1 when any check fails.
- ``seed``   - scaffold records for candidates that have none (defaults;
  existing records are never overwritten - the ledger is curated, not
  regenerated from backtests).
- ``render`` - markdown table for the weekly review.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import registry

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_REGISTRY = REPO_ROOT / "research" / "registry.jsonl"


def _discover() -> list[registry.Candidate]:
    return registry.discover_strategies(
        REPO_ROOT / "strategies"
    ) + registry.discover_backlog(REPO_ROOT / "docs" / "BACKLOG.md")


def _cmd_check(args) -> int:
    records = registry.load_registry(args.registry)
    candidates = _discover()
    unique = sorted({c.id for c in candidates})
    problems = registry.check(records, candidates, REPO_ROOT / "runs" / "index.jsonl")
    print(f"{len(records)} registry records, {len(unique)} discovered candidates")
    for problem in problems:
        print(f"PROBLEM: {problem}")
    if problems:
        return 1
    print("registry is healthy")
    return 0


def _cmd_seed(args) -> int:
    path = Path(args.registry)
    records = registry.load_registry(path)
    added = 0
    for c in sorted(_discover(), key=lambda c: c.id):
        if c.id not in records:
            records[c.id] = registry.default_record(c)
            added += 1
    if added:
        registry.save_registry(path, list(records.values()))
        print(f"seeded {added} new records into {path}")
    else:
        print("registry already covers every candidate (nothing added)")
    return 0


def _cmd_render(args) -> int:
    print(registry.render(registry.load_registry(args.registry)), end="")
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="research idea registry (Phase 1.3)")
    p.add_argument("--registry", default=str(DEFAULT_REGISTRY))
    sub = p.add_subparsers(dest="command", required=True)
    sub.add_parser("check").set_defaults(fn=_cmd_check)
    sub.add_parser("seed").set_defaults(fn=_cmd_seed)
    sub.add_parser("render").set_defaults(fn=_cmd_render)
    args = p.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
