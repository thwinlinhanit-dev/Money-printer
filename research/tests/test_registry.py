"""Phase 1.3 research-idea registry: discovery, exactly-one-record, evidence
agreement with runs/index.jsonl (hand-built fixtures, deterministic)."""

import json


import registry

BACKLOG_FIXTURE = """# Backlog - every idea

## Strategies & alpha (each needs hypothesis.md first - spec 006)
- **[v1.x] orderflow-v1 (FIRST BACKTEST 2026-08-13)** - depth-gauge +
  tape-bps_delta alignment continuation trade.
- **[v1.x] funding-arb-v1** - cross-venue funding spread, carry-v1's sibling.
- **[v2] vol/options overlay (Deribit)** - options collector spec first.
- **[maybe-never] sub-second HFT anything** - blueprint section 2 stands.
- **[v1.x] funding-carry study (DELIVERED 2026-08-13)** - delivered research.

## Data & features
- **[v1.x] read-time analytics transforms** - delivered, not a candidate.

## Explicitly rejected (don't re-propose without new evidence)
- **Copy trading** - mirroring individual whale wallets blind.
"""


def _write_strategy(tmp_path, name: str, edge: str) -> None:
    d = tmp_path / "strategies" / name
    d.mkdir(parents=True)
    (d / "hypothesis.md").write_text(
        f"# {name} - Hypothesis\n\n## Edge: what inefficiency\n{edge}\n",
        encoding="utf-8",
    )


def test_backlog_discovery_handles_parentheticals_and_closing_stars(tmp_path):
    backlog = tmp_path / "BACKLOG.md"
    backlog.write_text(BACKLOG_FIXTURE, encoding="utf-8")
    found = {c.id: c.name for c in registry.discover_backlog(backlog)}
    # Parenthetical date markers are stripped -> stable ids, no duplicate
    # records when the same idea also exists as a strategy.
    assert found["orderflow-v1"] == "orderflow-v1"
    assert found["funding-arb-v1"] == "funding-arb-v1"
    assert found["vol-options-overlay"] == "vol/options overlay"
    assert found["sub-second-hft-anything"] == "sub-second HFT anything"
    assert found["funding-carry-study"] == "funding-carry study"


def test_strategy_discovery_uses_hypothesis_header(tmp_path):
    _write_strategy(tmp_path, "carry-v1", "funding extremes when the crowd leans.")
    found = registry.discover_strategies(tmp_path / "strategies")
    assert [(c.id, c.source, c.default_state) for c in found] == [
        ("carry-v1", "strategy", "hypothesis")
    ]


def test_one_idea_one_record_across_sources(tmp_path):
    _write_strategy(tmp_path, "orderflow-v1", "the push with flow behind it.")
    backlog = tmp_path / "BACKLOG.md"
    backlog.write_text(BACKLOG_FIXTURE, encoding="utf-8")
    candidates = registry.discover_strategies(
        tmp_path / "strategies"
    ) + registry.discover_backlog(backlog)
    ids = {c.id for c in candidates}
    assert "orderflow-v1" in ids  # strategy + backlog bullet -> one id
    records = {c.id: registry.default_record(c) for c in candidates}
    problems = registry.check(records, candidates, tmp_path / "runs" / "index.jsonl")
    assert problems == []


def test_check_flags_missing_record_invalid_state_and_unknown_run_id(tmp_path):
    _write_strategy(tmp_path, "carry-v1", "edge text.")
    candidates = registry.discover_strategies(tmp_path / "strategies")
    records = {
        "carry-v1": {
            **registry.default_record(candidates[0]),
            "state": "not-a-state",
            "run_ids": ["ghost-run-9"],
        }
    }
    problems = registry.check(records, candidates, tmp_path / "runs" / "index.jsonl")
    joined = "\n".join(problems)
    assert "invalid state" in joined
    assert "ghost-run-9 not found" in joined


