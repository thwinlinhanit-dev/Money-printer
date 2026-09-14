"""Viewer live-push E2E (spec 041 TER-4 as consumed by terminal/app.js).

Brings up a real termd ThreadingHTTPServer over a tmp feature store, connects
a minimal RFC6455 client (the same client the browser WebSocket speaks to),
subscribes exactly like the viewer does, appends a newer cvd row to the
underlying parquet, and asserts the coalesced batch push arrives with the new
point. Also asserts a stale-ts append is NOT pushed (the client merges appends
only) and that / (the viewer page) serves the live badge + style.css link.
"""

import json
import threading
import time
from http.server import ThreadingHTTPServer

import polars as pl
import pytest

import termd
from tests.test_ws import DATE, SYMBOL, SYMBOL_ID, T0, VENUE, MiniWs, _write_feature


@pytest.fixture
def server(tmp_path, monkeypatch):
    root = tmp_path / "data"
    snap = root / "features" / "symbols"
    snap.mkdir(parents=True)
    (snap / "snap.json").write_text(
        json.dumps([{"id": SYMBOL_ID, "venue": VENUE, "venue_symbol": SYMBOL}])
    )
    _write_feature(root, "cvd.hyperliquid", [(T0, 10.0)])
    monkeypatch.setattr(termd, "DATA_ROOT", root)
    monkeypatch.setattr(termd, "MP_QUERY", tmp_path / "mp-query.exe")
    srv = ThreadingHTTPServer(("127.0.0.1", 0), termd.TermdHandler)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield {"host": "127.0.0.1", "port": srv.server_address[1], "root": root}
    srv.shutdown()
    srv.server_close()


def _get(base, path):
    import urllib.request

    with urllib.request.urlopen(f"http://{base}{path}", timeout=5) as r:
        return r.status, dict(r.headers), r.read()


def test_viewer_live_push_end_to_end(server):
    base = f"{server['host']}:{server['port']}"

    # The page the browser loads carries the live badge and the external sheet.
    status, _, body = _get(base, "/")
    html = body.decode()
    assert status == 200
    assert 'id="liveBadge"' in html, "viewer must expose the live-push badge"
    assert 'href="style.css"' in html, "CSP-safe stylesheet link required"
    assert "<style" not in html, "inline <style> is blocked by TER-10 CSP"
    status, headers, _ = _get(base, "/style.css")
    assert status == 200 and headers["Content-Type"].startswith("text/css")
    assert "frame-ancestors 'none'" in headers["Content-Security-Policy"]

    c = MiniWs(base.split(":")[0], server["port"])
    try:
        assert c.status == 101
        hello = c.recv_json()
        assert hello["type"] == "hello"
        assert hello["protocol_version"] == termd.WS_PROTOCOL_VERSION

        # The exact subscribe the viewer sends on load (app.js liveSubscribe).
        c.send_json(
            {
                "type": "subscribe",
                "features": ["cvd.hyperliquid"],
                "venue": VENUE,
                "symbol": SYMBOL,
                "date": DATE,
            }
        )
        ack = c.recv_json()
        assert ack["type"] == "subscribed"
        assert any("cvd.hyperliquid" in f for f in ack["features"])

        # New data lands in the feature store (what the collector/materializer
        # does in production); the server's next poll must push it.
        newer = T0 + 5_000_000_000
        _write_feature(
            server["root"],
            "cvd.hyperliquid",
            [(T0, 10.0), (newer, 11.5)],
        )

        batch = c.recv_json(timeout_s=5.0)
        assert batch["type"] == "batch"
        updates = [
            u
            for u in batch["updates"]
            if u["feature_id"] == "cvd.hyperliquid" and u["ts_ns"] == newer
        ]
        assert updates, f"expected the appended point in a batch push: {batch}"
        assert updates[0]["value"] == 11.5

        # A same-ts re-append must NOT be pushed (client merges appends only;
        # server-side last_ts dedup makes this the wire contract).
        _write_feature(
            server["root"],
            "cvd.hyperliquid",
            [(T0, 10.0), (newer, 11.5), (T0 - 1, 0.0)],
        )
        got_repeat = False
        try:
            deadline = time.monotonic() + 1.5
            while time.monotonic() < deadline:
                c.sock.settimeout(0.3)
                msg = c.recv_json(timeout_s=0.5)
                if any(u.get("ts_ns") == T0 - 1 for u in msg.get("updates", [])):
                    got_repeat = True
        except (AssertionError, TimeoutError, OSError):
            pass
        assert not got_repeat, "older-ts rows must never be pushed"
    finally:
        c.close()
