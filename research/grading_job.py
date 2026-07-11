"""The weekly screener-grading job (RES-2): idempotent, journaling a
leaderboard. An ops timer (`ops/systemd/grading.timer`) fires it weekly; the
job itself is a pure function of its inputs plus an idempotent writer, so a
re-fired week never double-journals.

Grading math lives in `grading.py` (unit-tested on hand-verified fixtures);
this module owns the job contract: idempotency + the append-only leaderboard
journal (W-6).
"""

from __future__ import annotations

import json
from pathlib import Path

from grading import Hit, RuleGrade, grade_hits, leaderboard


def run_weekly_grading(
    week: str,
    hits: list[Hit],
    prices: dict[str, list[tuple[int, float]]],
    horizon_ns: int,
    baseline: dict[str, float],
    out_dir: Path | str,
) -> tuple[Path, bool]:
    """Grade one week of screener hits and journal the leaderboard.

    Returns ``(grades_path, ran)``. Idempotent (RES-2): if this week's grade
    file already exists the job is a no-op (``ran=False``) — a re-fired timer
    never rewrites a graded week (W-6 append-only) and never double-appends
    to the leaderboard journal.
    """
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    grades_path = out_dir / f"{week}.json"
    if grades_path.exists():
        return grades_path, False

    grades = grade_hits(hits, prices, horizon_ns, baseline)
    board = leaderboard(grades)

    payload = {
        "week": week,
        "horizon_ns": horizon_ns,
        "rules": [_row(g) for g in board],
    }
    grades_path.write_text(json.dumps(payload, sort_keys=True, indent=1), encoding="utf-8")

    # Leaderboard journal: one line per rule per week, append-only.
    with (out_dir / "leaderboard.jsonl").open("a", encoding="utf-8") as f:
        for rank, g in enumerate(board, start=1):
            f.write(json.dumps({"week": week, "rank": rank, **_row(g)}, sort_keys=True) + "\n")
    return grades_path, True


def _row(g: RuleGrade) -> dict:
    return {
        "rule": g.rule,
        "n": g.n,
        "win_rate": g.win_rate,
        "avg_excess": g.avg_excess,
    }
