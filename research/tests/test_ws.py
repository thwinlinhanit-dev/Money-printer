"""termd analytics-terminal server tests (spec 041): versioned RFC6455
WebSocket push (TER-1/TER-4), strict security headers (TER-10), and REST
pagination (TER-8) over a live local server + synthetic feature store.
Hermetic: everything runs on 127.0.0.1 ephemeral ports."""

import base64
import json
import os
import socket
import threading
import time
import urllib.error
import urllib.request

import polars as pl
import pytest
from http.server import ThreadingHTTPServer

import termd

VENUE = "hyperliquid"
SYMBOL = "TEST"
SYMBOL_ID = 7
DATE = "2026-08-13"


def _write_feature(root, family, rows):
    d = root / "features" / family / "ver=0" / f"venue={VENUE}" / f"symbol={SYMBOL_ID}"
    d.mkdir(parents=True, exist_ok=True)
    pl.DataFrame(
        {
            "symbol_id": [SYMBOL_ID] * len(rows),
            "venue_code": [3] * len(rows),
            "ts_ns": [r[0] for r in rows],
            "value": [r[1] for r in rows],
            "ver": [0] * len(rows),
        }
    ).write_parquet(d / f"{DATE}.parquet")


T0 = 1_700_000_000_000_000_000


@pytest.fixture
def server(tmp_path, monkeypatch):
    """Live ThreadingHTTPServer on an ephemeral port over a tmp store."""
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


