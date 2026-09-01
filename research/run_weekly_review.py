"""Weekly research review generator (serious-research-lab roadmap Phase 1.2).

A machine-derived weekly review: registry table, journaled runs whose as-of
date falls in the ISO week, autopsies, and the benchmark row. The benchmark is
grounded on `docs/OWNER_POLICY.md` §3 (roadmap item 1.1, BINDING since
2026-08-25) and FAILS CLOSED to the explicit `unset` row when the policy file
or any of its three benchmark fields is missing or unparseable — a corrupt
policy never silently produces a wrong benchmark. Follows the grading_job split: machine
derivation first, human notes in a clearly marked section. Append-only - a
review for a week that already exists is refused (never silently overwritten).

Determinism contract: the only wall-clock input is the generated date; every
content row comes from registry.jsonl, runs/index.jsonl, and research/autopsies.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import registry

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_REGISTRY = REPO_ROOT / "research" / "registry.jsonl"
DEFAULT_RUNS = REPO_ROOT / "runs" / "index.jsonl"
DEFAULT_OUT = REPO_ROOT / "research" / "reviews"
DEFAULT_AUTOPSIES = REPO_ROOT / "research" / "autopsies"
DEFAULT_POLICY = REPO_ROOT / "docs" / "OWNER_POLICY.md"

_RUN_ID_DATE = re.compile(r"(20\d{2})-(\d{2})-(\d{2})")

_BENCHMARK_FIELDS = (
    ("primary", "Primary benchmark"),
    ("window", "Comparison window"),
    ("acceptance", "Acceptance rule"),
)

# ALP-8: the expected benchmark values after 2026-08-25 owner confirmation.
# If the policy file exists and any of these is missing, that is a bug
# ("unset" must never appear when the file is parseable).
_DEFAULT_BENCHMARK = {
    "primary": "Buy-and-hold BTC over identical evaluation windows",
    "window": "Trailing 90 days for weekly/monthly reviews",
    "acceptance": "Rolling 6-month expectancy > 0 after all costs AND >= benchmark",
}


def load_benchmark(policy_path) -> dict | None:
    """Parse the benchmark definition from OWNER_POLICY.md §3.

    Returns {primary, window, acceptance} or None when the policy file is
    missing or any required field row cannot be parsed (fail closed).
    """
    path = Path(policy_path)
    if not path.is_file():
        return None
    text = path.read_text(encoding="utf-8")
    out: dict[str, str] = {}
    for key, label in _BENCHMARK_FIELDS:
        m = re.search(rf"\|\s*{re.escape(label)}\s*\|\s*(.+?)\s*\|", text)
        if not m or not m.group(1).strip():
            return None
        out[key] = m.group(1).strip()
    return out


def _as_of(rec: dict) -> str:
    """As-of date: parse run_id date when present (named runs), else the
    data_to_ns wall-clock (journaled runs carry no separate timestamp)."""
    m = _RUN_ID_DATE.search(rec.get("run_id", ""))
    if m:
        return f"{m.group(1)}-{m.group(2)}-{m.group(3)}"
    ns = rec.get("data_to_ns")
    if isinstance(ns, (int, float)):
        return datetime.fromtimestamp(ns / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")
    return "unknown"


def week_bounds(week: str) -> tuple[str, str]:
    """ISO week string 2026-W34 -> (monday, sunday) dates."""
    m = re.fullmatch(r"(\d{4})-W(\d{1,2})", week)
    if not m:
        raise ValueError(f"bad week '{week}' (want YYYY-Wnn)")
    year, w = int(m.group(1)), int(m.group(2))
    monday = datetime.fromisocalendar(year, w, 1).date()
    return monday.isoformat(), (monday + timedelta(days=6)).isoformat()


def week_runs(runs_path, week: str) -> tuple[list[dict], list[dict]]:
    monday, sunday = week_bounds(week)
    in_week: list[dict] = []
    for line in Path(runs_path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        rec = json.loads(line)
        day = _as_of(rec)
        if monday <= day <= sunday:
            in_week.append(rec)
    by_kind: dict[str, int] = {}
    for rec in in_week:
        by_kind[rec.get("kind", "sim")] = by_kind.get(rec.get("kind", "sim"), 0) + 1
    return in_week, sorted(by_kind.items())


def render(week: str, registry_path, runs_path, autopsies_dir, policy_path=DEFAULT_POLICY) -> str:
    recs = registry.load_registry(registry_path)
    table = registry.render(recs)
    table = table.replace("# Research idea registry (roadmap Phase 1.3)\n\n", "")
    in_week, by_kind = week_runs(runs_path, week)
    monday, sunday = week_bounds(week)

    lines = [
        f"# Research weekly review: {week}",
        "",
        f"- period: {monday} .. {sunday}",
        f"- generated: {datetime.now(timezone.utc).strftime('%Y-%m-%d')}",
        "",
        "## Benchmark",
        "",
    ]
    bench = load_benchmark(policy_path)
    if bench is not None:
        lines += [
            f"- primary: {bench['primary']}",
            f"- window: {bench['window']}",
            f"- acceptance: {bench['acceptance']}",
            "- source: docs/OWNER_POLICY.md §3",
        ]
    else:
        # ALP-8: after 2026-08-25, missing/unparseable benchmark is a bug,
        # not "pending". The policy file exists and is BINDING; if any field
        # is missing, say so honestly — "unset" only when the file is absent.
        policy_exists = Path(policy_path).is_file()
        if policy_exists:
            lines += [
                "- `unset` — BUG: OWNER_POLICY.md exists but benchmark fields "
                "could not be parsed. This is a code defect (ALP-8), not "
                "a pending owner decision. Fix load_benchmark().",
            ]
        else:
            lines += [
                "- `unset` — OWNER_POLICY.md missing; no benchmark defined.",
            ]
    lines += [
        "",
        "## Idea registry",
        "",
        f"({len(recs)} records; full table from `run_registry.py render`)",
        "",
        table,
        "",
        "## Journaled runs this week",
        "",
        f"- total: {len(in_week)} runs across {len(by_kind)} kinds "
        f"({', '.join(f'{k}: {n}' for k, n in by_kind) or 'none'})",
        "",
    ]
    if in_week:
        lines += [
            "| run_id | kind | as-of | verdict / key result |",
            "|---|---|---|---|",
        ]
        for rec in sorted(in_week, key=lambda r: (_as_of(r), r.get("run_id", ""))):
            key = (
                rec.get("verdict")
                or rec.get("reason")
                or rec.get("economic_feasibility")
                or ""
            )
            lines.append(
                f"| {rec.get('run_id', '')} | {rec.get('kind', 'sim')} | "
                f"{_as_of(rec)} | {str(key).replace(chr(124), '/')} |"
            )
    lines += ["", "## Autopsies", ""]
    autops_dir = Path(autopsies_dir)
    autops = sorted(autops_dir.glob("*.md")) if autops_dir.is_dir() else []
    if autops:
        for a in autops:
            lines.append(f"- {a.name}")
    else:
        lines.append("- none")
    lines += ["", "## Human notes", "", "_owner review: (pending)_", ""]
    return "\n".join(lines)


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="weekly research review (1.2)")
    p.add_argument("--week", required=True)
    p.add_argument("--registry", default=str(DEFAULT_REGISTRY))
    p.add_argument("--runs", default=str(DEFAULT_RUNS))
    p.add_argument("--out-dir", default=str(DEFAULT_OUT))
    p.add_argument("--autopsies", default=str(DEFAULT_AUTOPSIES))
    p.add_argument("--policy", default=str(DEFAULT_POLICY))
    args = p.parse_args(argv)

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{args.week}.md"
    if path.exists():
        print(
            f"exit 2: review already exists: {path} (append-only, re-run refused)",
            file=sys.stderr,
        )
        raise SystemExit(2)

    try:
        week_bounds(args.week)
    except ValueError as exc:
        print(f"exit 2: {exc}", file=sys.stderr)
        raise SystemExit(2)

    report = render(args.week, args.registry, args.runs, args.autopsies, args.policy)
    path.write_text(report, encoding="utf-8")
    print(f"wrote {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
