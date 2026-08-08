"""RES-4 band-accuracy (spec 029 LIQ-6): parsing, weekly trend, and the
job contract — hand-verified numbers and a fake ``whale_study`` shim."""

import json
import sys
from pathlib import Path

import pytest

from band_accuracy import BandRun, parse_report, week_from_ns, weekly_trend
from band_accuracy_job import BandJobError, run_weekly_band_accuracy
from run_band_accuracy import main as cli_main

# 2026-08-05T12:00:00Z in ns — hand-computed: 20_670 days to 2026-08-05,
# *86_400 s + 12 h, *1e9.
TS_2026_W32 = 1_785_931_200_000_000_000
# 2025-12-31T00:00:00Z — ISO week 1 of 2026 (the year of its Thursday).
TS_2025_W31 = 1_767_139_200_000_000_000


# A canonical whale_study --json report (shape = the Rust binary's output).
def report(
    *,
    data_from: int = TS_2026_W32,
    data_to: int = TS_2026_W32 + 86_400_000_000_000,
    long=(1, 0.020, 0.900),
    short=(1, 0.040, 0.800),
    total=(2, 0.030, 0.850),
    run_id="01JWHALESTUDY00000000000001",
) -> dict:
    return {
        "study": "whale_study",
        "git_sha": "abc1234",
        "params": {
            "maintenance_buffer": 0.05,
            "leverage_tiers": [{"leverage": 10.0, "weight": 1.0}],
        },
        "events": {
            "mark": 10,
            "open_interest": 5,
            "whale_positions": 2,
            "unmapped_skipped": 0,
        },
        "observations": total[0],
        "long": {"n": long[0], "mean_relative_error": long[1], "coverage": long[2]},
        "short": {"n": short[0], "mean_relative_error": short[1], "coverage": short[2]},
        "total": {"n": total[0], "mean_relative_error": total[1], "coverage": total[2]},
        "data_from_ns": data_from,
        "data_to_ns": data_to,
        "run_id": run_id,
        "config_hash": "deadbeefdeadbeef",
    }


def make_run(data_from: int, **kw) -> BandRun:
    return parse_report(report(data_from=data_from, **kw))


# ---- pure math ------------------------------------------------------------


def test_res_4_band_parse_report_fields():
    r = parse_report(report())
    assert r.run_id == "01JWHALESTUDY00000000000001"
    assert r.git_sha == "abc1234"
    assert r.config_hash == "deadbeefdeadbeef"
    assert r.data_from_ns == TS_2026_W32
    assert r.observations == 2
    assert (
        r.long.n == 1
        and r.long.mean_relative_error == 0.020
        and r.long.coverage == 0.900
    )
    assert (
        r.short.n == 1
        and r.short.mean_relative_error == 0.040
        and r.short.coverage == 0.800
    )
    assert (
        r.total.n == 2
        and r.total.mean_relative_error == 0.030
        and r.total.coverage == 0.850
    )
    assert r.events == {
        "mark": 10,
        "open_interest": 5,
        "whale_positions": 2,
        "unmapped_skipped": 0,
    }


def test_res_4_band_parse_report_fail_closed():
    # Wrong study tag.
    bad = report()
    bad["study"] = "car_study"
    with pytest.raises(ValueError):
        parse_report(bad)
    # Missing side metrics.
    for key in ("long", "short", "total"):
        bad = report()
        del bad[key]
        with pytest.raises(ValueError):
            parse_report(bad)
    # Bad metric types (bool is not a number; float n is not an int).
    bad = report()
    bad["total"]["n"] = 2.0
    with pytest.raises(ValueError):
        parse_report(bad)
    bad = report()
    bad["long"]["coverage"] = True
    with pytest.raises(ValueError):
        parse_report(bad)
    # Non-dict events.
    bad = report()
    bad["events"] = [1, 2]
    with pytest.raises(ValueError):
        parse_report(bad)
    # Not an object at all.
    with pytest.raises(ValueError):
        parse_report([1, 2])


def test_res_4_band_week_from_ns_iso_buckets():
    # 2026-08-05 is a Wednesday in ISO week 32 of 2026.
    assert week_from_ns(TS_2026_W32) == "2026-W32"
    # 2025-12-31's ISO week belongs to 2026 (the year of its Thursday).
    assert week_from_ns(TS_2025_W31) == "2026-W01"


