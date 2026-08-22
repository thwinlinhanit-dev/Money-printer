"""CLI for the Phase 4.1 economic feasibility gate.

Usage:
    py research/run_feasibility.py --spec <file.json> [--run-id <id>]
        [--runs-dir <dir>] [--registry <file>] [--candidate <id>]

Prints the base + stressed economics as JSON. With ``--run-id`` the result is
journaled into ``runs/index.jsonl`` (append-only; a duplicate id is refused,
E7, exit 2). With ``--registry`` + ``--candidate`` the candidate's registry
record is updated with the verdict and cost summary (Phase 1.3 linkage) -
the candidate must exist, exit 2 otherwise.

Spec shape: {"candidate": "<registry id>", "max_hold_days": 30,
"notes": "...", "legs": [{Leg fields}, ...]}. Funding legs must carry their
settlement cadence (realized post-entry cash flows, not annualized peaks).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import NoReturn

import feasibility

REPO_ROOT = Path(__file__).resolve().parents[1]


def _fail2(message: str) -> NoReturn:
    print(f"exit 2: {message}", file=sys.stderr)
    raise SystemExit(2)


def _load_spec(path: str) -> dict:
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        _fail2(f"unreadable spec {path}: {exc}")


def _append_run_record(runs_dir: Path, record: dict) -> None:
    index = runs_dir / "index.jsonl"
    runs_dir.mkdir(parents=True, exist_ok=True)
    existing = set()
    if index.exists():
        for line in index.read_text(encoding="utf-8").splitlines():
            if line.strip():
                existing.add(json.loads(line).get("run_id"))
    if record["run_id"] in existing:
        _fail2(f"duplicate run-id {record['run_id']} refused (E7)")
    with open(index, "a", encoding="utf-8") as f:
        f.write(json.dumps(record, sort_keys=True) + "\n")


def _update_registry(registry_path: Path, candidate: str, res, spec: dict) -> None:
    records = feasibility_registry_load(registry_path)
    rec = records.get(candidate)
    if rec is None:
        _fail2(f"no registry record for candidate '{candidate}'")
    rec["economic_feasibility"] = (
        f"{res.verdict} (break-even {res.breakeven_days:.1f}/{res.breakeven_days_stressed:.1f}"
        f" days vs {res.max_hold_days:.0f}-day horizon)"
    )
    rec["costs"] = (
        f"RT {res.rt_cost_bps:.1f} bps base / {res.rt_cost_bps_stressed:.1f} bps stressed; "
        f"carry {res.carry_bps_per_day:.2f} bps/day ({spec.get('notes', '')})"
    )
    records[candidate] = rec
    lines = [json.dumps(records[k], sort_keys=True) + "\n" for k in sorted(records)]
    registry_path.write_text("".join(lines), encoding="utf-8")


def feasibility_registry_load(registry_path: Path) -> dict[str, dict]:
    records: dict[str, dict] = {}
    if registry_path.exists():
        for line in registry_path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                rec = json.loads(line)
                records[rec["id"]] = rec
    return records


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="Phase 4.1 economic feasibility gate")
    p.add_argument("--spec", required=True, help="feasibility spec JSON file")
    p.add_argument("--run-id", help="journal a run record under this id")
    p.add_argument("--runs-dir", default=str(REPO_ROOT / "runs"))
    p.add_argument("--registry", default=str(REPO_ROOT / "research" / "registry.jsonl"))
    p.add_argument("--candidate", help="registry candidate id to update")
    args = p.parse_args(argv)

    spec = _load_spec(args.spec)
    try:
        legs, max_hold = feasibility.from_spec(spec)
    except ValueError as exc:
        _fail2(str(exc))
    res = feasibility.evaluate(legs, max_hold)

    out = {
        "candidate": spec.get("candidate", ""),
        "verdict": res.verdict,
        "rt_cost_bps": res.rt_cost_bps,
        "carry_bps_per_day": res.carry_bps_per_day,
        "breakeven_days": res.breakeven_days,
        "min_move_bps_at_max_hold": res.min_move_bps,
        "stressed": {
            "rt_cost_bps": res.rt_cost_bps_stressed,
            "breakeven_days": res.breakeven_days_stressed,
            "min_move_bps_at_max_hold": res.min_move_bps_stressed,
        },
        "max_hold_days": res.max_hold_days,
        "reason": res.reason,
    }

    if args.run_id:
        record = {
            "run_id": args.run_id,
            "kind": "feasibility",
            "candidate": spec.get("candidate", ""),
            "verdict": res.verdict,
            "rt_cost_bps": res.rt_cost_bps,
            "carry_bps_per_day": res.carry_bps_per_day,
            "breakeven_days": res.breakeven_days,
            "breakeven_days_stressed": res.breakeven_days_stressed,
            "min_move_bps": res.min_move_bps,
            "max_hold_days": res.max_hold_days,
            "assumptions_hash": feasibility.hash_spec(spec),
            "notes": spec.get("notes", ""),
        }
        _append_run_record(Path(args.runs_dir), record)
    if args.candidate:
        _update_registry(Path(args.registry), args.candidate, res, spec)

    print(json.dumps(out, sort_keys=True, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
