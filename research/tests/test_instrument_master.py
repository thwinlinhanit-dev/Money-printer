"""Phase 3.3 point-in-time instrument master: resolution windows, fail-closed
corpus check (fixtures only, deterministic)."""

import json

import pytest

import instrument_master as im


def _write_master(tmp_path, lines: list[dict]) -> str:
    path = tmp_path / "master.jsonl"
    path.write_text(
        "\n".join(json.dumps(line) for line in lines) + "\n", encoding="utf-8"
    )
    return str(path)


BASE = {
    "venue": "bybit",
    "symbol": "BTCUSDT",
    "instrument_type": "perp",
    "contract_multiplier": 1.0,
    "tick_size": 0.1,
    "step_size_usd": 1.0,
    "quote_asset": "USDT",
    "margin_asset": "USDT",
    "funding_period_hours": 8,
    "funding_settlements_per_day": 3,
    "fee_tier": "VIP0",
    "taker_bps": 5.5,
    "maker_bps": 2.0,
    "observed_from_ns": im.parse_ts_ns("2026-08-14T00:00:00Z"),
    "observed_to_ns": None,
    "migration": None,
    "notes": "",
}


def _master_with_migration(tmp_path):
    v1 = {**BASE, "notes": "v1 before migration"}
    v2 = {
        **BASE,
        "taker_bps": 4.5,
        "migration": "bybit:BTCUSDT",
        "observed_from_ns": im.parse_ts_ns("2026-08-16T00:00:00Z"),
        "observed_to_ns": None,
        "notes": "v2 after migration",
    }
    return _write_master(tmp_path, [v1, v2])


def test_resolve_returns_in_force_definition(tmp_path):
    path = _master_with_migration(tmp_path)
    master = im.load_master(path)
    before = im.resolve(
        master, "bybit", "BTCUSDT", im.parse_ts_ns("2026-08-15T00:00:00Z")
    )
    after = im.resolve(
        master, "bybit", "BTCUSDT", im.parse_ts_ns("2026-08-17T00:00:00Z")
    )
    assert before.taker_bps == 5.5 and "v1" in before.notes
    assert after.taker_bps == 4.5 and "v2" in after.notes


def test_resolve_blocks_before_observation_and_unknown_symbol(tmp_path):
    path = _write_master(tmp_path, [BASE])
    master = im.load_master(path)
    with pytest.raises(im.InstrumentUnknown):
        im.resolve(master, "bybit", "BTCUSDT", im.parse_ts_ns("2026-08-01T00:00:00Z"))
    with pytest.raises(im.InstrumentUnknown):
        im.resolve(master, "bybit", "SOLUSDT", im.parse_ts_ns("2026-08-15T00:00:00Z"))


def test_resolve_honors_observed_to_delisting(tmp_path):
    delisted = {**BASE, "observed_to_ns": im.parse_ts_ns("2026-08-16T00:00:00Z")}
    path = _write_master(tmp_path, [delisted])
    master = im.load_master(path)
    ok = im.resolve(master, "bybit", "BTCUSDT", im.parse_ts_ns("2026-08-15T00:00:00Z"))
    assert ok.venue == "bybit"
    with pytest.raises(im.InstrumentUnknown):
        im.resolve(master, "bybit", "BTCUSDT", im.parse_ts_ns("2026-08-16T12:00:00Z"))


def test_corpus_check_lists_every_unresolved_file(tmp_path):
    raw = tmp_path / "raw"
    raw.mkdir()
    for name in (
        "20260815_bybit_BTCUSDT.log",
        "20260816_bybit_SOLUSDT.log",  # not in master -> must block
        "20260818_hyperliquid_positions.log",  # census, not an instrument
        "watchdog_20260816.log",  # not a market log
    ):
        (raw / name).write_bytes(b"")
    master = im.load_master(_write_master(tmp_path, [BASE]))
    logs = im.corpus_logs(raw)
    assert (len(logs)) == 2  # census + watchdog are ignored
    problems = im.check(master, logs)
    assert len(problems) == 1
    assert "bybit:SOLUSDT" in problems[0]


def test_ts_helpers_round_trip():
    ns = im.parse_ts_ns("2026-08-14T00:00:00Z")
    assert im.format_ts_ns(ns) == "2026-08-14"
    assert im.day_start_ns("20260814") == ns


def test_cli_check_exit_codes(tmp_path):
    import subprocess
    import sys
    from pathlib import Path

    script = Path(__file__).resolve().parents[1] / "run_instrument_master.py"
    raw = tmp_path / "raw"
    raw.mkdir()
    (raw / "20260815_bybit_BTCUSDT.log").write_bytes(b"")
    master_file = _write_master(tmp_path, [BASE])

    def run(*args):
        return subprocess.run(
            [
                sys.executable,
                str(script),
                "--master",
                master_file,
                *args,
            ],
            text=True,
            capture_output=True,
            check=False,
        )

    ok = run("check", "--raw", str(raw))
    assert ok.returncode == 0, ok.stderr

    (raw / "20260816_bybit_SOLUSDT.log").write_bytes(b"")
    blocked = run("check", "--raw", str(raw))
    assert blocked.returncode == 2
    assert "BLOCKED" in blocked.stdout

    resolved = run(
        "resolve",
        "--venue",
        "bybit",
        "--symbol",
        "BTCUSDT",
        "--at",
        "2026-08-15T00:00:00Z",
    )
    assert resolved.returncode == 0
    assert json.loads(resolved.stdout)["taker_bps"] == 5.5

    unknown = run(
        "resolve",
        "--venue",
        "bybit",
        "--symbol",
        "SOLUSDT",
        "--at",
        "2026-08-15T00:00:00Z",
    )
    assert unknown.returncode == 2
    assert "no instrument definition" in unknown.stderr
