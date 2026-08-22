"""Phase 4.1 economic-feasibility gate: hand-computed numbers, deterministic."""

import json

import pytest

from feasibility import Leg, evaluate, from_spec, hash_spec

# Single directional leg, bybit-perp-shaped: fee 5 bps/side, spread 1 bps,
# slippage 2 bps/fill, paying funding 1 bps per 8h settlement (3/day).
# Round trip = 2*5 + 1 + 2*2 = 15 bps; carry = 3 bps/day.
SINGLE = Leg(
    "BTCUSDT perp long",
    notional_usd=10_000,
    fee_bps=5.0,
    spread_bps=1.0,
    slippage_bps=2.0,
    funding_bps_per_settlement=1.0,
    settlements_per_day=3.0,
)


def test_hand_computed_break_even_and_move():
    res = evaluate([SINGLE], max_hold_days=2.0)
    assert res.rt_cost_bps == 15.0
    assert res.carry_bps_per_day == 3.0
    assert res.breakeven_days == pytest.approx(5.0)
    assert res.min_move_bps == pytest.approx(21.0)  # 15 + 3*2
    assert res.verdict == "REJECTS_BASE"


def test_stressed_math_is_hand_computed():
    res = evaluate([SINGLE], max_hold_days=10.0)
    assert res.rt_cost_bps_stressed == pytest.approx(22.0)  # 2*6.25+1.5+2*4
    assert res.breakeven_days_stressed == pytest.approx(22.0 / 3.0)
    assert res.verdict == "CLEARS_BASE_AND_STRESSED"


def test_clears_base_only_when_stress_breaks_horizon():
    res = evaluate([SINGLE], max_hold_days=6.0)
    assert res.breakeven_days == pytest.approx(5.0)
    assert res.breakeven_days_stressed == pytest.approx(7.333333333333333)
    assert res.verdict == "CLEARS_BASE_ONLY"


def test_no_carry_never_breaks_even_by_holding():
    res = evaluate([SINGLE], max_hold_days=30.0)
    no_carry = Leg(
        SINGLE.label,
        SINGLE.notional_usd,
        fee_bps=SINGLE.fee_bps,
        spread_bps=SINGLE.spread_bps,
        slippage_bps=SINGLE.slippage_bps,
    )
    res = evaluate([no_carry], max_hold_days=30.0)
    assert res.breakeven_days is None
    assert res.verdict == "REJECTS_BASE"
    assert "no net carry" in res.reason
    assert "15.0 bps" in res.reason  # min move = rt at zero carry


def test_reject_message_never_claims_signal_is_false():
    res = evaluate([SINGLE], max_hold_days=2.0)
    assert res.verdict == "REJECTS_BASE"
    assert "cannot afford to discover whether the signal is true" in res.reason


def test_two_leg_funding_arb_hand_numbers():
    hl = Leg(
        "hyperliquid BTC perp (funding receiver)",
        notional_usd=100_000,
        fee_bps=4.5,
        spread_bps=0.5,
        slippage_bps=2.0,
        funding_bps_per_settlement=-1.0,  # we receive 1 bps/8h (the +1095 cap)
        settlements_per_day=3.0,
    )
    by = Leg(
        "bybit BTCUSDT perp (funding payer)",
        notional_usd=100_000,
        fee_bps=5.5,
        spread_bps=0.5,
        slippage_bps=2.0,
        funding_bps_per_settlement=1.0 / 73.0,  # ~15 bps/yr realized (x1095)
        settlements_per_day=3.0,
    )
    res = evaluate([hl, by], max_hold_days=30.0)
    # RT = 2*(4.5+5.5) + (0.5+0.5) + 2*(2+2) = 29 bps
    assert res.rt_cost_bps == pytest.approx(29.0)
    # carry = (-1 + 1/73)*3 = -2.959 (net receiving)
    assert res.carry_bps_per_day == pytest.approx(-2.958904109589041)
    # breakeven = 29 / 2.9589 = 9.80 days; stressed RT = 42.5 -> 14.36 days
    assert res.breakeven_days == pytest.approx(9.800925925925925)
    assert res.breakeven_days_stressed == pytest.approx(14.363425925925926)
    assert res.min_move_bps == pytest.approx(-59.76712328767123)
    assert res.verdict == "CLEARS_BASE_AND_STRESSED"


def test_from_spec_parses_and_validates():
    spec = {
        "legs": [
            {
                "label": "perp",
                "notional_usd": 1000,
                "fee_bps": 5.0,
                "funding_bps_per_settlement": 1.0,
                "settlements_per_day": 3.0,
            }
        ],
        "max_hold_days": 7,
    }
    legs, hold = from_spec(spec)
    assert len(legs) == 1 and legs[0].label == "perp"
    assert hold == 7.0


