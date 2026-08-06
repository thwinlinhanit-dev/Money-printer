"""RES-4 leverage calibration (spec 029 LIQ-11): parsing, TOML rendering, and
the job contract — hand-verified numbers and a fake ``whale_study`` shim."""

import json
import sys
from pathlib import Path

import pytest

from calibrate_leverage import parse_report, render_toml
from calibrate_leverage_job import CalibrationError, run_leverage_calibration
from run_calibrate_leverage import main as cli_main


def report(
    *,
    n: int = 4,
    positions_seen: int = 5,
    total_notional: float = 500.0,
    sum_weights: float = 1.0,
    run_id: str = "cal-1",
    tiers: list[dict] | None = None,
) -> dict:
    if tiers is None:
        tiers = [
            {"leverage": 1.0, "weight": 0.0, "count": 0, "notional": 0.0},
            {"leverage": 2.0, "weight": 0.4, "count": 1, "notional": 200.0},
            {"leverage": 5.0, "weight": 0.2, "count": 1, "notional": 100.0},
            {"leverage": 10.0, "weight": 0.2, "count": 1, "notional": 100.0},
            {"leverage": 20.0, "weight": 0.0, "count": 0, "notional": 0.0},
            {"leverage": 50.0, "weight": 0.2, "count": 1, "notional": 100.0},
        ]
    return {
        "study": "leverage_calibration",
        "git_sha": "abc1234",
        "n": n,
        "positions_seen": positions_seen,
        "total_notional": total_notional,
        "maintenance_buffer": 1.5,
        "tiers": tiers,
        "sum_weights": sum_weights,
        "config_hash": "deadbeefdeadbeef",
        "data_from_ns": 1,
        "data_to_ns": 5,
        "run_id": run_id,
    }


def write_shim(tmp_path: Path, *, payload: dict | None = None, exit_code: int = 0, stdout: str | None = None) -> Path:
    body = f"import sys\nprint({json.dumps(stdout if stdout is not None else json.dumps(payload))})\n"
    if exit_code != 0:
        body = f"import sys\nsys.stderr.write('boom')\nsys.exit({exit_code})\n"
    shim = tmp_path / "whale_study.py"
    shim.write_text(body, encoding="utf-8")
    return shim


# ---- pure math ------------------------------------------------------------


def test_res_4_calibrate_parse_report_fields():
    r = parse_report(report())
    assert r.run_id == "cal-1"
    assert r.git_sha == "abc1234"
    assert r.config_hash == "deadbeefdeadbeef"
    assert r.n == 4 and r.positions_seen == 5
    assert r.total_notional == 500.0
    assert r.maintenance_buffer == 1.5
    assert r.sum_weights == 1.0
    assert len(r.tiers) == 6
    assert r.tiers[1].leverage == 2.0 and r.tiers[1].weight == 0.4 and r.tiers[1].count == 1
    assert r.tiers[5].leverage == 50.0 and r.tiers[5].weight == 0.2


def test_res_4_calibrate_parse_fail_closed():
    bad = report()
    bad["study"] = "whale_study"
    with pytest.raises(ValueError):
        parse_report(bad)
    for key in ("tiers",):
        bad = report()
        del bad[key]
        with pytest.raises(ValueError):
            parse_report(bad)
    bad = report()
    bad["tiers"] = []
    with pytest.raises(ValueError):
        parse_report(bad)
    bad = report()
    bad["tiers"] = [{"leverage": "x", "weight": 1.0, "count": 1, "notional": 1.0}]
    with pytest.raises(ValueError):
        parse_report(bad)
    bad = report()
    bad["n"] = 4.0
    with pytest.raises(ValueError):
        parse_report(bad)
    bad = report()
    bad["sum_weights"] = True
    with pytest.raises(ValueError):
        parse_report(bad)
    with pytest.raises(ValueError):
        parse_report([1, 2])


def test_res_4_calibrate_render_toml_exact():
    expected = (
        "[liq_est_bands]\n"
        "# Calibrated from recorded spec 028 real leverage distribution (spec 029 LIQ-11):\n"
        "# n=4 positions, total_notional=500.0, config_hash=deadbeefdeadbeef\n"
        "maintenance_buffer = 1.5\n"
        "[[liq_est_bands.leverage_tiers]]\n"
        "leverage = 1.0\n"
        "weight = 0.0\n"
        "[[liq_est_bands.leverage_tiers]]\n"
        "leverage = 2.0\n"
        "weight = 0.4\n"
        "[[liq_est_bands.leverage_tiers]]\n"
        "leverage = 5.0\n"
        "weight = 0.2\n"
        "[[liq_est_bands.leverage_tiers]]\n"
        "leverage = 10.0\n"
        "weight = 0.2\n"
        "[[liq_est_bands.leverage_tiers]]\n"
        "leverage = 20.0\n"
        "weight = 0.0\n"
        "[[liq_est_bands.leverage_tiers]]\n"
        "leverage = 50.0\n"
        "weight = 0.2\n"
    )
    assert render_toml(parse_report(report())) == expected


