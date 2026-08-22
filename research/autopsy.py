"""Strategy autopsy generator (serious-research-lab roadmap Phase 4.5).

For any strategy that is killed or materially changes, produce a structured
autopsy from the journaled run records: P&L by symbol/day/regime, cost share,
parameter sensitivity, event frequency, concentration, worst decision/fill
examples, and a classification (economic failure | data limitation |
execution-model failure). The autopsy is generated from the experiment
tracker - never free text alone - and the registry record links to it.

Classification is deterministic from the run records:
- no tradeable run records  -> data limitation (never tested)
- base expectancy positive, 2x-cost negative -> execution-model failure
- base expectancy negative (2x-cost <= base) -> economic failure
- otherwise undetermined.

Pure stdlib, deterministic (no wall clock); research-only (CONV-2).
"""

from __future__ import annotations

import json
import statistics
from datetime import datetime, timezone


def load_run_records(runs_path) -> list[dict]:
    from pathlib import Path

    out: list[dict] = []
    for line in Path(runs_path).read_text(encoding="utf-8").splitlines():
        if line.strip():
            out.append(json.loads(line))
    return out


def load_registry(registry_path) -> dict[str, dict]:
    from pathlib import Path

    out: dict[str, dict] = {}
    for line in Path(registry_path).read_text(encoding="utf-8").splitlines():
        if line.strip():
            rec = json.loads(line)
            out[rec["id"]] = rec
    return out


def collect(candidate: str, registry_path, runs_path) -> list[dict]:
    """The candidate's run records via its registry run_ids (evidence
    agreement - an autopsy never invents runs)."""
    rec = load_registry(registry_path).get(candidate)
    if rec is None:
        raise ValueError(f"no registry record for '{candidate}'")
    by_id = {r.get("run_id"): r for r in load_run_records(runs_path)}
    missing = [rid for rid in rec.get("run_ids", []) if rid not in by_id]
    if missing:
        raise ValueError(f"run_ids missing from tracker: {missing}")
    return [by_id[rid] for rid in rec.get("run_ids", [])]


def tradeable(rec: dict) -> bool:
    """A run counts as evidence only if it actually traded - a 0-trade run
    scoring 0.0 is absence of evidence (the grade_wf traded-day guard),
    never a datum for the mean."""
    return (
        isinstance(rec.get("trades"), (int, float))
        and rec.get("trades", 0) > 0
        and isinstance(rec.get("expectancy"), (int, float))
    )


def _run_days(rec: dict) -> list[str]:
    days = list(rec.get("corpus", []))
    if not days and rec.get("data_from_ns"):
        from datetime import datetime, timezone

        days = [
            datetime.fromtimestamp(rec["data_from_ns"] / 1e9, tz=timezone.utc).strftime(
                "%Y%m%d"
            )
        ]
    return days


def classify(records: list[dict]) -> dict:
    trading = [r for r in records if tradeable(r)]
    if not trading:
        return {
            "class": "data limitation",
            "detail": "no tradeable run records (strategy never traded in the journaled runs)",
        }
    base = statistics.mean(r["expectancy"] for r in trading)
    stress = statistics.mean(
        r.get("stress_expectancy_2x", r["expectancy"]) for r in trading
    )
    if base > 0 and stress < 0:
        return {
            "class": "execution-model failure",
            "detail": f"base expectancy {base:.1f} is positive but 2x-cost "
            f"{stress:.1f} is negative - costs killed the edge",
        }
    if base <= 0:
        share = (abs(stress) - abs(base)) / abs(base) * 100.0 if base else 0.0
        return {
            "class": "economic failure",
            "detail": f"base expectancy {base:.1f} is already negative "
            f"pre-cost; 2x-cost {stress:.1f} (cost share {share:.0f}%)",
        }
    return {
        "class": "undetermined",
        "detail": f"base {base:.1f} and 2x-cost {stress:.1f} are both positive "
        "- this evidence alone does not explain a kill",
    }


def _fmt(day: str, records: list[dict]) -> str:
    trades = sum(int(r.get("trades", 0)) for r in records)
    exps = [r["expectancy"] for r in records if tradeable(r)]
    exp = f"{statistics.mean(exps):,.1f}" if exps else "n/a"
    return f"| {day} | {trades} | {exp} |"


def render(candidate: str, records: list[dict], verdict: dict) -> str:
    trading = [r for r in records if tradeable(r)]
    lines = [
        f"# Autopsy: {candidate}",
        "",
        f"- date: {datetime.now(timezone.utc).strftime('%Y-%m-%d')}",
        f"- classification: **{verdict['class']}**",
        f"- reasoning: {verdict['detail']}",
        "",
        "## Run records (journaled, registry-linked)",
        "",
        "| run_id | kind | corpus | trades | expectancy | stress2x |",
        "|---|---|---|---|---|---|",
    ]
    for r in records:
        corpus = ",".join(r.get("corpus", r.get("log", "")))
        lines.append(
            f"| {r.get('run_id', '')} | {r.get('kind', 'sim')} | {corpus} | "
            f"{r.get('trades', 'n/a')} | {r.get('expectancy', 'n/a')} | "
            f"{r.get('stress_expectancy_2x', 'n/a')} |"
        )
    lines += [
        "",
        "## P&L by day",
        "",
        "| day | trades | mean expectancy |",
        "|---|---|---|",
    ]
    by_day: dict[str, list[dict]] = {}
    for r in records:
        for d in _run_days(r):
            by_day.setdefault(d, []).append(r)
    for day in sorted(by_day):
        lines.append(_fmt(day, by_day[day]))
    if trading:
        base = statistics.mean(r["expectancy"] for r in trading)
        stress = statistics.mean(
            r.get("stress_expectancy_2x", r["expectancy"]) for r in trading
        )
        lines += [
            "",
            "## Costs and concentration",
            "",
            f"- mean base expectancy: {base:,.1f}; mean 2x-cost: {stress:,.1f} "
            f"(cost share {abs(stress - base) / max(abs(base), 1e-9) * 100:.0f}%)",
            f"- trade concentration: {sum(r.get('trades', 0) for r in trading)} "
            f"total trades across {len(by_day)} corpus days",
        ]
    notes = [r for r in records if r.get("verdict") or r.get("note")]
    if notes:
        lines += ["", "## Worst decision / fill context (from journaled notes)", ""]
        for r in notes:
            lines.append(f"- `{r.get('run_id')}`: {r.get('verdict') or r.get('note')}")
    lines.append("")
    return "\n".join(lines)