def test_from_spec_refuses_nonsense():
    with pytest.raises(ValueError, match="max_hold_days"):
        from_spec({"legs": [{"label": "x", "notional_usd": 1}]})
    with pytest.raises(ValueError, match="notional"):
        from_spec({"legs": [{"label": "x", "notional_usd": 0}], "max_hold_days": 1})
    with pytest.raises(ValueError, match="cadence"):
        from_spec(
            {
                "legs": [
                    {
                        "label": "x",
                        "notional_usd": 1,
                        "funding_bps_per_settlement": 1.0,
                    }
                ],
                "max_hold_days": 1,
            }
        )
    with pytest.raises(ValueError, match="unknown fields"):
        from_spec(
            {
                "legs": [{"label": "x", "notional_usd": 1, "yolo_bps": 1}],
                "max_hold_days": 1,
            }
        )


def test_hash_spec_is_deterministic_and_sensitive():
    spec = {"legs": [{"label": "a", "notional_usd": 1}], "max_hold_days": 1}
    assert hash_spec(spec) == hash_spec(spec)
    assert hash_spec(spec) != hash_spec({**spec, "max_hold_days": 2})


SPEC_FILE = {
    "candidate": "funding-arb-v1",
    "max_hold_days": 30,
    "notes": "test",
    "legs": [
        {
            "label": "hl",
            "notional_usd": 100000,
            "fee_bps": 4.5,
            "funding_bps_per_settlement": -1.0,
            "settlements_per_day": 3.0,
        },
        {
            "label": "bybit",
            "notional_usd": 100000,
            "fee_bps": 5.5,
            "funding_bps_per_settlement": 0.0136986301369863,
            "settlements_per_day": 3.0,
        },
    ],
}


def _run_cli(tmp_path, *extra):
    import subprocess
    import sys
    from pathlib import Path

    script = Path(__file__).resolve().parents[1] / "run_feasibility.py"
    spec = tmp_path / "spec.json"
    spec.write_text(json.dumps(SPEC_FILE), encoding="utf-8")
    return subprocess.run(
        [sys.executable, str(script), "--spec", str(spec), *extra],
        text=True,
        capture_output=True,
        check=False,
    )


def test_cli_journals_run_record_and_refuses_duplicate(tmp_path):
    import json as _json

    runs = tmp_path / "runs"
    result = _run_cli(
        tmp_path, "--run-id", "feasibility-cli-test-1", "--runs-dir", str(runs)
    )
    assert result.returncode == 0, result.stderr
    payload = _json.loads(result.stdout)
    assert payload["verdict"] == "CLEARS_BASE_AND_STRESSED"
    record = _json.loads((runs / "index.jsonl").read_text(encoding="utf-8").strip())
    assert record["kind"] == "feasibility"
    # fixture legs carry only fees: RT = 2*(4.5+5.5) = 20 bps
    assert record["breakeven_days"] == pytest.approx(20.0 / 2.958904109589041)
    # Same run-id again: refused (E7, exit 2), tracker untouched.
    again = _run_cli(
        tmp_path, "--run-id", "feasibility-cli-test-1", "--runs-dir", str(runs)
    )
    assert again.returncode == 2
    assert "duplicate run-id" in again.stderr
    assert len((runs / "index.jsonl").read_text(encoding="utf-8").splitlines()) == 1


def test_cli_updates_registry_record_and_refuses_unknown_candidate(tmp_path):
    import json as _json

    registry_file = tmp_path / "registry.jsonl"
    registry_file.write_text(
        _json.dumps(
            {
                "id": "funding-arb-v1",
                "economic_feasibility": "not evaluated",
                "costs": "",
            }
        )
        + "\n",
        encoding="utf-8",
    )
    result = _run_cli(
        tmp_path, "--candidate", "funding-arb-v1", "--registry", str(registry_file)
    )
    assert result.returncode == 0, result.stderr
    rec = _json.loads(registry_file.read_text(encoding="utf-8").strip())
    assert rec["economic_feasibility"].startswith("CLEARS_BASE_AND_STRESSED")
    assert "RT 20.0 bps base / 25.0 bps stressed" in rec["costs"]
    missing = _run_cli(
        tmp_path, "--candidate", "ghost-idea", "--registry", str(registry_file)
    )
    assert missing.returncode == 2
    assert "no registry record" in missing.stderr
