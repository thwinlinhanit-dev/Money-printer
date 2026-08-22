"""Phase 3.1 historical panel: manifest rebuild determinism and membership
(offline file:// fixtures - no network in tests)."""

import io
import json
import zipfile
from pathlib import Path

import pytest

import panel as pnl

HEADER = "open_time,open,high,low,close,volume,close_time,quote_volume,count,taker_buy_base,taker_buy_quote,ignore\n"


def _kline_zip(rows: list[tuple[int, float, float]]) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        lines = [HEADER]
        for ts, close, qv in rows:
            lines.append(
                f"{ts},{close},{close},{close},{close},1,{ts + 60000},{qv},1,0.5,{qv / 2},0\n"
            )
        zf.writestr("BTCUSDT-1m-test.csv", "".join(lines))
    return buf.getvalue()


def _write_fixture_root(tmp_path: Path, files: dict[str, bytes]) -> str:
    root = tmp_path / "root"
    for rel, data in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
    return root.as_uri()


def _universe(tmp_path: Path, days: list[str], daily_threshold_usd: float = 5.0) -> str:
    path = tmp_path / "universe.json"
    path.write_text(
        json.dumps(
            {
                "universe_version": "test-v1",
                "downloader_version": "test-1",
                "source": "fixture",
                "source_timezone": "UTC",
                "fidelity_label": "external_archive:fixture",
                "inclusion_rule": "fixture rule",
                "liquidity_threshold_usd": 1000.0,
                "daily_threshold_usd": daily_threshold_usd,
                "delisting_rule": "fixture",
                "partition": {
                    "train": [],
                    "validation": [],
                    "test": [],
                    "rule": "fixture",
                },
                "notes": "",
                "symbols": [
                    {
                        "venue": "binance",
                        "symbol": "BTCUSDT",
                        "start": days[0],
                        "end": days[-1],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    return str(path)


def test_kline_stats_counts_rows_and_quote_volume():
    data = _kline_zip([(1000, 100.0, 200.0), (1060, 101.0, 300.0)])
    rows, volume = pnl._kline_stats(data)
    assert rows == 2
    assert volume == pytest.approx(500.0)


def test_download_to_manifest_round_trip(tmp_path):
    day = "2026-08-10"
    rel = f"data/futures/um/daily/klines/BTCUSDT/1m/BTCUSDT-1m-{day}.zip"
    base = _write_fixture_root(tmp_path, {rel: _kline_zip([(1000, 100.0, 42.0)])})
    uni = pnl.load_universe(_universe(tmp_path, [day]))
    data, url, rows, volume = pnl.fetch_kline(base, "binance", "BTCUSDT", day)
    assert rows == 1 and volume == pytest.approx(42.0)
    rec = pnl.membership_record(
        uni, "binance", "BTCUSDT", day, (data, url, rows, volume)
    )
    assert rec["status"] == "ok"
    assert rec["zip_sha256"] == pnl._sha256_bytes(data)
    assert rec["membership_eligible"] is True  # 42.0 >= daily threshold 5.0
    pnl.append_manifest(tmp_path, uni.universe_version, [rec])
    manifest = pnl.load_manifest(tmp_path, uni.universe_version)
    assert ("binance", "BTCUSDT", day) in manifest


def test_missing_file_recorded_with_reason_not_silently_dropped(tmp_path):
    base = _write_fixture_root(tmp_path, {})
    uni = pnl.load_universe(_universe(tmp_path, ["2026-08-10"]))
    with pytest.raises(pnl.PanelError):
        pnl.fetch_kline(base, "binance", "BTCUSDT", "2026-08-10")
    rec = pnl.membership_record(uni, "binance", "BTCUSDT", "2026-08-10", None)
    assert rec["status"] == "missing"
    assert rec["membership_eligible"] is False
    assert "missing on source" in rec["membership_reason"]


def test_membership_threshold_is_honest():
    day = "2026-08-10"
    data = _kline_zip([(1000, 100.0, 2.0)])  # 2.0 < threshold 5.0
    uni = pnl.load_universe(_universe(Path("."), [day]))
    rec = pnl.membership_record(uni, "binance", "BTCUSDT", day, (data, "u", 1, 2.0))
    assert rec["membership_eligible"] is False
    assert "<" in rec["membership_reason"]


def test_verify_sample_rebuilds_byte_identical_and_catches_corruption(tmp_path):
    day = "2026-08-10"
    rel = f"data/futures/um/daily/klines/BTCUSDT/1m/BTCUSDT-1m-{day}.zip"
    base = _write_fixture_root(tmp_path, {rel: _kline_zip([(1000, 100.0, 42.0)])})
    uni = pnl.load_universe(_universe(tmp_path, [day]))
    data, url, rows, volume = pnl.fetch_kline(base, "binance", "BTCUSDT", day)
    pnl.append_manifest(
        tmp_path,
        uni.universe_version,
        [
            pnl.membership_record(
                uni, "binance", "BTCUSDT", day, (data, url, rows, volume)
            )
        ],
    )
    assert pnl.verify_sample(tmp_path, uni.universe_version, base, 1, 42) == []
    # Corrupt the source -> the rebuild must fail loudly.
    (tmp_path / "root" / rel).write_bytes(b"corrupted")
    problems = pnl.verify_sample(tmp_path, uni.universe_version, base, 1, 42)
    assert any("missing or unparseable" in p for p in problems)


def test_check_complete_flags_universe_gaps(tmp_path):
    days = ["2026-08-10", "2026-08-11", "2026-08-12"]
    uni = pnl.load_universe(_universe(tmp_path, days))
    manifest = {
        ("binance", "BTCUSDT", days[0]): {
            "venue": "binance",
            "symbol": "BTCUSDT",
            "date": days[0],
            "status": "ok",
        },
        ("binance", "BTCUSDT", days[1]): {
            "venue": "binance",
            "symbol": "BTCUSDT",
            "date": days[1],
            "status": "missing",
        },
    }
    path = tmp_path / "manifests"
    path.mkdir(parents=True)
    (path / f"{uni.universe_version}.jsonl").write_text(
        "\n".join(json.dumps(v) for v in manifest.values()) + "\n", encoding="utf-8"
    )
    problems = pnl.check_complete(tmp_path, uni.universe_version, uni)
    assert len(problems) == 1
    assert days[2] in problems[0]


def test_kline_url_rejects_non_binance():
    with pytest.raises(AssertionError):
        pnl.kline_url("http://x", "bybit", "BTCUSDT", "2026-08-10")


def test_month_range_spans_inclusive():
    assert pnl.month_range("2026-01", "2026-04") == [
        "2026-01",
        "2026-02",
        "2026-03",
        "2026-04",
    ]
    assert pnl.month_range("2026-11", "2027-02") == [
        "2026-11",
        "2026-12",
        "2027-01",
        "2027-02",
    ]


def test_kline_url_supports_interval_and_monthly_bucket():
    url = pnl.kline_url(
        "http://x", "binance", "BTCUSDT", "2026-08-10", pnl.MONTHLY, "4h"
    )
    assert url == (
        "http://x/data/futures/um/monthly/klines/BTCUSDT/4h/BTCUSDT-4h-2026-08.zip"
    )


def test_download_klines_monthly_round_trip(tmp_path):
    ym = "2026-08"
    rel = f"data/futures/um/monthly/klines/BTCUSDT/4h/BTCUSDT-4h-{ym}.zip"
    base = _write_fixture_root(tmp_path, {rel: _kline_zip([(1000, 100.0, 42.0)])})
    data, url, rows, volume = pnl.download_klines(base, "binance", "BTCUSDT", "4h", ym)
    assert rows == 1 and volume == pytest.approx(42.0)
    assert url == (
        f"{base}/data/futures/um/monthly/klines/BTCUSDT/4h/BTCUSDT-4h-{ym}.zip"
    )


def test_kline_manifest_round_trip(tmp_path):
    uni = pnl.load_universe(_universe(tmp_path, ["2026-08-10"]))
    ym = "2026-08"
    rel = f"data/futures/um/monthly/klines/BTCUSDT/1d/BTCUSDT-1d-{ym}.zip"
    base = _write_fixture_root(tmp_path, {rel: _kline_zip([(1000, 100.0, 42.0)])})
    data, url, rows, volume = pnl.download_klines(base, "binance", "BTCUSDT", "1d", ym)
    rec = pnl.kline_record("BTCUSDT", "1d", ym, (data, url, rows, volume))
    assert rec["status"] == "ok"
    assert rec["zip_sha256"] == pnl._sha256_bytes(data)
    pnl.append_kline_manifest(tmp_path, uni.universe_version, [rec])
    ledger = pnl.load_kline_manifest(tmp_path, uni.universe_version)
    assert ("BTCUSDT", "1d", ym, None) in ledger
    assert ledger[("BTCUSDT", "1d", ym, None)]["rows"] == 1


def test_kline_missing_record_journaled(tmp_path):
    uni = pnl.load_universe(_universe(tmp_path, ["2026-08-10"]))
    rec = pnl.kline_record("BTCUSDT", "4h", "2026-08", None)
    assert rec["status"] == "missing"
    assert rec["zip_sha256"] is None
    pnl.append_kline_manifest(tmp_path, uni.universe_version, [rec])
    ledger = pnl.load_kline_manifest(tmp_path, uni.universe_version)
    assert ledger[("BTCUSDT", "4h", "2026-08", None)]["status"] == "missing"
