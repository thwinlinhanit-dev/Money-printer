"""Research idea registry (research-lab roadmap Phase 1.3, spec 006).

One owner-visible record per candidate idea - whether it ever became code or
stayed a backlog line - so nothing lives only in a chat and the weekly report /
experiment tracker read from a single source. The registry is NEVER rebuilt
from a winning backtest alone; it is a ledger that the generator scaffolds from
the repo's own evidence (strategy hypotheses + the backlog "Strategies & alpha"
section) and that humans/agents update as state changes.

Checks enforced by :func:`check`:
- every discovered candidate has exactly one record (``id`` is the key);
  a strategy and a backlog bullet naming the same idea resolve to ONE id
  (backlog parentheticals like ``(FIRST BACKTEST 2026-08-13)`` are stripped
  so slugs stay stable and the ledger never holds two records for one idea);
- ``state`` is drawn from an allowed enum (spec 006 funnel states + terminal
  kills + ``not-gradable``);
- every ``run_id`` listed in a record exists in ``runs/index.jsonl`` (the
  record's evidence agrees with the experiment tracker, not free text).

Pure stdlib, deterministic (no wall clock); research-only (CONV-2).
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass

# Funnel states (spec 006) + terminal verdicts. ``recorded`` = backlog idea that
# has not (yet) entered the funnel.
ALLOWED_STATES = {
    "recorded",
    "hypothesis",
    "backtest",
    "walkforward",
    "paper",
    "live-small",
    "live-scaled",
    "held",
    "killed",
    "not-gradable",
}

_EDGE_HEAD = "## Edge:"
_HDR_RE = re.compile(r"^# (.+?)\s+[-–—]\s*Hypothesis$")
_BACKLOG_BULLET = re.compile(
    r"^-\s+\*\*\[(?P<tag>v1\.x|v2|maybe-never)\]\s*(?P<name>[^*]+?)\s*(?:\((?:.*?)\))?\*\*\s*[-–—]"
)
_SLUG_KEEP = re.compile(r"[^a-z0-9]+")


def _slug(raw: str) -> str:
    return _SLUG_KEEP.sub("-", raw.strip().lower()).strip("-")


def _first_edge(text: str) -> str:
    idx = text.find(_EDGE_HEAD)
    if idx < 0:
        return ""
    rest = text[idx + len(_EDGE_HEAD) :].splitlines()
    for ln in rest:
        s = ln.strip()
        if s and not s.startswith("#") and not s.startswith("```"):
            return s[:400]
    return ""


def discover_strategies(strategies_dir) -> list["Candidate"]:
    from pathlib import Path

    d = Path(strategies_dir)
    out: list[Candidate] = []
    if not d.is_dir():
        return out
    for folder in sorted(p for p in d.iterdir() if p.is_dir()):
        hyp = folder / "hypothesis.md"
        if not hyp.exists():
            continue
        text = hyp.read_text(encoding="utf-8")
        edge = _first_edge(text)
        m = _HDR_RE.search(text)
        cid = m.group(1).strip() if m else folder.name
        out.append(
            Candidate(
                id=cid,
                name=folder.name,
                source="strategy",
                hypothesis=edge,
                default_state="hypothesis",
            )
        )
    return out


def discover_backlog(backlog_path) -> list["Candidate"]:
    """Backlog *alpha* candidates (the "## Strategies & alpha" section, spec 006)."""
    from pathlib import Path

    p = Path(backlog_path)
    if not p.exists():
        return []
    lines = p.read_text(encoding="utf-8").splitlines()
    out: list[Candidate] = []
    active = False
    for line in lines:
        if line.startswith("## Strateg") and "alpha" in line.lower():
            active = True
            continue
        if active:
            if line.startswith("## "):
                break  # left the alpha section
            m = _BACKLOG_BULLET.match(line)
            if not m:
                continue
            name = m.group("name")
            out.append(
                Candidate(
                    id=_slug(name),
                    name=name,
                    source="backlog",
                    hypothesis="",
                    default_state="recorded",
                )
            )
    return out


def default_record(c: "Candidate") -> dict:
    return {
        "id": c.id,
        "name": c.name,
        "source": c.source,
        "hypothesis": c.hypothesis,
        "state": c.default_state,
        "reason": "",
        "run_ids": [],
        "evidence": [],
        "economic_feasibility": "not evaluated",
        "costs": "",
        "parameter_budget": "",
        "required_data": "",
        "reviewer": "",
    }


@dataclass(frozen=True)
class Candidate:
    """A candidate the registry must have exactly one record for."""

    id: str
    name: str
    source: str  # "strategy" | "backlog"
    hypothesis: str
    default_state: str


def load_registry(path) -> dict[str, dict]:
    from pathlib import Path

    p = Path(path)
    if not p.exists():
        return {}
    out: dict[str, dict] = {}
    for line in p.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        rec = json.loads(line)
        out[rec["id"]] = rec
    return out


def save_registry(path, records: list[dict]) -> None:
    from pathlib import Path

    Path(path).parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        for rec in sorted(records, key=lambda r: r["id"]):
            f.write(json.dumps(rec, sort_keys=True) + "\n")


def _run_exists(runs_path, run_id: str) -> bool:
    from pathlib import Path

    p = Path(runs_path)
    if not p.exists():
        return False
    for line in p.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            if json.loads(line).get("run_id") == run_id:
                return True
        except json.JSONDecodeError:
            continue
    return False


def check(
    registry: dict[str, dict], candidates: list[Candidate], runs_path
) -> list[str]:
    """Return a list of problems; empty means the registry is healthy."""
    problems: list[str] = []
    ids = {c.id for c in candidates}
    for c in candidates:
        if c.id not in registry:
            problems.append(f"missing registry record for candidate '{c.id}'")
    for cid, rec in sorted(registry.items()):
        if cid not in ids:
            problems.append(
                f"registry entry '{cid}' has no matching candidate in the repo"
            )
        if rec.get("state") not in ALLOWED_STATES:
            problems.append(f"'{cid}' has invalid state {rec.get('state')!r}")
        for rid in rec.get("run_ids", []):
            if not _run_exists(runs_path, rid):
                problems.append(f"'{cid}' run_id {rid} not found in runs/index.jsonl")
    return problems


def render(registry: dict[str, dict]) -> str:
    rows = sorted(registry.values(), key=lambda r: r["id"])
    lines = [
        "# Research idea registry (roadmap Phase 1.3)",
        "",
        "| id | source | state | reason |",
        "|---|---|---|---|",
    ]
    for r in rows:
        reason = (r.get("reason") or "").replace("|", "/").replace("\n", " ")
        lines.append(
            f"| {r['id']} | {r.get('source', '')} | {r.get('state', '')} | {reason} |"
        )
    return "\n".join(lines) + "\n"
