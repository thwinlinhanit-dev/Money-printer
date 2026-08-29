"""termd API tests (terminal Slice 1, spec 011): read-only endpoint handlers
over a synthetic feature store, error paths, and an integration bars/DOM
parity check against a real raw log + the real `mp-query` binary (skipped when
either is absent — the suite stays hermetic)."""

import json

import polars as pl
import pytest

import termd

VENUE = "hyperliquid"
SYMBOL = "TEST"
SYMBOL_ID = 7
DATE = "2026-08-13"


def _write_feature(root, family, rows):
    d = root / "features" / family / "ver=0" / f"venue={VENUE}" / f"symbol={SYMBOL_ID}"
    d.mkdir(parents=True)
    pl.DataFrame(
        {
            "symbol_id": [SYMBOL_ID] * len(rows),
            "venue_code": [3] * len(rows),
            "ts_ns": [r[0] for r in rows],
            "value": [r[1] for r in rows],
            "ver": [0] * len(rows),
        }
    ).write_parquet(d / f"{DATE}.parquet")


def _make_store(root):
    snap = root / "features" / "symbols"
    snap.mkdir(parents=True)
    (snap / "snap.json").write_text(
        json.dumps([{"id": SYMBOL_ID, "venue": VENUE, "venue_symbol": SYMBOL}]),
        encoding="utf-8",
    )
    t0 = 1_700_000_000_000_000_000
    _write_feature(
        root, "footprint.delta.60s.mid", [(t0, 1.5), (t0 + 60_000_000_000, -2.0)]
    )
    _write_feature(root, "cvd.hyperliquid", [(t0, 10.0), (t0 + 60_000_000_000, 8.5)])
    _write_feature(root, "funding.rate", [(t0, 0.0001)])
    _write_feature(root, "oi.delta", [(t0 + 30_000_000_000, 25.0)])
    _write_feature(root, "whale_print.hyperliquid", [(t0 + 1, 505_000.0)])
    return root


@pytest.fixture
def store(tmp_path, monkeypatch):
    root = _make_store(tmp_path)
    monkeypatch.setattr(termd, "DATA_ROOT", root)
    monkeypatch.setattr(termd, "MP_QUERY", tmp_path / "mp-query.exe")
    return root


def test_symbols_lists_feature_backed_symbols(store):
    out = termd.symbols_payload()
    assert len(out["symbols"]) == 1
    s = out["symbols"][0]
    assert s["venue"] == VENUE
    assert s["symbol"] == SYMBOL
    assert s["id"] == SYMBOL_ID
    assert s["dates"] == [DATE]


def test_dates_and_series_envelope(store):
    assert termd.dates_payload(VENUE, SYMBOL)["dates"] == [DATE]
    out = termd._series("footprint.delta.60s.mid", VENUE, SYMBOL, SYMBOL_ID, DATE)
    assert out["feature"] == "footprint.delta.60s.mid"
    assert [p["value"] for p in out["points"]] == [1.5, -2.0]
    assert out["note"] is None
    assert out["points"][0]["ts_ns"] < out["points"][1]["ts_ns"]


def test_whale_funding_oi_endpoints(store):
    w = termd._series("whale_print.hyperliquid", VENUE, SYMBOL, SYMBOL_ID, DATE)
    assert w["points"][0]["value"] == 505_000.0
    f = termd._series("funding.rate", VENUE, SYMBOL, SYMBOL_ID, DATE)
    assert f["points"][0]["value"] == 0.0001
    o = termd._series("oi.delta", VENUE, SYMBOL, SYMBOL_ID, DATE)
    assert o["points"][0]["value"] == 25.0


def test_empty_symbol_day_is_explicit_note_not_blank(store):
    out = termd._series("cvd.hyperliquid", VENUE, SYMBOL, SYMBOL_ID, "2026-08-14")
    assert out["points"] == []
    assert out["note"] is not None  # UI-5: explicit empty state


def test_unknown_symbol_is_404(store):
    with pytest.raises(termd.TermdError) as e:
        termd.require_symbol(VENUE, "NOPE")
    assert e.value.status == 404


def test_corrupt_parquet_is_500_with_path(store):
    d = (
        store
        / "features"
        / "cvd.hyperliquid"
        / "ver=0"
        / f"venue={VENUE}"
        / f"symbol={SYMBOL_ID}"
    )
    (d / f"{DATE}.parquet").write_bytes(b"not a parquet file")
    with pytest.raises(termd.TermdError) as e:
        termd._series("cvd.hyperliquid", VENUE, SYMBOL, SYMBOL_ID, DATE)
    assert e.value.status == 500
    assert "cvd.hyperliquid" in str(e.value)


def test_bars_empty_when_no_raw_log(store):
    out = termd._bars(VENUE, SYMBOL, DATE, 60)
    assert out["points"] == []
    assert out["note"] is not None


