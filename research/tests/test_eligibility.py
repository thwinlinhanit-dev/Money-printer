"""Tests for the data-eligibility selection layer (roadmap 2.3-2.4, RES-1)."""

from __future__ import annotations

import json

import run_eligibility
from mp_data.eligibility import (
    EligibilityRule,
    all_dates,
    all_universe,
    load_scorecards,
    select,
)


def _write_scorecard(dir_, date, recordings):
    doc = {"date": date, "recordings": recordings}
    (dir_ / f"{date}.json").write_text(json.dumps(doc), encoding="utf-8")


def _hl(clean, coverage, stale=0, gap=None):
    r = {
        "venue": "hyperliquid",
        "symbol": "BTC",
        "clean": clean,
        "event_count": 1000,
        "coverage": coverage,
        "findings": 1,
        "blocking_findings": 0 if clean else 1,
    }
    if stale is not None:
        r["stale_bursts"] = stale
    if gap is not None:
        r["worst_gap_ns"] = gap
    return r


def test_load_tolerates_old_schema_missing_keys(tmp_path):
    # 2026-07-18-style card: no worst_gap_ns / stale_bursts keys.
    _write_scorecard(tmp_path, "2026-07-18", [_hl(False, 1.0, stale=None)])
    grades = load_scorecards(tmp_path)
    g = grades[("hyperliquid", "BTC")]["2026-07-18"]
    assert g.clean is False
    assert g.coverage == 1.0
    assert g.stale_bursts is None
    assert g.worst_gap_ns is None


def test_load_skips_non_scorecard_and_auxiliary_files(tmp_path):
    (tmp_path / ".scorecard_sources.json").write_text("{}")
    (tmp_path / "2026-08-17_bursts.json").write_text("{}")
    (tmp_path / "2026-08-17.determinism.json").write_text("{}")
    (tmp_path / "pipeline.log").write_text("x")
    _write_scorecard(tmp_path, "2026-08-17", [_hl(False, 0.98)])
    grades = load_scorecards(tmp_path)
    assert all_universe(grades) == (("hyperliquid", "BTC"),)
    assert all_dates(grades) == ("2026-08-17",)


def test_select_eligible_backed_by_clean_high_coverage(tmp_path):
    _write_scorecard(tmp_path, "2026-08-13", [_hl(True, 1.0, stale=0)])
    grades = load_scorecards(tmp_path)
    rep = select(grades, [("hyperliquid", "BTC")], ["2026-08-13"])
    assert rep.eligible_count() == 1
    assert rep.excluded_count() == 0


def test_select_excludes_dirty_and_low_coverage_with_reasons(tmp_path):
    _write_scorecard(tmp_path, "2026-08-17", [_hl(False, 0.978, stale=4)])
    grades = load_scorecards(tmp_path)
    rep = select(grades, [("hyperliquid", "BTC")], ["2026-08-17"])
    assert rep.eligible_count() == 0
    assert len(rep.excluded) == 1
    e = rep.excluded[0]
    assert e.present is True
    assert any("clean=false" in r for r in e.reasons)
    assert any("coverage" in r for r in e.reasons)
    assert any("stale_bursts" in r for r in e.reasons)


def test_ungraded_day_is_excluded_as_blind(tmp_path):
    # No scorecard for the day at all -> ungraded/blind, not silently admitted.
    _write_scorecard(tmp_path, "2026-08-13", [_hl(True, 1.0)])
    grades = load_scorecards(tmp_path)
    rep = select(grades, [("hyperliquid", "ETH")], ["2026-08-14"])
    assert rep.eligible_count() == 0
    assert len(rep.excluded) == 1
    assert rep.excluded[0].present is False
    assert "no scorecard" in rep.excluded[0].reasons[0]


def test_require_clean_off_allows_dirty_but_low_coverage_still_excluded(tmp_path):
    _write_scorecard(tmp_path, "2026-08-17", [_hl(False, 0.978)])
    grades = load_scorecards(tmp_path)
    rule = EligibilityRule(require_clean=False)
    rep = select(grades, [("hyperliquid", "BTC")], ["2026-08-17"], rule)
    # Dirty is allowed, but coverage still 0.978 < 0.995 -> excluded.
    assert rep.eligible_count() == 0
    reasons = rep.excluded[0].reasons
    assert not any("clean=false" in r for r in reasons)
    assert any("coverage" in r for r in reasons)


def test_embed_lists_every_exclusion_in_run_record(tmp_path):
    _write_scorecard(tmp_path, "2026-08-17", [_hl(False, 0.978, stale=4)])
    grades = load_scorecards(tmp_path)
    rep = select(grades, [("hyperliquid", "BTC")], ["2026-08-17"])
    rec = rep.embed(run_id="run-123", git_sha="abc", source=str(tmp_path))
    assert rec["run_id"] == "run-123"
    assert rec["eligible_days"] == 0
    assert rec["excluded_days"] == 1
    assert rec["excluded"][0]["date"] == "2026-08-17"
    assert rec["excluded"][0]["reasons"]
    # JSON-serializable fragment (what would be appended to runs/index.jsonl).
    json.dumps(rec)


def test_cli_fail_closed_when_no_eligible(tmp_path):
    _write_scorecard(tmp_path, "2026-08-17", [_hl(False, 0.978)])
    rc = run_eligibility.main(
        ["--scorecards-dir", str(tmp_path), "--universe", "hyperliquid:BTC", "--json"]
    )
    assert rc == 2


def test_cli_appends_and_refuses_duplicate_run_id(tmp_path, capfd):
    _write_scorecard(tmp_path, "2026-08-13", [_hl(True, 1.0)])
    runs = tmp_path / "runs"
    argv = [
        "--scorecards-dir",
        str(tmp_path),
        "--universe",
        "hyperliquid:BTC",
        "--run-id",
        "elig-test-1",
        "--runs-dir",
        str(runs),
    ]
    assert run_eligibility.main(argv) == 0
    # Appending the same run-id again is refused (append-only tracker).
    assert run_eligibility.main(argv) == 2
    lines = [
        json.loads(line)
        for line in (runs / "index.jsonl").read_text().splitlines()
        if line
    ]
    assert len(lines) == 1
    assert lines[0]["kind"] == "data_eligibility"
    assert lines[0]["excluded_days"] == 0
    assert lines[0]["eligible_days"] == 1
