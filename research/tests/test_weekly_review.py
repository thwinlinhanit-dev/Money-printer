"""Phase 1.2 weekly review generator: ISO-week windowing and append-only
rendering from registry + runs fixtures."""

import json

import pytest

import run_weekly_review as wr

RUNS = [
    {
        "run_id": "backlog-event-studies-2026-08-16",
        "kind": "res4_event_study",
        "verdict": "NOT GRADABLE",
    },
    {
        "run_id": "feasibility-2026-08-19-funding-arb-v1",
        "kind": "feasibility",
        "economic_feasibility": "CLEARS_BASE_AND_STRESSED",
    },
    {
        "run_id": "01M00SJJPNDVA46811VDM8KA80",
        "kind": "sim",
        "data_to_ns": 1786809600000000000,
        "trades": 0,
    },  # 2026-08-15
    {
        "run_id": "unrelated-2027-01-05",
        "kind": "res4_event_study",
        "verdict": "future week",
    },
]

REGISTRY = [
    {
        "id": "a-v1",
        "source": "strategy",
        "state": "hypothesis",
        "reason": "",
        "hypothesis": "x",
        "required_data": "y",
        "run_ids": [],
        "evidence": [],
        "economic_feasibility": "not evaluated",
        "costs": "",
        "parameter_budget": "",
        "reviewer": "",
    },
    {
        "id": "liq-fade-v1",
        "source": "strategy",
        "state": "killed",
        "reason": "killed",
        "hypothesis": "",
        "required_data": "",
        "run_ids": [],
        "evidence": [],
        "economic_feasibility": "not evaluated",
        "costs": "",
        "parameter_budget": "",
        "reviewer": "",
    },
]


def _setup(tmp_path) -> tuple[str, str]:
    runs = tmp_path / "runs" / "index.jsonl"
    runs.parent.mkdir(parents=True)
    runs.write_text("\n".join(json.dumps(r) for r in RUNS) + "\n", encoding="utf-8")
    reg = tmp_path / "registry.jsonl"
    reg.write_text("\n".join(json.dumps(r) for r in REGISTRY) + "\n", encoding="utf-8")
    return str(runs), str(reg)


def test_week_bounds_iso():
    assert wr.week_bounds("2026-W34") == ("2026-08-17", "2026-08-23")


def test_week_bounds_rejects_bad_week():
    with pytest.raises(ValueError):
        wr.week_bounds("2026-08-17")


def test_as_of_from_run_id_or_data_to_ns():
    assert wr._as_of({"run_id": "feasibility-2026-08-19-x"}) == "2026-08-19"
    assert wr._as_of({"run_id": "r", "data_to_ns": 1786809600000000000}) == "2026-08-15"
    assert wr._as_of({"run_id": "r"}) == "unknown"


def test_week_runs_windows_by_as_of(tmp_path):
    runs, _reg = _setup(tmp_path)
    in_week, by_kind = wr.week_runs(runs, "2026-W34")
    ids = {r["run_id"] for r in in_week}
    assert ids == {"feasibility-2026-08-19-funding-arb-v1"}
    assert dict(by_kind) == {"feasibility": 1}


def _policy(tmp_path, *, complete=True) -> str:
    """Minimal OWNER_POLICY.md §3 stand-in for hermetic tests."""
    rows = [
        "| Primary benchmark | **Buy-and-hold BTC** over identical windows |",
        "| Comparison window | trailing 90 days |",
        "| Acceptance rule | rolling 6-month expectancy > 0 after all costs AND >= benchmark |",
    ]
    if not complete:
        rows = rows[:2]  # drop Acceptance rule -> must fail closed
    p = tmp_path / "OWNER_POLICY.md"
    p.write_text("\n".join(["## 3. Benchmark", "", "| Item | Decision |", "|---|---|"] + rows) + "\n", encoding="utf-8")
    return str(p)


def test_load_benchmark_parses_fields(tmp_path):
    bench = wr.load_benchmark(_policy(tmp_path))
    assert bench is not None
    assert "Buy-and-hold BTC" in bench["primary"]
    assert "90 days" in bench["window"]
    assert ">= benchmark" in bench["acceptance"]


def test_load_benchmark_fails_closed(tmp_path):
    assert wr.load_benchmark(tmp_path / "missing.md") is None
    assert wr.load_benchmark(_policy(tmp_path, complete=False)) is None


def test_render_includes_benchmark_registry_and_runs(tmp_path):
    runs, reg = _setup(tmp_path)
    report = wr.render("2026-W34", reg, runs, tmp_path / "autopsies", _policy(tmp_path))
    assert "- primary: **Buy-and-hold BTC**" in report
    assert "source: docs/OWNER_POLICY.md §3" in report
    assert "unset (1.1 pending)" not in report
    assert "| a-v1 |" in report
    assert "| feasibility-2026-08-19-funding-arb-v1 |" in report
    assert "Human notes" in report


def test_render_benchmark_unset_without_policy(tmp_path):
    runs, reg = _setup(tmp_path)
    missing = tmp_path / "no" / "policy.md"
    report = wr.render("2026-W34", reg, runs, tmp_path / "autopsies", missing)
    # ALP-8: "unset (1.1 pending)" is no longer rendered; the text now says
    # the file is missing, not "pending" — a missing policy is a bug, not a
    # pending decision.
    assert "unset" in report
    assert "OWNER_POLICY.md missing" in report


def test_alp_8_policy_exists_but_unparseable_is_flagged_as_bug(tmp_path):
    """ALP-8: when OWNER_POLICY.md exists but benchmark fields can't be
    parsed, the report must flag this as a code defect, not render 'unset'".
    """
    runs, reg = _setup(tmp_path)
    # Policy file exists but has no benchmark table.
    bad_policy = tmp_path / "OWNER_POLICY.md"
    bad_policy.write_text("# Empty policy\n", encoding="utf-8")
    report = wr.render("2026-W34", reg, runs, tmp_path / "autopsies", bad_policy)
    assert "unset" in report
    assert "BUG" in report
    assert "ALP-8" in report


def test_render_lists_autopsies(tmp_path):
    runs, reg = _setup(tmp_path)
    autops = tmp_path / "autopsies"
    autops.mkdir()
    (autops / "liq-fade-v1-2026-08-19.md").write_text("x", encoding="utf-8")
    report = wr.render("2026-W34", reg, runs, autops)
    assert "liq-fade-v1-2026-08-19.md" in report


def test_cli_append_only_and_bad_week_exit_2(tmp_path):
    import subprocess
    import sys
    from pathlib import Path

    runs, reg = _setup(tmp_path)
    script = Path(__file__).resolve().parents[1] / "run_weekly_review.py"
    out = tmp_path / "reviews"
    policy = _policy(tmp_path)
    base_cmd = [
        sys.executable,
        str(script),
        "--week",
        "2026-W34",
        "--registry",
        reg,
        "--runs",
        runs,
        "--out-dir",
        str(out),
        "--autopsies",
        str(tmp_path / "autopsies"),
        "--policy",
        policy,
    ]
    result = subprocess.run(base_cmd, text=True, capture_output=True, check=False)
    assert result.returncode == 0, result.stderr
    written = (out / "2026-W34.md").read_text(encoding="utf-8")
    assert "- primary:" in written
    again = subprocess.run(base_cmd, text=True, capture_output=True, check=False)
    assert again.returncode == 2
    assert "already exists" in again.stderr
    bad = subprocess.run(
        [
            sys.executable,
            str(script),
            "--week",
            "nonsense",
            "--registry",
            reg,
            "--runs",
            runs,
            "--out-dir",
            str(out),
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert bad.returncode == 2