def test_bars_and_dom_parity_on_real_log():
    """Integration: derived OHLCV must satisfy OHLC invariants and bucket
    alignment, and the DOM ladder must carry price-sorted sides — against the
    real mp-query binary and a real hyperliquid raw log. Skipped when either
    is absent so the suite stays hermetic."""
    import os
    from pathlib import Path

    repo = Path(__file__).resolve().parents[2]
    exe = repo / "target" / "release" / "mp-query.exe"
    log = repo / "data" / "raw" / "20260816_hyperliquid_BTC.log"
    if not exe.exists() or not log.exists():
        pytest.skip("mp-query binary or real raw log absent")
    if os.environ.get("TERMD_DATA_ROOT"):
        root = Path(os.environ["TERMD_DATA_ROOT"])
    else:
        root = repo / "data"
    termd.DATA_ROOT = root
    termd.MP_QUERY = exe

    bars = termd._bars("hyperliquid", "BTC", "2026-08-16", 60)
    assert bars["points"], "real day must produce bars"
    prev = None
    for b in bars["points"]:
        assert b["high"] >= max(b["open"], b["close"])
        assert b["low"] <= min(b["open"], b["close"])
        assert b["n_trades"] > 0
        assert b["ts_ns"] % 60_000_000_000 == 0  # floor-aligned
        assert (
            prev is None or b["ts_ns"] > prev
        )  # strictly increasing (gaps = no trades)
        prev = b["ts_ns"]

    dom = termd._dom("hyperliquid", "BTC", "2026-08-16", "", "", 60_000, 3)
    assert dom["points"], "real day must produce ladders"
    for s in dom["points"]:
        if s["stale"]:
            assert s["bids"] == [] and s["asks"] == []
        else:
            bid_px = [p[0] for p in s["bids"]]
            ask_px = [p[0] for p in s["asks"]]
            assert bid_px == sorted(bid_px, reverse=True)
            assert ask_px == sorted(ask_px)


# --- Security hardening regression tests (audit 2026-08-25) -----------------


def test_read_feature_rejects_traversal_family(store):
    """VULN-001: a feature_id with `..` must never leave the feature store."""
    with pytest.raises(termd.TermdError) as e:
        termd._read_feature("../../etc/passwd", VENUE, SYMBOL_ID, DATE)
    assert e.value.status == 400
    assert "unsafe feature family" in str(e.value)


def test_read_feature_rejects_traversal_date(store):
    """VULN-001: a date with enough `..` to escape the feature store is blocked
    by the final-path containment guard (defense in depth beyond the family
    root check)."""
    escaping_date = "../../../../../outside"
    with pytest.raises(termd.TermdError) as e:
        termd._read_feature("cvd.hyperliquid", VENUE, SYMBOL_ID, escaping_date)
    assert e.value.status == 400
    assert "unsafe feature path" in str(e.value)


def test_require_valid_date_accepts_iso(store):
    """VULN-002: a well-formed YYYY-MM-DD date passes the gate."""
    termd._require_valid_date(DATE)  # must not raise


@pytest.mark.parametrize(
    "bad",
    [
        "../16",  # traversal
        "2026/08/13",  # wrong separators
        "2026-8-1",  # non-zero-padded
        "2026-0813",  # missing hyphen
        "20260813",  # no hyphens at all
        "2026-08-13.txt",  # trailing junk
        "",  # empty
    ],
)
def test_require_valid_date_rejects_malformed(store, bad):
    """VULN-002: anything that is not exactly YYYY-MM-DD is rejected."""
    with pytest.raises(termd.TermdError) as e:
        termd._require_valid_date(bad)
    assert e.value.status == 400


@pytest.mark.parametrize("bad_host", ["0.0.0.0", "", "192.168.1.10"])
def test_non_loopback_bind_is_refused(store, monkeypatch, capsys, bad_host):
    """HARDING-003: the read-only terminal must fail-closed on a non-loopback
    bind unless the operator explicitly opts in via TERMD_ALLOW_REMOTE=1.
    The empty string is included deliberately: ThreadingHTTPServer(("", port))
    binds INADDR_ANY (audit 2026-08-26)."""
    monkeypatch.delenv("TERMD_ALLOW_REMOTE", raising=False)
    with pytest.raises(SystemExit):
        termd.main(["--host", bad_host, "--port", "0", "--data-root", str(store)])
    assert "non-loopback" in capsys.readouterr().err


def test_loopback_bind_allowed_with_remote_opt_in(store, monkeypatch):
    """HARDING-003: TERMD_ALLOW_REMOTE=1 silences the refusal (host validation
    passes; the server would then try to bind port 0 on 0.0.0.0/ephemeral —
    use a monkeypatched server factory to avoid actually listening)."""
    monkeypatch.setenv("TERMD_ALLOW_REMOTE", "1")
    bound = {}

    def fake_server(addr, handler):
        bound["addr"] = addr

        # Return a fake object whose serve_forever blocks forever; run it in a
        # thread and stop immediately so main() exits.
        class Fake:
            def serve_forever(self):
                pass

            def server_close(self):
                pass

        return Fake()

    monkeypatch.setattr(termd, "ThreadingHTTPServer", fake_server)
    termd.main(["--host", "0.0.0.0", "--port", "0", "--data-root", str(store)])
    assert bound["addr"] == ("0.0.0.0", 0)
