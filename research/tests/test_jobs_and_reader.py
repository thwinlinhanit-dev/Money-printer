"""RES-1 reader coverage gate over real Parquet, RES-2 weekly job idempotency."""

import json

import polars as pl
import pytest

from grading import Hit
from grading_job import run_weekly_grading
from mp_data.reader import events

MIN = 60_000_000_000
DAY = 86_400_000_000_000


def _write_fixture(root, with_gap: bool):
    d = root / "trades" / "venue=bybit" / "symbol=BTCUSDT" / "date=2026-07-11"
    d.mkdir(parents=True)
    pl.DataFrame(
        {"recv_ts_ns": [10, 20, 30], "price": [100.0, 101.0, 99.5], "qty": [1.0, 1.0, 1.0]}
    ).write_parquet(d / "part-000.parquet")
    m = root / "manifests" / "venue=bybit"
    m.mkdir(parents=True)
    gaps = [{"from_ns": 1000, "to_ns": 1000 + DAY // 2}] if with_gap else []
    manifest = {
        "from_ns": 0,
        "to_ns": DAY,
        "streams": {"trades:BTCUSDT": {"events": 3, "gaps": gaps}},
    }
    (m / "date=2026-07-11.json").write_text(json.dumps(manifest), encoding="utf-8")


def test_res_1_reader_returns_frame_and_gaps_in_warn_mode(tmp_path):
    _write_fixture(tmp_path, with_gap=True)
    frame, gaps = events(tmp_path, "bybit", "BTCUSDT", "2026-07-11", mode="warn")
    assert frame.height == 3  # the data comes back ...
    assert len(gaps) == 1  # ... and the gap is surfaced, never hidden
    assert gaps[0].start == 1000


def test_res_1_reader_refuses_gapped_or_unmanifested_data(tmp_path):
    from mp_data import CoverageGap

    _write_fixture(tmp_path, with_gap=True)
    with pytest.raises(CoverageGap):
        events(tmp_path, "bybit", "BTCUSDT", "2026-07-11", mode="refuse")
    # A clean manifest passes refuse mode.
    clean = tmp_path / "clean"
    _write_fixture(clean, with_gap=False)
    frame, gaps = events(clean, "bybit", "BTCUSDT", "2026-07-11", mode="refuse")
    assert frame.height == 3 and gaps == []
    # No manifest at all ⇒ refused (unmanifested data is untrusted).
    with pytest.raises(CoverageGap):
        events(clean, "bybit", "BTCUSDT", "2026-07-12", mode="refuse")


def test_res_2_weekly_grading_job_is_idempotent_and_journals_leaderboard(tmp_path):
    prices = {"BTC": [(0, 100.0), (MIN, 110.0)], "ETH": [(0, 100.0), (MIN, 101.0)]}
    hits = [Hit("strong", "BTC", 0), Hit("weak", "ETH", 0)]
    baseline = {"BTC": 0.0, "ETH": 0.0}

    path, ran = run_weekly_grading("2026-W28", hits, prices, MIN, baseline, tmp_path)
    assert ran and path.exists()
    payload = json.loads(path.read_text(encoding="utf-8"))
    assert payload["rules"][0]["rule"] == "strong"  # leaderboard order

    journal = (tmp_path / "leaderboard.jsonl").read_text(encoding="utf-8")
    assert len(journal.strip().splitlines()) == 2

    # Re-firing the same week is a no-op: no rewrite, no double journal (RES-2).
    _, ran2 = run_weekly_grading("2026-W28", hits, prices, MIN, baseline, tmp_path)
    assert not ran2
    journal2 = (tmp_path / "leaderboard.jsonl").read_text(encoding="utf-8")
    assert journal == journal2