# ---- job contract ---------------------------------------------------------


def test_res_4_calibrate_job_happy_path(tmp_path):
    shim = write_shim(tmp_path, payload=report())
    log = tmp_path / "positions.log"
    log.write_text("ignored", encoding="utf-8")

    path, run = run_leverage_calibration([log], tmp_path / "out", shim)
    assert path.name == "cal-1.json"
    record = json.loads(path.read_text(encoding="utf-8"))
    assert record["study"] == "leverage_calibration"
    assert record["n"] == 4
    assert record["sum_weights"] == 1.0
    assert "[liq_est_bands]" in record["toml"]
    assert "weight = 0.4" in record["toml"]
    # The record is self-contained (reproducible from itself, SIM-10).
    assert record["config_hash"] == "deadbeefdeadbeef"

    lines = (tmp_path / "out" / "calibrations.jsonl").read_text(encoding="utf-8").strip().splitlines()
    assert len(lines) == 1
    ev = json.loads(lines[0])
    assert ev["run_id"] == "cal-1" and ev["n"] == 4 and ev["sum_weights"] == 1.0


def test_res_4_calibrate_job_fail_closed(tmp_path):
    log = tmp_path / "positions.log"
    log.write_text("ignored", encoding="utf-8")

    # Non-zero exit ⇒ job error, nothing journaled.
    shim = write_shim(tmp_path, payload=report(), exit_code=1)
    with pytest.raises(CalibrationError):
        run_leverage_calibration([log], tmp_path / "out", shim)

    # Unparseable output ⇒ job error.
    shim = write_shim(tmp_path, stdout="not json")
    with pytest.raises(CalibrationError):
        run_leverage_calibration([log], tmp_path / "out", shim)

    # An empty census is a failed job, never a bogus override.
    shim = write_shim(tmp_path, payload=report(n=0, positions_seen=0, total_notional=0.0, sum_weights=0.0))
    with pytest.raises(CalibrationError):
        run_leverage_calibration([log], tmp_path / "out", shim)

    # An inconsistent distribution (Σ ≠ 1) is a failed job.
    shim = write_shim(tmp_path, payload=report(sum_weights=0.95))
    with pytest.raises(CalibrationError):
        run_leverage_calibration([log], tmp_path / "out", shim)

    # Missing log / missing binary ⇒ job error before any shell-out.
    shim = write_shim(tmp_path, payload=report())
    with pytest.raises(CalibrationError):
        run_leverage_calibration([tmp_path / "nope.log"], tmp_path / "out", shim)
    with pytest.raises(CalibrationError):
        run_leverage_calibration([log], tmp_path / "out", tmp_path / "missing-whale")

    assert not (tmp_path / "out" / "calibrations.jsonl").exists()


def test_res_4_calibrate_job_never_overwrites_evidence(tmp_path):
    # Evidence records are immutable (W-6): a re-run with the SAME run_id must
    # fail, not silently overwrite the prior run's record.
    shim = write_shim(tmp_path, payload=report())
    log = tmp_path / "positions.log"
    log.write_text("ignored", encoding="utf-8")

    path, _ = run_leverage_calibration([log], tmp_path / "out", shim)
    assert path.name == "cal-1.json"
    before = path.read_text(encoding="utf-8")
    with pytest.raises(CalibrationError):
        run_leverage_calibration([log], tmp_path / "out", shim, run_id="cal-1")
    assert path.read_text(encoding="utf-8") == before


def test_res_4_calibrate_cli_exit_codes_and_toml(tmp_path, capsys):
    shim = write_shim(tmp_path, payload=report())
    log = tmp_path / "positions.log"
    log.write_text("ignored", encoding="utf-8")

    code = cli_main(["--log", str(log), "--whale-study", str(shim), "--out-dir", str(tmp_path / "out")])
    assert code == 0
    summary = json.loads(capsys.readouterr().out)
    assert summary["n"] == 4 and summary["run_id"] == "cal-1"

    # Second invocation with a fresh out-dir (records are immutable per run,
    # so the same run_id cannot write twice).
    code = cli_main(
        ["--log", str(log), "--whale-study", str(shim), "--out-dir", str(tmp_path / "out2"), "--print-toml"]
    )
    assert code == 0
    assert capsys.readouterr().out.startswith("[liq_est_bands]\n")

    code = cli_main(["--log", str(tmp_path / "missing.log"), "--whale-study", str(shim), "--out-dir", str(tmp_path / "out")])
    assert code == 2
