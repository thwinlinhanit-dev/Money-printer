"""Honesty tests for the positioning-stress study runner (RES-4): confluence
event construction (funding x OI purge x liq pressure in the same hour),
E2 n_days / ci_reliable disclosure, E3 same-UTC-day session gating, and E7
append-only run-id duplicate rejection."""

import json

from run_positioning_stress import (
    HOUR_NS,
    LIQ_THRESH_USD,
    annualized_funding_bps,
    main as run_stress_main,
    study_positioning_stress,
)


def _carry_rows(oi: float, mark: float, funding_rate: float) -> dict:
    return {
        "total_oi": oi,
        "mark": mark,
        "index": mark,
        "basis_bps": 0.0,
        "funding_rate": funding_rate,
        "oi_unit": "usd",
        "venue": "hyperliquid",
    }


def _fund_rate_for_bps_yr(target_bps: float) -> float:
    """HL hourly funding rate that annualizes to ``target_bps`` bps/yr."""
    return target_bps / 1e4 / 8760.0


def test_stress_confluence_counts_same_hour_three_legs():
    """A single hour with funding stress + OI purge + one-sided liq pressure
    is ONE confluence event, tagged long_flush (funding>0, OI down, liq
    sell-dominant). Hours lacking any leg are not candidates."""
    ts = {h: h * HOUR_NS for h in range(24)}
    rate = _fund_rate_for_bps_yr(1000.0)  # >= 800 bps/yr corpus-tail band
    carry = {ts[h]: _carry_rows(1000.0, 100.0, rate) for h in range(24) if h != 10}
    carry[ts[10]] = _carry_rows(900.0, 100.0, rate)  # OI purge 10%
    liq = {ts[10]: -LIQ_THRESH_USD["BTCUSDT"] - 1.0}  # sell-dominant flush
    # Cross-asset excess map covering hour 10's full [-1h, +6h] window.
    ex = {ts[h]: 0.001 for h in range(9, 17)}
    results = study_positioning_stress(
        {"BTC": carry, "ETH": {}},
        {"BTCUSDT": liq, "ETHUSDT": {}},
        {"BTC": ex, "ETH": {}},
        seed=42,
    )
    btc = results["fund_ge_800bpsyr_oi_ge_1pct"]["BTCUSDT"]
    assert btc["n_candidates_raw"] == 1
    assert btc["n_events"] == 1
    assert btc["regimes"] == {"long_flush": 1, "short_squeeze": 0, "mixed": 0}


def test_stress_needs_all_three_legs():
    """A funding-stressed hour with NO liq pressure that hour is not a
    candidate — the confluence gate never fires on partial legs."""
    ts = {h: h * HOUR_NS for h in range(24)}
    rate = _fund_rate_for_bps_yr(1000.0)
    carry = {ts[h]: _carry_rows(1000.0, 100.0, rate) for h in range(24)}
    carry[ts[10]] = _carry_rows(900.0, 100.0, rate)  # OI purge, funding OK
    liq: dict[int, float] = {}  # but NO liquidation hour 10
    ex = {ts[h]: 0.001 for h in range(9, 17)}
    results = study_positioning_stress(
        {"BTC": carry, "ETH": {}},
        {"BTCUSDT": liq, "ETHUSDT": {}},
        {"BTC": ex, "ETH": {}},
        seed=42,
    )
    assert results["fund_ge_800bpsyr_oi_ge_1pct"]["BTCUSDT"]["n_candidates_raw"] == 0


def test_stress_absolute_band_reports_not_testable_with_reason():
    """Bands above the corpus's measured funding range journal an honest
    NOT TESTABLE reason instead of a silent zero (E6 spirit)."""
    ts = {h: h * HOUR_NS for h in range(24)}
    rate = _fund_rate_for_bps_yr(1000.0)
    carry = {ts[h]: _carry_rows(1000.0, 100.0, rate) for h in range(24)}
    results = study_positioning_stress(
        {"BTC": carry, "ETH": {}},
        {"BTCUSDT": {}, "ETHUSDT": {}},
        {"BTC": {}, "ETH": {}},
        seed=42,
    )
    btc = results["fund_ge_2000bpsyr_oi_ge_1pct"]["BTCUSDT"]
    assert btc["n_events"] == 0
    assert btc["n_days"] == 0
    assert btc["ci_reliable"] is False
    assert "NOT TESTABLE" in btc["reason"]
    assert "2000" in btc["reason"]


def test_stress_annualization_units():
    """Cadence-aware annualization: HL hourly funding -> bps/yr (units never
    mixed — the 800 bps/yr threshold reads in the same units)."""
    assert annualized_funding_bps(1e-4) == 8760.0
    assert annualized_funding_bps(-1e-4) == -8760.0


def test_stress_run_id_duplicate_refused_append_only(tmp_path):
    """E7/F8: a duplicate run-id is refused (exit 2) and never appended."""
    index = tmp_path / "index.jsonl"
    index.write_text(
        json.dumps({"run_id": "dup-id", "kind": "res4_event_study"}) + "\n",
        encoding="utf-8",
    )
    empty = tmp_path / "no-data"
    common = ["--runs-dir", str(tmp_path), "--data-raw", str(empty)]

    assert run_stress_main(common + ["--run-id", "dup-id"]) == 2
    assert len(index.read_text(encoding="utf-8").strip().splitlines()) == 1

    # A fresh run-id journals one append-only record carrying the E2 fields.
    assert run_stress_main(common + ["--run-id", "fresh-id"]) == 0
    lines = index.read_text(encoding="utf-8").strip().splitlines()
    assert len(lines) == 2
    rec = json.loads(lines[1])
    assert rec["run_id"] == "fresh-id"
    btc = rec["results"]["fund_ge_2000bpsyr_oi_ge_1pct"]["BTCUSDT"]
    assert btc["n_days"] == 0
    assert btc["ci_reliable"] is False
    assert "reason" in btc


def test_stress_event_window_helper():
    """E3: the same-UTC-day gate used by the study (a [-1h,+6h] window fits a
    day; a [-6h,+24h] window cannot)."""
    from run_positioning_stress import _window_within_utc_day

    noon = 12 * HOUR_NS
    assert _window_within_utc_day(noon, HOUR_NS, pre=1, post=6) is True
    assert _window_within_utc_day(noon, HOUR_NS, pre=6, post=24) is False


def test_stress_event_regime_slicing_tags():
    """Regime tags survive into the event list (long_flush / short_squeeze /
    mixed) so downstream reporting can slice by episode type."""
    ts = {h: h * HOUR_NS for h in range(24)}
    rate = _fund_rate_for_bps_yr(1000.0)
    carry = {ts[h]: _carry_rows(1000.0, 100.0, rate) for h in range(24)}
    # short-squeeze hour: funding<0, OI down, buy-dominant liq.
    carry[ts[5]] = _carry_rows(900.0, 100.0, -rate)
    liq = {ts[5]: LIQ_THRESH_USD["BTCUSDT"] + 1.0}
    ex = {ts[h]: 0.001 for h in range(4, 12)}
    results = study_positioning_stress(
        {"BTC": carry, "ETH": {}},
        {"BTCUSDT": liq, "ETHUSDT": {}},
        {"BTC": ex, "ETH": {}},
        seed=42,
    )
    btc = results["fund_ge_800bpsyr_oi_ge_1pct"]["BTCUSDT"]
    assert btc["n_candidates_raw"] == 1
    assert btc["regimes"] == {"long_flush": 0, "short_squeeze": 1, "mixed": 0}
