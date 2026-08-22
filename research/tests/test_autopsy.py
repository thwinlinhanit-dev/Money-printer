"""Phase 4.5 strategy autopsies: deterministic classification from journaled
run records (fixtures only)."""

import json

import pytest

import autopsy

RUNS_FIXTURE = [
    {
        "run_id": "r1",
        "kind": "sim",
        "data_from_ns": 1786694096476997520,
        "trades": 3,
        "expectancy": -969.517108,
        "stress_expectancy_2x": -1261.129685,
        "note": "worst fill context",
    },
    {
        "run_id": "r2",
        "kind": "wf_cross_day_grade",
        "corpus": ["20260814", "20260815"],
        "verdict": "OVERALL KILL via criterion #1",
    },
    {
        "run_id": "r3",
        "kind": "res4_event_study",
        "results": {},
    },
]


def _setup(tmp_path) -> tuple[str, str]:
    runs = tmp_path / "runs" / "index.jsonl"
    runs.parent.mkdir(parents=True)
    runs.write_text(
        "\n".join(json.dumps(r) for r in RUNS_FIXTURE) + "\n", encoding="utf-8"
    )
    reg = tmp_path / "registry.jsonl"
    reg.write_text(
        json.dumps(
            {
                "id": "liq-fade-v1",
                "run_ids": ["r1", "r2", "r3"],
                "state": "killed",
                "evidence": [],
            }
        )
        + "\n",
        encoding="utf-8",
    )
    return str(runs), str(reg)


def test_collect_uses_registry_run_ids_only(tmp_path):
    runs, reg = _setup(tmp_path)
    records = autopsy.collect("liq-fade-v1", reg, runs)
    assert [r["run_id"] for r in records] == ["r1", "r2", "r3"]


def test_collect_refuses_missing_run_id(tmp_path):
    runs, reg = _setup(tmp_path)
    with open(reg, "a", encoding="utf-8") as f:
        f.write(json.dumps({"id": "ghost", "run_ids": ["nope"]}) + "\n")
    with pytest.raises(ValueError, match="nope"):
        autopsy.collect("ghost", reg, runs)


def test_classify_economic_failure_when_base_already_negative(tmp_path):
    runs, reg = _setup(tmp_path)
    verdict = autopsy.classify(autopsy.collect("liq-fade-v1", reg, runs))
    assert verdict["class"] == "economic failure"
    assert "-969.5" in verdict["detail"]
    assert "cost share" in verdict["detail"]


def test_classify_excludes_zero_trade_runs_from_mean():
    records = [
        {
            "run_id": "a",
            "trades": 3,
            "expectancy": -969.5,
            "stress_expectancy_2x": -1261.1,
        },
        {"run_id": "b", "trades": 0, "expectancy": 0, "stress_expectancy_2x": 0},
        {
            "run_id": "c",
            "trades": 5,
            "expectancy": -30.5,
            "stress_expectancy_2x": -50.1,
        },
    ]
    verdict = autopsy.classify(records)
    # Zero-trade run b must not drag the mean; base is (-969.5 + -30.5)/2 = -500.0.
    assert "-500.0" in verdict["detail"]


def test_classify_execution_model_failure_when_costs_kill():
    records = [
        {"run_id": "a", "trades": 2, "expectancy": 50.0, "stress_expectancy_2x": -30.0}
    ]
    assert autopsy.classify(records)["class"] == "execution-model failure"


def test_classify_data_limitation_when_never_traded():
    records = [
        {"run_id": "a", "kind": "wf_min_trades", "verdict": "8/8 VACUOUS"},
        {"run_id": "b", "kind": "res4_event_study", "results": {}},
    ]
    verdict = autopsy.classify(records)
    assert verdict["class"] == "data limitation"


def test_render_contains_run_table_and_verdict(tmp_path):
    runs, reg = _setup(tmp_path)
    records = autopsy.collect("liq-fade-v1", reg, runs)
    verdict = autopsy.classify(records)
    report = autopsy.render("liq-fade-v1", records, verdict)
    assert "# Autopsy: liq-fade-v1" in report
    assert "| r1 |" in report
    assert "| 20260814 | 3 |" in report  # P&L by day row
    assert "worst fill context" in report
    assert "economic failure" in report


def test_cli_writes_append_only_autopsy_and_links_registry(tmp_path):
    import subprocess
    import sys
    from pathlib import Path

    runs, reg = _setup(tmp_path)
    script = Path(__file__).resolve().parents[1] / "run_autopsy.py"
    out = tmp_path / "autopsies"
    result = subprocess.run(
        [
            sys.executable,
            str(script),
            "liq-fade-v1",
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
    assert result.returncode == 0, result.stderr
    files = list(out.glob("*.md"))
    assert len(files) == 1
    # Registry evidence now links the autopsy (no orphan reports).
    rec = json.loads(
        [line for line in Path(reg).read_text(encoding="utf-8").splitlines() if line][0]
    )
    assert any("autopsies" in e for e in rec["evidence"])
    # Append-only: a second run the same day is refused.
    again = subprocess.run(
        [
            sys.executable,
            str(script),
            "liq-fade-v1",
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
    assert again.returncode == 2
    assert "already exists" in again.stderr
