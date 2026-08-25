"""Accumulation detector grading tests (spec 045, RES-4 gate).

acc_5 verifies the event-study side of the detector: forward returns at
+4h/+12h/+24h computed from a fixture price series must match hand-computed
values exactly (grading.step semantics — no interpolation, no look-ahead).
"""

import pytest

from grading import Hit, forward_return

HOUR_NS = 3_600_000_000_000
DAY_NS = 86_400_000_000_000


def _fixture_prices() -> list[tuple[int, float]]:
    """Hourly closes around a hit at T0. Hand-computable:

    entry(asof T0)=100.0; exits: +4h=104.0, +12h=112.0, +24h=96.0.
    """
    return [
        (-1 * HOUR_NS, 99.0),
        (0 * HOUR_NS, 100.0),  # hit bar close — entry sample
        (2 * HOUR_NS, 101.5),
        (4 * HOUR_NS, 104.0),
        (8 * HOUR_NS, 108.0),
        (12 * HOUR_NS, 112.0),
        (18 * HOUR_NS, 105.0),
        (24 * HOUR_NS, 96.0),
    ]


def test_acc_5_forward_return_computation():
    prices = _fixture_prices()
    hit_ts = 0
    # Hand-computed returns from entry 100.0 to each horizon's exit sample.
    expected = {
        4 * HOUR_NS: 0.04,  # 104/100 − 1
        12 * HOUR_NS: 0.12,  # 112/100 − 1
        24 * HOUR_NS: -0.04,  # 96/100 − 1
    }
    for horizon_ns, want in expected.items():
        got = forward_return(prices, hit_ts, horizon_ns)
        assert got == pytest.approx(want, abs=1e-12), f"horizon={horizon_ns}"
    # The graded hit carries the same shape the detector emits (rule_id,
    # symbol, ts) so the study can slice by regime/asset per ACC-5.
    hit = Hit(rule="accumulation_detector", symbol="BTC", ts_ns=hit_ts)
    assert hit.rule == "accumulation_detector"
    assert forward_return(prices, hit.ts_ns, DAY_NS) == pytest.approx(-0.04)


def test_acc_5_incomplete_window_is_never_graded():
    """A series that stops before the horizon end grades None (PD-5 honesty):
    no silently-short horizons may inflate the n≥30 promotion corpus."""
    prices = _fixture_prices()[:-1]  # last close now +18h < +24h end
    assert forward_return(prices, 0, 24 * HOUR_NS) is None
    assert forward_return(prices, 0, 12 * HOUR_NS) == pytest.approx(0.12)


def test_acc_5_step_lookup_no_lookahead():
    """Exit uses the LAST price at or before ts+horizon (step semantics)."""
    prices = [
        (0, 100.0),
        (3 * HOUR_NS, 103.0),  # asof +4h (next print is after the end)
        (5 * HOUR_NS, 999.0),  # strictly AFTER the +4h end → ignored
    ]
    assert forward_return(prices, 0, 4 * HOUR_NS) == pytest.approx(0.03)
