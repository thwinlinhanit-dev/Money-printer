"""RES-4 event study — hand-verified CAR, seeded reproducible bootstrap CI,
regime slicing."""

from event_study import Event, bootstrap_ci, car, car_by_regime, run_study


def _excess():
    # bar_ns = 1; offsets -1,0,+1 around events at 10 and 20.
    return {
        9: 0.00,
        10: 0.02,
        11: 0.01,
        19: 0.00,
        20: 0.04,
        21: 0.03,
    }


def test_res4_car_averages_then_cumsums_across_events():
    events = [Event(10), Event(20)]
    curve = car(events, _excess(), bar_ns=1, pre=1, post=1)
    # per-offset avg = [(0+0)/2, (0.02+0.04)/2, (0.01+0.03)/2] = [0, 0.03, 0.02]
    # cumulative                                              = [0, 0.03, 0.05]
    assert curve == [0.0, 0.03, 0.05]


def test_res4_incomplete_windows_are_dropped():
    events = [Event(10), Event(999)]  # 999 has no bars in the map
    curve = car(events, _excess(), bar_ns=1, pre=1, post=1)
    # Only event 10 contributes: [0, 0.02, 0.03] cumulative.
    assert curve == [0.0, 0.02, 0.03]


def test_res4_bootstrap_ci_is_seeded_and_reproducible():
    events = [Event(10), Event(20)]
    ex = _excess()
    a = bootstrap_ci(events, ex, 1, 1, 1, seed=42, n_boot=500)
    b = bootstrap_ci(events, ex, 1, 1, 1, seed=42, n_boot=500)
    assert a == b  # CONV-11 determinism
    lo, hi = a
    assert lo <= hi
    # Different seed may differ but stays within the sample's min/max terminal.
    # terminals: event10 sum=0.03, event20 sum=0.07 ⇒ CI within [0.03, 0.07].
    assert 0.03 <= lo <= hi <= 0.07


def test_res4_regime_slicing_splits_by_tag():
    events = [Event(10, regime="chop"), Event(20, regime="trend")]
    by = car_by_regime(events, _excess(), 1, 1, 1)
    assert set(by.keys()) == {"chop", "trend"}
    # Each regime has a single event ⇒ its own CAR.
    assert by["chop"] == [0.0, 0.02, 0.03]
    assert by["trend"] == [0.0, 0.04, 0.07]


def test_res4_run_study_produces_record_with_ci_and_summary():
    events = [Event(10), Event(20)]
    rec = run_study("liq-cluster", events, _excess(), 1, 1, 1, seed=7, n_boot=200)
    assert rec.n_events == 2
    assert rec.car[-1] == 0.05
    assert rec.ci_lo <= rec.ci_hi
    assert "liq-cluster" in rec.summary()
    assert "seed=7" in rec.summary()
