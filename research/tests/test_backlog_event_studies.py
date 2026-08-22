"""Honesty fixes for the backlog event-study runner (RES-4): E2 n_days /
ci_reliable disclosure, E3 same-UTC-day session gating, E6 dead-loop removal,
E7 append-only run-id duplicate rejection."""

import json

from run_backlog_event_studies import (
    HOUR_NS,
    _existing_run_ids,
    _window_within_utc_day,
    main as run_studies_main,
    study_listing,
    study_oi_purge,
)


def _hour_row(oi: float, mark: float) -> dict:
    return {"total_oi": oi, "mark": mark, "basis_bps": 0.0}


def test_res_4_oi_purge_excludes_cross_day_events():
    """Regression (E3): an OI-drop hour whose [-6h,+24h] window crosses a
    UTC-day boundary must NOT be counted — the corpus is per-day sessions, so
    the previous day's bucket would be the OI baseline and the +24h window
    would contain the next day's data. Excluded events are reported honestly
    (raw detections preserved, counted = 0, reason present)."""
    # Two full UTC days of hourly buckets (hours 0..23 = day 1, 24..47 = day 2).
    ts = {h: h * HOUR_NS for h in range(48)}
    series = {ts[h]: _hour_row(1000.0, 100.0) for h in range(48)}
    # Purge at hour 24 (00:00 day 2): 'before' bucket (hour 23) is the
    # previous day's session AND the +24h window reaches into day 3.
    series[ts[23]] = _hour_row(2000.0, 100.0)
    series[ts[24]] = _hour_row(1000.0, 99.0)
    # Same-day-internal purge at hour 12: +24h window still crosses midnight.
    series[ts[11]] = _hour_row(2000.0, 100.0)
    series[ts[12]] = _hour_row(1000.0, 99.0)
    # Full excess map over a 60-hour span: windows would be complete if the
    # session-purity gate were absent, so the exclusion is the gate's doing.
    ex = {h * HOUR_NS: 0.0 for h in range(60)}
    # load_series always materializes both symbols (empty ETH is the honest
    # no-data leg here — the study iterates BTC and ETH by contract).
    results = study_oi_purge({"BTC": series, "ETH": {}}, ex, {}, seed=42)
    assert "reason" in results  # honest NOT TESTABLE reason journaled
    btc = results["oi_drop_1pct"]["BTC"]
    assert btc["n_events_raw"] == 2  # both detections fire pre-gate ...
    assert btc["n_events"] == 0  # ... but neither window fits one UTC day
    assert btc["n_days"] == 0
    assert btc["ci_reliable"] is False


def test_res_4_oi_purge_same_day_window_helper():
    noon = 12 * HOUR_NS
    # 31-bar window can never fit inside a 24h session.
    assert _window_within_utc_day(noon, HOUR_NS, pre=6, post=24) is False
    assert _window_within_utc_day(noon, HOUR_NS, pre=6, post=6) is True
    assert _window_within_utc_day(noon, HOUR_NS, pre=0, post=11) is True
    assert _window_within_utc_day(noon, HOUR_NS, pre=0, post=12) is False


def test_res_4_listing_study_returns_honest_reason_without_dead_loop():
    """E6: the listing study is the reason string — no dead loop over
    Listing events that cannot exist in the corpus."""
    assert study_listing() == {
        "n_events": 0,
        "reason": "no collector subscribes a listing feed; "
        "corpus has no Listing events by construction (spec 002/031)",
    }


def test_res_4_existing_run_ids_scans_tracker(tmp_path):
    index = tmp_path / "index.jsonl"
    index.write_text(
        "\n".join(
            [
                json.dumps({"run_id": "a", "kind": "res4_event_study"}),
                "not-json-this-line",  # malformed lines carry no identity
                json.dumps({"run_id": "b", "kind": "wf_min_trades"}),
                "",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    assert _existing_run_ids(tmp_path) == {"a", "b"}
    assert _existing_run_ids(tmp_path / "missing") == set()


def test_res_4_run_id_duplicate_refused_append_only(tmp_path):
    """E7/F8: a duplicate run-id is refused (exit 2) and never appended —
    even with --force (append-only tracker; overwrite is never allowed)."""
    index = tmp_path / "index.jsonl"
    index.write_text(
        json.dumps({"run_id": "dup-id", "kind": "res4_event_study"}) + "\n",
        encoding="utf-8",
    )
    empty = tmp_path / "no-data"  # no logs => studies journal honest zeros
    common = ["--runs-dir", str(tmp_path), "--data-raw", str(empty)]

    assert run_studies_main(common + ["--run-id", "dup-id"]) == 2
    assert run_studies_main(common + ["--run-id", "dup-id", "--force"]) == 2
    assert len(index.read_text(encoding="utf-8").strip().splitlines()) == 1

    # A fresh run-id journals one append-only record.
    assert run_studies_main(common + ["--run-id", "fresh-id"]) == 0
    lines = index.read_text(encoding="utf-8").strip().splitlines()
    assert len(lines) == 2
    rec = json.loads(lines[1])
    assert rec["run_id"] == "fresh-id"

    # The journaled record carries the E2 honesty fields: n_days,
    # ci_reliable, and the oi-purge NOT TESTABLE reason.
    oi = rec["results"]["oi_purge_continuation"]
    assert "reason" in oi
    assert oi["oi_drop_1pct"]["BTC"]["n_days"] == 0
    assert oi["oi_drop_1pct"]["BTC"]["ci_reliable"] is False
    assert rec["results"]["listing_flow"]["n_events"] == 0