def test_res_4_band_weekly_trend_weighted_merge_and_order():
    a = make_run(
        TS_2026_W32,
        long=(1, 0.020, 0.900),
        short=(1, 0.040, 0.800),
        total=(2, 0.030, 0.850),
    )
    # Same week, heavier run: n-weighted MRE/coverage.
    b = make_run(
        TS_2026_W32,
        long=(3, 0.100, 0.600),
        short=(1, 0.000, 1.000),
        total=(4, 0.075, 0.700),
    )
    # Earlier week sorts first (cross-year lexical == chronological).
    c = make_run(
        TS_2025_W31,
        long=(1, 0.010, 0.950),
        short=(0, 0.0, 0.0),
        total=(1, 0.010, 0.950),
    )

    trend = weekly_trend([b, c, a])
    assert [t.week for t in trend] == ["2026-W01", "2026-W32"]

    first = trend[0]
    assert first.n_runs == 1 and first.observations == 1
    assert first.long.n == 1 and first.long.mean_relative_error == pytest.approx(0.010)

    second = trend[1]
    assert second.n_runs == 2 and second.observations == 6
    # long: n=1+3=4, mre=(0.02*1+0.10*3)/4=0.08, cov=(0.9*1+0.6*3)/4=0.675.
    assert second.long.n == 4
    assert second.long.mean_relative_error == pytest.approx(0.080)
    assert second.long.coverage == pytest.approx(0.675)
    # total: n=2+4=6, mre=(0.03*2+0.075*4)/6=0.06.
    assert second.total.n == 6
    assert second.total.mean_relative_error == pytest.approx(0.060)


def test_res_4_band_weekly_trend_zero_n_week():
    zero = make_run(
        TS_2026_W32, long=(0, 0.0, 0.0), short=(0, 0.0, 0.0), total=(0, 0.0, 0.0)
    )
    row = weekly_trend([zero])[0]
    assert row.observations == 0 and row.total.n == 0
    assert row.total.mean_relative_error == 0.0 and row.total.coverage == 0.0


# ---- job contract (fake whale_study shim) ---------------------------------


def write_shim(
    tmp_path: Path,
    *,
    payload: dict | None = None,
    exit_code: int = 0,
    stdout: str | None = None,
) -> Path:
    """A tiny 'whale_study' that prints a canned report (or fails)."""
    body = f"import sys\nprint({json.dumps(stdout if stdout is not None else json.dumps(payload))})\n"
    if exit_code != 0:
        body = f"import sys\nsys.stderr.write('boom')\nsys.exit({exit_code})\n"
    shim = tmp_path / "whale_study.py"
    shim.write_text(body, encoding="utf-8")
    return shim


def test_res_4_band_job_happy_path_journals_week_and_trend(tmp_path):
    shim = write_shim(tmp_path, payload=report())
    logs = [tmp_path / "hl.log"]
    logs[0].write_text("ignored", encoding="utf-8")

    path, ran, payload = run_weekly_band_accuracy(
        logs, tmp_path / "out", shim, week="2026-W32"
    )
    assert ran is True
    assert path.name == "2026-W32.json"
    saved = json.loads(path.read_text(encoding="utf-8"))
    assert saved["week"] == "2026-W32"
    assert saved["observations"] == 2
    assert saved["total"]["n"] == 2
    assert saved["config_hash"] == "deadbeefdeadbeef"

    lines = (
        (tmp_path / "out" / "band_accuracy.jsonl")
        .read_text(encoding="utf-8")
        .strip()
        .splitlines()
    )
    assert len(lines) == 1
    row = json.loads(lines[0])
    assert row["week"] == "2026-W32"
    assert (
        row["n"] == 2
        and row["mean_relative_error"] == 0.030
        and row["coverage"] == 0.850
    )


def test_res_4_band_job_explicit_week_short_circuits_refire(tmp_path):
    shim = write_shim(tmp_path, payload=report())
    logs = [tmp_path / "hl.log"]
    logs[0].write_text("ignored", encoding="utf-8")
    out = tmp_path / "out"

    path1, ran1, _ = run_weekly_band_accuracy(logs, out, shim, week="2026-W32")
    path2, ran2, payload2 = run_weekly_band_accuracy(logs, out, shim, week="2026-W32")
    assert ran1 is True and ran2 is False and payload2 is None
    assert path1 == path2
    # Append-only: the trend journal was not touched by the re-fire.
    assert (
        len(
            (out / "band_accuracy.jsonl")
            .read_text(encoding="utf-8")
            .strip()
            .splitlines()
        )
        == 1
    )


