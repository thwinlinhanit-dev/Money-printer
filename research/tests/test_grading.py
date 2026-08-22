"""RES-2 screener grading and RES-3 edge-decay — hand-verified numbers."""

from pytest import approx

from grading import (
    Hit,
    RuleGrade,
    decay_flag,
    forward_return,
    grade_hits,
    leaderboard,
    recommendation,
)

MIN = 60_000_000_000  # 1 minute in ns


def test_res_2_forward_return_step_lookup_and_short_series_guard():
    prices = [(0, 100.0), (MIN, 110.0)]
    # +1m return: 110/100 - 1 = 0.10.
    assert forward_return(prices, 0, MIN) == approx(0.10)
    # Horizon beyond the last sample ⇒ None (no silently-shortened horizon).
    assert forward_return(prices, 0, 2 * MIN) is None
    # Missing entry side ⇒ None.
    assert forward_return(prices, -MIN, MIN) is None


def test_res_2_grade_hits_computes_excess_winrate_and_avg():
    prices = {"BTC": [(0, 100.0), (MIN, 110.0), (2 * MIN, 121.0)]}
    baseline = {"BTC": 0.05}  # symbol's do-nothing forward return
    hits = [Hit("pump", "BTC", 0), Hit("pump", "BTC", MIN)]
    grades = grade_hits(hits, prices, MIN, baseline)
    g = grades["pump"]
    # Both hits: +10% forward vs +5% baseline ⇒ +5% excess, both wins.
    assert g.n == 2
    assert g.wins == 2
    assert g.win_rate == 1.0
    assert abs(g.avg_excess - 0.05) < 1e-12


def test_res_2_leaderboard_ranks_by_avg_excess():
    prices = {"BTC": [(0, 100.0), (MIN, 110.0)], "ETH": [(0, 100.0), (MIN, 101.0)]}
    baseline = {"BTC": 0.0, "ETH": 0.0}
    hits = [Hit("strong", "BTC", 0), Hit("weak", "ETH", 0)]
    board = leaderboard(grade_hits(hits, prices, MIN, baseline))
    assert [g.rule for g in board] == ["strong", "weak"]
    assert board[0].avg_excess > board[1].avg_excess


def test_res_2_uncomputable_hits_are_skipped_not_counted():
    prices = {"BTC": [(0, 100.0)]}  # too short for any forward return
    hits = [Hit("pump", "BTC", 0), Hit("pump", "MISSING", 0)]
    grades = grade_hits(hits, prices, MIN, {"BTC": 0.0})
    # No computable returns ⇒ rule never enters the table (honest denominator).
    assert "pump" not in grades


def test_res_3_decay_flag_fires_on_fading_edge_only():
    # 8 strong weeks then 4 weak weeks: 4-wk mean well below half the 12-wk mean.
    fading = [0.10] * 8 + [0.01] * 4
    assert decay_flag(fading) is True
    # Steady positive edge ⇒ no decay.
    assert decay_flag([0.10] * 12) is False
    # Fewer than 12 weeks ⇒ not enough history to call it.
    assert decay_flag([0.01] * 4) is False
    # A never-positive rule is a kill decision, not a decay flag.
    assert decay_flag([-0.02] * 12) is False


def _grade(excesses: list[float]) -> RuleGrade:
    """RuleGrade consistent with its own excesses (win_rate = wins / n)."""
    return RuleGrade(
        rule="r",
        horizon_ns=MIN,
        n=len(excesses),
        wins=sum(1 for x in excesses if x > 0),
        sum_excess=sum(excesses),
        excesses=excesses,
    )


def test_grd_5_promotion_recommendation():
    # promote: hit rate >= 60% AND Sharpe > 1.0 (spec 017 GRD-3).
    assert recommendation(_grade([0.10, 0.11, 0.09, 0.10, 0.11])) == "promote"
    # demote: hit rate < 40%, not killed (Sharpe >= 0).
    assert recommendation(_grade([0.30, 0.02, -0.10, -0.08, -0.05, -0.01])) == "demote"
    # kill: hit rate < 20% (stricter than demote — kill wins).
    assert recommendation(_grade([0.10] + [-0.05] * 9)) == "kill"
    # kill: negative Sharpe AND hit rate < 50%.
    assert recommendation(_grade([0.01, -0.02, -0.03, 0.01, -0.01])) == "kill"
    # hold: everything else (high hit rate but Sharpe <= 1.0).
    assert recommendation(_grade([0.05, -0.02, 0.03, 0.01])) == "hold"


def test_grd_5_degenerate_variance_never_promotes():
    # All identical excesses: Sharpe degenerates to 0.0 (PD-5 — a rule with
    # no variance evidence is not promoted on a made-up Sharpe).
    g = _grade([0.05, 0.05, 0.05, 0.05])
    assert g.sharpe == 0.0
    assert recommendation(g) == "hold"


def test_grd_4_grading_produces_report(tmp_path):
    """GRD-5: the weekly job emits the machine {week}.json AND the
    human-readable {week}.md with a "Next stage recommendations" section."""
    from grading_job import run_weekly_grading

    prices = {"BTC": [(0, 100.0), (MIN, 110.0), (2 * MIN, 121.0)]}
    hits = [Hit("pump", "BTC", 0), Hit("pump", "BTC", MIN)]
    path, ran = run_weekly_grading(
        "2026-W29", hits, prices, MIN, {"BTC": 0.05}, tmp_path
    )
    assert ran and path.exists()
    report = tmp_path / "2026-W29.md"
    assert report.exists()
    text = report.read_text(encoding="utf-8")
    assert "# Screener grading report — 2026-W29" in text
    assert "| 1 | pump |" in text
    assert "## Next stage recommendations" in text
    # Recommendations are concrete per rule (GRD-3).
    assert any(line.startswith("- **") for line in text.splitlines())