class MiniWs:
    """Minimal RFC6455 client: handshake, masked text frames, recv decode."""

    def __init__(self, host, port, path="/v1/ws", origin=None, key=None):
        self.sock = socket.create_connection((host, port), timeout=5)
        key = key or base64.b64encode(os.urandom(16)).decode()
        req = (
            f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
        )
        if origin:
            req += f"Origin: {origin}\r\n"
        req += "\r\n"
        self.sock.sendall(req.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += self.sock.recv(4096)
        header, _, rest = buf.partition(b"\r\n\r\n")
        self.status = int(header.split(b"\r\n")[0].split()[1])
        self.headers = {}
        for line in header.split(b"\r\n")[1:]:
            k, _, v = line.decode().partition(":")
            self.headers[k.strip().lower()] = v.strip()
        if self.status == 101:
            assert self.headers["sec-websocket-accept"] == termd.ws_compute_accept(
                key
            ), "server MUST derive accept per RFC6455"
        self.buf = rest

    def send_json(self, obj):
        data = json.dumps(obj).encode()
        mask = os.urandom(4)
        header = bytes([0x81])
        n = len(data)
        if n < 126:
            header += bytes([0x80 | n])
        else:
            header += bytes([0x80 | 126]) + n.to_bytes(2, "big")
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
        self.sock.sendall(header + mask + masked)

    def _fill(self):
        self.buf += self.sock.recv(65536)

    def recv_json(self, timeout_s=3.0):
        end = time.monotonic() + timeout_s
        while time.monotonic() < end:
            frame = termd.ws_decode_frame(self.buf)
            if frame is not None:
                opcode, payload, consumed = frame
                self.buf = self.buf[consumed:]
                if opcode == 1:
                    return json.loads(payload.decode())
                continue  # control frames skipped
            self.sock.settimeout(max(0.05, end - time.monotonic()))
            try:
                self._fill()
            except TimeoutError:
                break
        raise AssertionError("no complete text frame before timeout")

    def close(self):
        try:
            self.sock.sendall(bytes([0x88]))  # bare close frame
            self.sock.close()
        except OSError:
            pass


# ---------------------------------------------------------------- unit ------


def test_ws_handshake_accept_key_matches_rfc6455():
    # The RFC's own example vector — a wrong derivation breaks every browser.
    assert (
        termd.ws_compute_accept("dGhlIHNhbXBsZSBub25jZQ==")
        == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    )


# ------------------------------------------------------------- live ws ------


def test_ws_protocol_version_and_hello(server):
    c = MiniWs(server["host"], server["port"])
    try:
        assert c.status == 101
        assert c.headers.get("x-ws-protocol-version") == termd.WS_PROTOCOL_VERSION
        hello = c.recv_json()
        assert hello["type"] == "hello"
        assert hello["protocol_version"] == termd.WS_PROTOCOL_VERSION
    finally:
        c.close()


def test_ws_bad_upgrade_without_key_is_400(server):
    s = socket.create_connection((server["host"], server["port"]), timeout=5)
    s.sendall(
        f"GET /v1/ws HTTP/1.1\r\nHost: {server['host']}:{server['port']}\r\n"
        "Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n".encode()
    )
    status = s.recv(1024).split(b"\r\n")[0].split()[1]
    assert status == b"400"
    s.close()


def test_ws_origin_validation_rejects_foreign(server):
    c = MiniWs(server["host"], server["port"], origin="https://evil.example")
    assert c.status == 403
    c.close()


def test_ws_subscribe_batch_push_latency_under_500ms(server):
    """ter_1: a row appended AFTER subscribe must land in a batch frame in
    under 500ms end-to-end."""
    c = MiniWs(server["host"], server["port"])
    try:
        assert c.recv_json()["type"] == "hello"
        c.send_json(
            {
                "type": "subscribe",
                "features": ["cvd.hyperliquid"],
                "venue": VENUE,
                "symbol": SYMBOL,
                "date": DATE,
            }
        )
        sub_ack = c.recv_json()
        assert sub_ack["type"] == "subscribed"

        # Emit a new feature row NOW; measure until it arrives on the wire.
        append_at = time.monotonic()
        new_ts = T0 + 60_000_000_000
        _write_feature(server["root"], "cvd.hyperliquid", [(T0, 10.0), (new_ts, -2.5)])
        got = None
        deadline = time.monotonic() + 3.0
        while time.monotonic() < deadline and got is None:
            msg = c.recv_json(timeout_s=max(0.1, deadline - time.monotonic()))
            if msg.get("type") != "batch":
                continue
            for u in msg["updates"]:
                if u["ts_ns"] == new_ts:
                    got = u
        latency = time.monotonic() - append_at
        assert got is not None, "appended row never pushed"
        assert got["feature_id"] == "cvd.hyperliquid"
        assert got["value"] == -2.5
        assert latency < 0.5, f"ter_1 violated: {latency * 1000:.0f}ms > 500ms"
    finally:
        c.close()


def test_ws_history_serves_points_from_store(server):
    c = MiniWs(server["host"], server["port"])
    try:
        c.recv_json()
        c.send_json(
            {
                "type": "history",
                "feature_id": "cvd.hyperliquid",
                "venue": VENUE,
                "symbol": SYMBOL,
                "date": DATE,
            }
        )
        out = c.recv_json()
        assert out["type"] == "history" and out["total"] == 1
        assert out["points"][0]["value"] == 10.0
        # ping/pong roundtrip keeps intermediaries happy
        c.send_json({"type": "ping"})
        assert c.recv_json()["type"] == "pong"
    finally:
        c.close()


# ------------------------------------------------------- REST surface ------


def _get(base, path):
    req = urllib.request.Request(f"http://{base}{path}")
    with urllib.request.urlopen(req, timeout=5) as r:
        return r.status, dict(r.headers), json.loads(r.read())


def test_ter_10_csp_no_unsafe_eval(server):
    base = f"{server['host']}:{server['port']}"
    status, headers, _ = _get(base, "/v1/symbols")
    assert status == 200
    csp = headers.get("Content-Security-Policy", "")
    assert csp, "CSP header mandatory (TER-10)"
    assert "unsafe-eval" not in csp and "unsafe-inline" not in csp
    assert "frame-ancestors 'none'" in csp
    assert headers.get("X-Content-Type-Options") == "nosniff"


def test_ter_8_rest_history_pagination(server):
    """ter_8: 1000 stored points served through offset/limit pages whose
    concatenation equals the full sorted series, total always honest."""
    rows = [(T0 + i * 1_000_000_000, float(i)) for i in range(1000)]
    _write_feature(server["root"], "funding.rate", rows)

    base = f"{server['host']}:{server['port']}"
    status, _, page = _get(
        base,
        f"/v1/funding?venue={VENUE}&symbol={SYMBOL}&date={DATE}&offset=500&limit=250",
    )
    assert status == 200
    assert page["total"] == 1000
    assert len(page["points"]) == 250
    assert page["points"][0]["ts_ns"] == T0 + 500 * 1_000_000_000

    seen = []
    for off in range(0, 1000, 250):
        _, _, p = _get(
            base,
            f"/v1/funding?venue={VENUE}&symbol={SYMBOL}&date={DATE}&limit=250&offset={off}",
        )
        seen.extend(p["points"])
    assert [pt["ts_ns"] for pt in seen] == sorted(r[0] for r in rows)
    assert [pt["value"] for pt in seen] == [float(i) for i in range(1000)]

    # Beyond-the-end window is explicit, never silently empty (UI-5 spirit).
    _, _, tail = _get(
        base,
        f"/v1/funding?venue={VENUE}&symbol={SYMBOL}&date={DATE}&offset=2000&limit=10",
    )
    assert tail["points"] == [] and tail["total"] == 1000 and tail["note"]
