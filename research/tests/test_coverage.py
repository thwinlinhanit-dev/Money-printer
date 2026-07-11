"""RES-1 coverage checks — hand-verified gap math, refuse-vs-warn semantics.
Test names embed the requirement id (RES-1) for traceability (W-2)."""

import pytest

from mp_data import Coverage, CoverageGap, Interval


def test_res_1_coverage_finds_the_gap_between_covered_ranges():
    cov = Coverage([Interval(0, 100), Interval(200, 300)])
    assert cov.gaps(50, 250) == [Interval(100, 200)]
    assert not cov.is_complete(50, 250)
    # A window fully inside a covered range has no gaps.
    assert cov.is_complete(0, 100)
    assert cov.gaps(10, 90) == []


def test_res_1_leading_and_trailing_gaps_are_reported():
    cov = Coverage([Interval(100, 200)])
    # Query starts before coverage and ends after it ⇒ two gaps.
    assert cov.gaps(0, 300) == [Interval(0, 100), Interval(200, 300)]


def test_res_1_overlapping_inputs_are_merged():
    cov = Coverage([Interval(0, 100), Interval(50, 150), Interval(150, 160)])
    assert cov.covered == [Interval(0, 160)]


def test_res_1_refuse_raises_warn_returns_gaps():
    cov = Coverage([Interval(0, 100)])
    # warn: returns the gaps for the caller to log, no raise.
    assert cov.check(0, 200, mode="warn") == [Interval(100, 200)]
    # refuse: any gap is fatal (scheduled jobs).
    with pytest.raises(CoverageGap):
        cov.check(0, 200, mode="refuse")
    # Complete window passes in either mode.
    assert cov.check(0, 100, mode="refuse") == []


def test_res_1_bad_interval_and_mode_rejected():
    with pytest.raises(ValueError):
        Interval(100, 50)
    with pytest.raises(ValueError):
        Coverage([]).check(0, 10, mode="bogus")
