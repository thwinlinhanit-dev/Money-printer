"""The leverage-calibration job (spec 029 LIQ-11): turns recorded spec 028
positions into the `liq.est_bands` tier weights that replace the documented
assumption.

Shells out to `whale_study --leverage-calibration --json --run-id --runs-dir`
once per invocation, so the SIM-10 run record lands in
`<runs-dir>/index.jsonl` (RES-4 tracker, append-only W-6) — same contract as
`band_accuracy_job.py`. This job's own artifacts: an immutable per-run record
`<out-dir>/<run_id>.json` (with the rendered `[liq_est_bands]` TOML inside)
and an append-only `calibrations.jsonl` evidence line.

Unlike the weekly band-accuracy ledger, calibration is NOT idempotent by
design: it is a cumulative estimate that improves as the census grows, so
each run is a fresh snapshot. Unavailable/invalid data is a failed job, not
an optimistic override: a non-zero `whale_study` exit, an unparseable report,
zero valid samples, or weights that don't sum to ≈1 raise
:class:`CalibrationError` (exit 2 at the CLI) and nothing is journaled.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from band_accuracy_job import ulid
from calibrate_leverage import WEIGHT_EPSILON, CalibrationRun, Tier, parse_report, render_toml


class CalibrationError(RuntimeError):
    """Unavailable/invalid inputs for a leverage calibration (maps to exit 2)."""


def run_leverage_calibration(
    logs: list[Path | str],
    out_dir: Path | str,
    binary: Path | str,
    *,
    config: Path | str | None = None,
    git_sha: str | None = None,
    run_id: str | None = None,
    runs_dir: Path | str | None = None,
) -> tuple[Path, CalibrationRun]:
    """Calibrate the `liq.est_bands` tier weights from recorded spec 028 logs.

    Returns ``(record_path, run)``. ``runs_dir`` defaults to
    ``<out_dir>/runs`` — a scheduled caller passes the canonical tracker dir.
    """
    logs = [Path(p) for p in logs]
    out_dir = Path(out_dir)
    if not logs:
        raise CalibrationError("no --log inputs given")
    missing = [str(p) for p in logs if not p.exists()]
    if missing:
        raise CalibrationError(f"log file(s) missing: {', '.join(missing)}")
    if not Path(binary).exists():
        raise CalibrationError(
            f"whale_study binary not found at {binary} — build it with "
            "`cargo build -p mp-features --bin whale_study`"
        )
    out_dir.mkdir(parents=True, exist_ok=True)
    run_id = run_id or ulid()
    runs_dir = Path(runs_dir) if runs_dir else out_dir / "runs"

    base = [str(binary)]
    if str(binary).endswith(".py"):
        base = [sys.executable, str(binary)]
    cmd = base + [
        "--leverage-calibration",
        "--json",
        "--run-id",
        run_id,
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
        raise CalibrationError(f"whale_study failed to run: {e}") from None
    if proc.returncode != 0:
        detail = proc.stderr.strip() or proc.stdout.strip() or f"exit {proc.returncode}"
        raise CalibrationError(f"whale_study exited non-zero: {detail[:400]}")
    try:
        run = parse_report(json.loads(proc.stdout))
    except (json.JSONDecodeError, ValueError) as e:
        raise CalibrationError(f"whale_study output unparseable: {e}") from None

    # Fail-closed: an empty census or an inconsistent distribution is a failed
    # job, never a silently-bogus config override.
    if run.n == 0:
        raise CalibrationError(
            "no valid spec 028 leverage samples — nothing to calibrate (check the --log inputs)"
        )
    if abs(run.sum_weights - 1.0) > WEIGHT_EPSILON:
        raise CalibrationError(
            f"calibrated weights sum to {run.sum_weights:.6f}, not ≈1.0 — inconsistent report"
        )

    record = {
        "study": "leverage_calibration",
        "run_id": run.run_id or run_id,
        "git_sha": run.git_sha,
        "config_hash": run.config_hash,
        "data_from_ns": run.data_from_ns,
        "data_to_ns": run.data_to_ns,
        "n": run.n,
        "positions_seen": run.positions_seen,
        "total_notional": run.total_notional,
        "maintenance_buffer": run.maintenance_buffer,
        "sum_weights": run.sum_weights,
        "tiers": [_tier(t) for t in run.tiers],
        # The operator-facing override, self-contained in the record.
        "toml": render_toml(run),
    }
    path = out_dir / f"{record['run_id']}.json"
    if path.exists():
        # Evidence records are immutable (W-6): a re-run must get a fresh
        # run_id, never silently overwrite a prior run's record.
        raise CalibrationError(
            f"run record {path.name} already exists — refusing to overwrite evidence "
            "(W-6); pass a fresh --run-id"
        )
    path.write_text(json.dumps(record, sort_keys=True, indent=1), encoding="utf-8")

    # Append-only evidence line (W-6): one per calibration run.
    with (out_dir / "calibrations.jsonl").open("a", encoding="utf-8") as f:
        f.write(
            json.dumps(
                {
                    "run_id": record["run_id"],
                    "n": run.n,
                    "positions_seen": run.positions_seen,
                    "total_notional": run.total_notional,
                    "sum_weights": run.sum_weights,
                    "config_hash": run.config_hash,
                    "data_from_ns": run.data_from_ns,
                    "data_to_ns": run.data_to_ns,
                },
                sort_keys=True,
            )
            + "\n"
        )
    return path, run


def _tier(t: Tier) -> dict:
    return {
        "leverage": t.leverage,
        "weight": t.weight,
        "count": t.count,
        "notional": t.notional,
    }