def test_check_accepts_run_ids_present_in_tracker(tmp_path):
    _write_strategy(tmp_path, "liq-fade-v1", "forced flows overshoot price.")
    runs = tmp_path / "runs"
    runs.mkdir()
    (runs / "index.jsonl").write_text(
        json.dumps({"run_id": "01M01A640WJBGD9NTX7MP2Q61R"}) + "\n", encoding="utf-8"
    )
    candidates = registry.discover_strategies(tmp_path / "strategies")
    rec = registry.default_record(candidates[0])
    rec["state"] = "killed"
    rec["run_ids"] = ["01M01A640WJBGD9NTX7MP2Q61R"]
    problems = registry.check({"liq-fade-v1": rec}, candidates, runs / "index.jsonl")
    assert problems == []


def test_check_flags_orphan_records(tmp_path):
    _write_strategy(tmp_path, "carry-v1", "edge text.")
    candidates = registry.discover_strategies(tmp_path / "strategies")
    orphan = registry.default_record(candidates[0])
    orphan["id"] = "ghost-idea"
    problems = registry.check({"ghost-idea": orphan}, candidates, tmp_path / "runs")
    assert any("no matching candidate" in p for p in problems)


def test_seed_only_adds_missing_records(tmp_path):
    _write_strategy(tmp_path, "carry-v1", "edge text.")
    backlog = tmp_path / "BACKLOG.md"
    backlog.write_text(BACKLOG_FIXTURE, encoding="utf-8")
    candidates = registry.discover_strategies(
        tmp_path / "strategies"
    ) + registry.discover_backlog(backlog)
    reg_path = tmp_path / "registry.jsonl"
    records = {c.id: registry.default_record(c) for c in candidates}
    registry.save_registry(reg_path, list(records.values()))
    records["carry-v1"]["state"] = "killed"  # curated state must survive seeding
    registry.save_registry(reg_path, list(records.values()))
    reloaded = registry.load_registry(reg_path)
    assert reloaded["carry-v1"]["state"] == "killed"
    assert set(reloaded) == {c.id for c in candidates}


def test_render_is_markdown_table(tmp_path):
    _write_strategy(tmp_path, "carry-v1", "edge text.")
    candidates = registry.discover_strategies(tmp_path / "strategies")
    records = {c.id: registry.default_record(c) for c in candidates}
    table = registry.render(records)
    assert "| id | source | state | reason |" in table
    assert "| carry-v1 | strategy | hypothesis |  |" in table


def _run_registry_cli(tmp_path, *args):
    import subprocess
    import sys
    from pathlib import Path

    script = Path(__file__).resolve().parents[1] / "run_registry.py"
    return subprocess.run(
        [
            sys.executable,
            str(script),
            "--registry",
            str(tmp_path / "registry.jsonl"),
            *args,
        ],
        text=True,
        capture_output=True,
        check=False,
    )


def test_cli_seed_check_render_round_trip(tmp_path):
    # An empty registry fails the check (missing records, exit 1).
    bad = _run_registry_cli(tmp_path, "check")
    assert bad.returncode == 1
    assert "PROBLEM" in bad.stdout

    seed = _run_registry_cli(tmp_path, "seed")
    assert seed.returncode == 0, seed.stderr
    assert "seeded" in seed.stdout

    # Seeding again never overwrites curated records (idempotent scaffold).
    again = _run_registry_cli(tmp_path, "seed")
    assert "nothing added" in again.stdout
    assert (
        len((tmp_path / "registry.jsonl").read_text(encoding="utf-8").splitlines())
        == 14
    )

    ok = _run_registry_cli(tmp_path, "check")
    assert ok.returncode == 0, ok.stdout
    assert "registry is healthy" in ok.stdout

    rendered = _run_registry_cli(tmp_path, "render")
    assert rendered.returncode == 0
    assert "| carry-v1 | strategy | hypothesis |  |" in rendered.stdout
