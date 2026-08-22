"""CLI for strategy autopsies (roadmap Phase 4.5).

Usage:
    py research/run_autopsy.py <candidate> [--registry <file>] [--runs <file>]
        [--out-dir research/autopsies]

Generates ``autopsies/<candidate>-<date>.md`` from the candidate's journaled
run records (via the registry's run_ids) and appends the autopsy path to the
candidate's registry ``evidence`` list. Refuses to overwrite an existing
autopsy file for the same day (append-only); exits 2 when the candidate or
any journaled run_id is missing.
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

import autopsy

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_REGISTRY = REPO_ROOT / "research" / "registry.jsonl"
DEFAULT_RUNS = REPO_ROOT / "runs" / "index.jsonl"
DEFAULT_OUT = REPO_ROOT / "research" / "autopsies"


def _fail2(message: str) -> None:
    print(f"exit 2: {message}", file=sys.stderr)
    raise SystemExit(2)


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="strategy autopsy (4.5)")
    p.add_argument("candidate")
    p.add_argument("--registry", default=str(DEFAULT_REGISTRY))
    p.add_argument("--runs", default=str(DEFAULT_RUNS))
    p.add_argument("--out-dir", default=str(DEFAULT_OUT))
    args = p.parse_args(argv)

    try:
        records = autopsy.collect(args.candidate, args.registry, args.runs)
    except ValueError as exc:
        _fail2(str(exc))
    verdict = autopsy.classify(records)

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    path = out_dir / f"{args.candidate}-{stamp}.md"
    if path.exists():
        _fail2(f"autopsy already exists: {path} (append-only, re-run refused)")

    report = autopsy.render(args.candidate, records, verdict)
    path.write_text(report, encoding="utf-8")

    recs = autopsy.load_registry(args.registry)
    rec = recs[args.candidate]
    evidence = list(rec.get("evidence", []))
    link = f"research/autopsies/{path.name}"
    if link not in evidence:
        rec["evidence"] = evidence + [link]
        lines = [json.dumps(recs[k], sort_keys=True) + "\n" for k in sorted(recs)]
        Path(args.registry).write_text("".join(lines), encoding="utf-8")

    print(f"wrote {path}")
    print(verdict["class"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
