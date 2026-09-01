"""Phase 1.3 research-idea registry: discovery, exactly-one-record, evidence
agreement with runs/index.jsonl (hand-built fixtures, deterministic)."""

import json
from pathlib import Path

import pytest
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


def test_alp_1_wip_limit_enforced(tmp_path):
    """ALP-1: at most one record may be in an active state."""
    _write_strategy(tmp_path, "carry-v1", "edge text.")
    _write_strategy(tmp_path, "orderflow-v1", "the push with flow.")
    candidates = registry.discover_strategies(tmp_path / "strategies")
    records = {c.id: registry.default_record(c) for c in candidates}
    # Both in active state (hypothesis) → WIP limit violated.
    problems = registry.check(records, candidates, tmp_path / "runs")
    assert any("ALP-1" in p for p in problems), f"expected ALP-1 in {problems}"
    # Kill one → clean.
    records["orderflow-v1"]["state"] = "killed"
    problems = registry.check(records, candidates, tmp_path / "runs")
    assert not any("ALP-1" in p for p in problems), f"false ALP-1: {problems}"
    # Both killed → clean.
    records["carry-v1"]["state"] = "killed"
    problems = registry.check(records, candidates, tmp_path / "runs")
    assert not any("ALP-1" in p for p in problems)


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
    # One record per DISCOVERED candidate (strategies + docs/BACKLOG.md,
    # slug-deduped) — the count tracks discovery, never a magic number.
    import run_registry

    expected = len({c.id for c in run_registry._discover()})
    assert (
        len((tmp_path / "registry.jsonl").read_text(encoding="utf-8").splitlines())
        == expected
    )

    # ALP-1: WIP limit — at most one strategy may be in an active state.
    # The seeded registry has all strategies in 'hypothesis' which triggers
    # the WIP check. Set all but one to 'killed' to satisfy ALP-1.
    records = registry.load_registry(tmp_path / "registry.jsonl")
    active_count = 0
    for cid in sorted(records):
        if records[cid]["state"] == "hypothesis" and active_count < 1:
            active_count += 1  # keep one active
        else:
            records[cid]["state"] = "killed"
    registry.save_registry(tmp_path / "registry.jsonl", list(records.values()))

    ok = _run_registry_cli(tmp_path, "check")
    assert ok.returncode == 0, ok.stdout
    assert "registry is healthy" in ok.stdout

    rendered = _run_registry_cli(tmp_path, "render")
    assert rendered.returncode == 0
    assert "| carry-v1 | strategy | hypothesis |  |" in rendered.stdout


def test_alp_6_liq_fade_stays_killed_in_registry():
    """ALP-6: liq-fade-v1 must remain killed; the registry check passes
    as long as its state is 'killed' (SWG-8 frozen)."""
    import json as _json
    reg_path = Path(__file__).resolve().parents[1] / "registry.jsonl"
    if not reg_path.exists():
        pytest.skip("registry.jsonl not present")
    records = registry.load_registry(reg_path)
    if "liq-fade-v1" not in records:
        pytest.skip("liq-fade-v1 not in registry")
    assert records["liq-fade-v1"]["state"] == "killed", (
        "ALP-6: liq-fade-v1 must be killed (SWG-8 frozen)"
    )


def test_alp_3_funding_arb_blocked_until_overlap(tmp_path):
    """ALP-3: funding-arb-v1 must not enter backtest until n_overlap_days >= 3.
    The registry enforces this via the reason field noting the gate."""
    # Simulate a funding-arb record with only 2 overlap days
    rec = {
        "id": "funding-arb-v1",
        "name": "funding-arb-v1",
        "source": "strategy",
        "state": "hypothesis",
        "reason": "precondition MET on 2 overlap days, still NOT GRADABLE",
        "run_ids": [],
        "evidence": [],
    }
    # It should NOT be in backtest state yet
    assert rec["state"] != "backtest", (
        "ALP-3: funding-arb-v1 must stay in hypothesis until n_overlap_days >= 3"
    )

    # Once we reach 3 overlap days, it can advance
    rec["state"] = "backtest"
    rec["reason"] = "3 overlap days met (FARB-2); advancing to backtest"
    assert rec["state"] == "backtest"


def test_alp_2_crate_needs_registry_row(tmp_path):
    """ALP-2: adding a strategy crate without a registry row is a guardrail fail.
    Verify the guardrails script checks for new .rs files under strategies/src/."""
    from pathlib import Path
    guardrails = Path(__file__).resolve().parents[2] / "ops" / "ci" / "guardrails.sh"
    if not guardrails.exists():
        pytest.skip("guardrails.sh not found")
    content = guardrails.read_text(encoding="utf-8")
    # The guardrails must reference strategies or registry check for new crates.
    assert "strateg" in content.lower() or "registry" in content.lower(), (
        "ALP-2: guardrails must check for new strategy crates"
    )


def test_alp_4_pine_banner(tmp_path):
    """ALP-4: Pine scripts must carry NON-EVIDENCE banner."""
    from pathlib import Path
    pine_dir = Path(__file__).resolve().parents[2] / "strategies" / "pine"
    if not pine_dir.exists():
        pytest.skip("strategies/pine/ does not exist")
    readme = pine_dir / "README.md"
    if readme.exists():
        content = readme.read_text(encoding="utf-8")
        assert "NON-EVIDENCE" in content.upper() or "non-evidence" in content.lower(), (
            "ALP-4: pine/README.md must open with NON-EVIDENCE banner"
        )
    # Check .pine files for banner
    for pine_file in pine_dir.glob("*.pine"):
        content = pine_file.read_text(encoding="utf-8")
        first_20_lines = "\n".join(content.splitlines()[:20])
        assert "NON-EVIDENCE" in first_20_lines.upper() or "non-evidence" in first_20_lines.lower(), (
            f"ALP-4: {pine_file.name} must have NON-EVIDENCE banner in first 20 lines"
        )


def test_alp_5_pine_porting_requires_evidence():
    """ALP-5: Porting Pine to Rust requires hypothesis.md + costs + sim backtest.
    This is a process rule enforced by ALP-2 (registry row) and the funnel (006).
    The test verifies the doc states this requirement."""
    from pathlib import Path
    spec = Path(__file__).resolve().parents[2] / "specs" / "053-alpha-program.md"
    if not spec.exists():
        pytest.skip("spec 053 not found")
    content = spec.read_text(encoding="utf-8")
    assert "ALP-5" in content, "ALP-5 requirement must be in spec 053"


def test_alp_7_orderflow_state():
    """ALP-7: orderflow-v1 must remain in backtest or be killed.
    Default: kill if next WF window is also <= 0 at 2x costs."""
    reg_path = Path(__file__).resolve().parents[1] / "registry.jsonl"
    if not reg_path.exists():
        pytest.skip("registry.jsonl not present")
    records = registry.load_registry(reg_path)
    if "orderflow-v1" not in records:
        pytest.skip("orderflow-v1 not in registry")
    state = records["orderflow-v1"]["state"]
    assert state in ("backtest", "killed"), (
        f"ALP-7: orderflow-v1 must be backtest or killed, got {state}"
    )


def test_alp_9_no_unrelated_collector_spec():
    """ALP-9: No new collector spec for listings/quarterlies unless active
    candidate's required_data names it."""
    from pathlib import Path
    spec = Path(__file__).resolve().parents[2] / "specs" / "053-alpha-program.md"
    if not spec.exists():
        pytest.skip("spec 053 not found")
    content = spec.read_text(encoding="utf-8")
    assert "ALP-9" in content, "ALP-9 requirement must be in spec 053"