def test_res_4_band_job_auto_week_from_data_range(tmp_path):
    shim = write_shim(tmp_path, payload=report())
    logs = [tmp_path / "hl.log"]
    logs[0].write_text("ignored", encoding="utf-8")

    path, ran, payload = run_weekly_band_accuracy(logs, tmp_path / "out", shim)
    assert ran is True
    # No --week: bucketed by the report's data_from_ns (2026-08-05 → 2026-W32).
    assert path.name == "2026-W32.json"
    assert payload["week"] == "2026-W32"


def test_res_4_band_job_auto_week_refire_does_not_double_journal(tmp_path):
    shim = write_shim(tmp_path, payload=report())
    logs = [tmp_path / "hl.log"]
    logs[0].write_text("ignored", encoding="utf-8")
    out = tmp_path / "out"

    _, ran1, _ = run_weekly_band_accuracy(logs, out, shim)
    _, ran2, _ = run_weekly_band_accuracy(logs, out, shim)
    assert ran1 is True and ran2 is False
    assert (
        len(
            (out / "band_accuracy.jsonl")
            .read_text(encoding="utf-8")
            .strip()
            .splitlines()
        )
        == 1
    )


def test_res_4_band_job_fail_closed(tmp_path):
    logs = [tmp_path / "hl.log"]
    logs[0].write_text("ignored", encoding="utf-8")

    # Non-zero exit from the binary ⇒ job error, nothing journaled.
    shim = write_shim(tmp_path, payload=report(), exit_code=1)
    with pytest.raises(BandJobError):
        run_weekly_band_accuracy(logs, tmp_path / "out", shim, week="2026-W32")

    # Unparseable output ⇒ job error.
    shim = write_shim(tmp_path, stdout="not json at all")
    with pytest.raises(BandJobError):
        run_weekly_band_accuracy(logs, tmp_path / "out", shim, week="2026-W32")

    # Missing log file ⇒ job error before any shell-out.
    with pytest.raises(BandJobError):
        run_weekly_band_accuracy(
            [tmp_path / "nope.log"], tmp_path / "out", shim, week="2026-W32"
        )

    # Missing binary ⇒ job error.
    with pytest.raises(BandJobError):
        run_weekly_band_accuracy(
            logs, tmp_path / "out", tmp_path / "missing-whale", week="2026-W32"
        )

    # Nothing was journaled by any failed attempt.
    assert not (tmp_path / "out" / "band_accuracy.jsonl").exists()


def test_res_4_band_job_runs_dir_override_journals_there(tmp_path):
    # A runs-aware shim that mimics the real binary: journals its SIM-10
    # record to the --runs-dir it is given, then prints the report.
    shim = tmp_path / "whale_study.py"
    shim.write_text(
        "import json, pathlib, sys\n"
        "i = sys.argv.index('--runs-dir')\n"
        "d = pathlib.Path(sys.argv[i + 1])\n"
        "d.mkdir(parents=True, exist_ok=True)\n"
        "(d / 'index.jsonl').write_text(json.dumps({'study': 'whale_study', 'run_id': 'x'}) + '\\n')\n"
        f"print({json.dumps(json.dumps(report()))})\n",
        encoding="utf-8",
    )
    logs = [tmp_path / "hl.log"]
    logs[0].write_text("ignored", encoding="utf-8")
    runs = tmp_path / "canonical-runs"

    path, ran, _ = run_weekly_band_accuracy(
        logs, tmp_path / "out", shim, week="2026-W32", runs_dir=runs
    )
    assert ran is True
    # The record went to the override, not the default <out-dir>/runs.
    assert (runs / "index.jsonl").read_text(
        encoding="utf-8"
    ).strip() == '{"study": "whale_study", "run_id": "x"}'
    assert not (tmp_path / "out" / "runs").exists()


def test_res_4_band_cli_happy_and_fail_exit_codes(tmp_path):
    shim = write_shim(tmp_path, payload=report())
    log = tmp_path / "hl.log"
    log.write_text("ignored", encoding="utf-8")

    code = cli_main(
        [
            "--log",
            str(log),
            "--whale-study",
            str(shim),
            "--out-dir",
            str(tmp_path / "out"),
            "--week",
            "2026-W32",
        ]
    )
    assert code == 0
    assert (tmp_path / "out" / "2026-W32.json").exists()

    # Unavailable data ⇒ exit 2, not an optimistic row.
    code = cli_main(
        [
            "--log",
            str(tmp_path / "missing.log"),
            "--whale-study",
            str(shim),
            "--out-dir",
            str(tmp_path / "out"),
        ]
    )
    assert code == 2
