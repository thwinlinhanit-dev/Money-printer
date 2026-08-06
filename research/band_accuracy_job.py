"""The weekly band-accuracy job (RES-4, spec 029 LIQ-6): idempotent, journaling
a per-week trend. Mirrors ``grading_job.py`` (RES-2): the job contract here is
idempotency + the append-only trend journal (W-6); parsing/aggregation math
lives in ``band_accuracy.py`` (unit-tested).

Each invocation shells out to the Rust ``whale_study`` binary once with
``--json --run-id --runs-dir``, so the run record is ALSO journaled to
``<out_dir>/runs/index.jsonl`` (the SIM-10 tracker the binary owns, RES-4 —
every invocation is a distinct run there). This job's own ledger is the
per-week ``{week}.json`` + ``band_accuracy.jsonl``:

* with an explicit ``--week``, an existing ``{week}.json`` short-circuits the
  whole job — no shell-out, no double-journal (true RES-2 no-op);
* with the week derived from the run's data range, the weekly ledger is still
  idempotent (an existing ``{week}.json`` is never rewritten or re-appended),
  but the binary has already journaled its per-run record by then — the
  tracker records runs, the ledger records weeks (documented in spec 029).

Unavailable market data is a failed job, not an optimistic row: a non-zero
``whale_study`` exit, an unparseable report, or missing logs raise
:class:`BandJobError` (exit 2 at the CLI), and nothing is journaled.
"""

from __future__ import annotations

import json
import secrets
import subprocess
import sys
import time
from pathlib import Path

from band_accuracy import BandRun, SideMetrics, parse_report

CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
JOURNAL_NAME = "band_accuracy.jsonl"


class BandJobError(RuntimeError):
    """Unavailable/invalid inputs for a band-accuracy week (maps to exit 2)."""


def run_weekly_band_accuracy(
    logs: list[Path | str],
    out_dir: Path | str,
    binary: Path | str,
    *,
    week: str | None = None,
    config: Path | str | None = None,
    git_sha: str | None = None,
    run_id: str | None = None,
    runs_dir: Path | str | None = None,
) -> tuple[Path, bool, dict | None]:
    """Grade one week of recorded logs against the real liq prices and journal
    the trend.

    Returns ``(grades_path, ran, payload)``. Idempotent (RES-2): with an
    explicit ``week``, an existing ``{week}.json`` makes the job a no-op
    (``ran=False``, ``payload=None``) — a re-fired timer never rewrites a
    graded week (W-6 append-only) and never double-appends to the trend
    journal. Without ``week`` it is derived from the report's data range.

    ``runs_dir`` overrides where the binary journals its SIM-10 run record
    (``<runs_dir>/index.jsonl``); default ``<out_dir>/runs``. A scheduled
    job passes the canonical tracker dir (e.g. ``/opt/money-printer/runs``)
    so RES-4 evidence lands in the shared ``runs/index.jsonl``.
    """
    logs = [Path(p) for p in logs]
    out_dir = Path(out_dir)
    if not logs:
        raise BandJobError("no --log inputs given")

    # Explicit week ⇒ true short-circuit BEFORE any input/binary checks
    # (RES-2): the graded ledger alone decides the no-op, so a re-fired timer
    # never re-does work and never depends on the binary or logs still being
    # present.
    if week is not None and (out_dir / f"{week}.json").exists():
        return out_dir / f"{week}.json", False, None

    missing = [str(p) for p in logs if not p.exists()]
    if missing:
        raise BandJobError(f"log file(s) missing: {', '.join(missing)}")
    if not Path(binary).exists():
        raise BandJobError(
            f"whale_study binary not found at {binary} — build it with "
            "`cargo build -p mp-features --bin whale_study`"
        )
    out_dir.mkdir(parents=True, exist_ok=True)

    record = _run_study(logs, binary, config, git_sha, run_id, Path(runs_dir) if runs_dir else out_dir / "runs")
    if record.observations == 0:
        # No spec 028 whale positions paired in the replay: the week has no
        # validation signal. Unavailable data is a failed job, not an
        # optimistic n=0 row (same principle as run_grading.py).
        raise BandJobError(
            "no band-accuracy observations — the replay paired no spec 028 "
            "whale liq prices (check the --log inputs)"
        )
    if week is None:
        week = record.week

    grades_path = out_dir / f"{week}.json"
    payload = {
        "week": week,
        "run_id": record.run_id,
        "git_sha": record.git_sha,
        "config_hash": record.config_hash,
        "data_from_ns": record.data_from_ns,
        "data_to_ns": record.data_to_ns,
        "events": record.events,
        "observations": record.observations,
        "long": _side(record.long),
        "short": _side(record.short),
        "total": _side(record.total),
    }
    if grades_path.exists():
        # Week derived from the data range and already graded: the ledger is
        # idempotent even though the binary journaled its per-run record.
        return grades_path, False, None

    grades_path.write_text(json.dumps(payload, sort_keys=True, indent=1), encoding="utf-8")

    # Trend journal: one line per graded week, append-only (W-6).
    with (out_dir / JOURNAL_NAME).open("a", encoding="utf-8") as f:
        f.write(
            json.dumps(
                {"week": week, "run_id": record.run_id, **_side(record.total), "config_hash": record.config_hash},
                sort_keys=True,
            )
            + "\n"
        )
    return grades_path, True, payload


def _run_study(
    logs: list[Path],
    binary: Path | str,
    config: Path | str | None,
    git_sha: str | None,
    run_id: str | None,
    runs_dir: Path,
) -> BandRun:
    """Shell out to ``whale_study --json`` once; fail-closed on any error.

    A ``.py`` binary is run under the current interpreter (used by the test
    suite's fake ``whale_study`` shim).
    """
    base = [str(binary)]
    if str(binary).endswith(".py"):
        base = [sys.executable, str(binary)]
    cmd = base + [
        "--json",
        "--run-id",
        run_id or ulid(),
        "--runs-dir",
        str(runs_dir),
    ]
    if config is not None:
        cmd += ["--config", str(config)]
    if git_sha is not None:
        cmd += ["--git-sha", git_sha]
    for p in logs:
        cmd += ["--log", str(p)]

    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise BandJobError(f"whale_study failed to run: {e}") from None
    if proc.returncode != 0:
        detail = proc.stderr.strip() or proc.stdout.strip() or f"exit {proc.returncode}"
        raise BandJobError(f"whale_study exited non-zero: {detail[:400]}")
    try:
        report = json.loads(proc.stdout)
        return parse_report(report)
    except ValueError as e:  # json.JSONDecodeError is a ValueError
        raise BandJobError(f"whale_study output unparseable: {e}") from None


def ulid() -> str:
    """Crockford-base32 ULID (48-bit ms timestamp + 80 random bits) — the
    run-id convention the sim tracker uses at the ops layer (SIM-10)."""
    ts = int(time.time() * 1000)
    out = ""
    for _ in range(10):
        out += CROCKFORD[ts & 31]
        ts >>= 5
    rand = int.from_bytes(secrets.token_bytes(10), "big")
    for _ in range(16):
        out += CROCKFORD[rand & 31]
        rand >>= 5
    return out[::-1]


def _side(m: SideMetrics) -> dict:
    return {
        "n": m.n,
        "mean_relative_error": m.mean_relative_error,
        "coverage": m.coverage,
    }
