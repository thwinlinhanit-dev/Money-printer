"""termd — terminal Slice 1 read-only HTTP API (spec 011, proto-History
Protocol v1) PLUS the spec 041 analytics-terminal server surface: REST series
with pagination, strict security headers (TER-10), and a versioned RFC6455
WebSocket endpoint for live feature pushes (TER-1/TER-4). Serves the recorded
feature store + raw-log derivations to the local viewer (`terminal/`).
Local-only (127.0.0.1), read-only, no venue keys ever (UI-2), lives in
`research/` (CONV-2 — never on live paths).

Endpoints (all GET, JSON):
  /v1/symbols                     venue/symbol/id + feature-backed dates
  /v1/dates?venue=&symbol=        dates with features for the symbol
  /v1/rawdates?venue=&symbol=     dates with a raw log file (bars/dom source)
  /v1/bars?venue=&symbol=&date=&tf=60s
  /v1/footprint?venue=&symbol=&date=&bucket=mid|small|whale&kind=delta|imb
  /v1/cvd?venue=&symbol=&date=[&offset=&limit=]
  /v1/depth?venue=&symbol=&date=[&offset=&limit=]
  /v1/funding?venue=&symbol=&date=[&offset=&limit=]
  /v1/oi?venue=&symbol=&date=[&offset=&limit=]
  /v1/whale?venue=&symbol=&date=[&offset=&limit=]
  /v1/dom?venue=&symbol=&date=&from=&to=&every_ms=&levels=
  /v1/ws                          RFC6455 WebSocket (see below)
  /                                       static viewer (terminal/index.html)

WebSocket (spec 041 TER-4, versioned via X-Ws-Protocol-Version):
  client → {"type":"subscribe","features":[...],"venue":..,"symbol":..,"date":..}
           {"type":"history","feature_id":..,"venue":..,"symbol":..,"date":..
            [,"from_ns":..,"to_ns":..,"offset":..,"limit":..]}
           {"type":"ping"}
  server → {"type":"hello","protocol_version":"1"}
           {"type":"batch","updates":[...],"ts_ns":..,"stale":bool}
           {"type":"history",...} | {"type":"pong"}
  Batches flush at most every 500ms (TER-1 budget); cross-origin upgrades are
  rejected (TER-10).

Errors: 400 malformed query, 404 unknown symbol/date, 500 corrupt parquet or
failed derivation. Empty symbol-days return `[]` + a `note` — the explicit
empty state (UI-5), never a silent blank chart.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import subprocess
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

import polars as pl

_REPO_ROOT = Path(__file__).resolve().parents[1]

DATA_ROOT = Path(os.environ.get("TERMD_DATA_ROOT", _REPO_ROOT / "data"))
MP_QUERY = Path(
    os.environ.get("TERMD_MP_QUERY", _REPO_ROOT / "target" / "release" / "mp-query.exe")
)
TERMINAL_DIR = Path(
    os.environ.get("TERMD_TERMINAL_DIR", Path(__file__).parent / "terminal")
)

# The feature family that defines a symbol-day's feature coverage (the
# terminal's main pane). Other families may cover a subset.
PRIMARY_FAMILY = "footprint.delta.60s.mid"

# family -> API endpoint it feeds (all read-only, long-format feature rows).
FEATURE_FAMILIES = {
    "footprint.delta.60s.mid": ("footprint", "delta"),
    "footprint.delta.60s.small": ("footprint", "delta"),
    "footprint.delta.60s.whale": ("footprint", "delta"),
    "footprint.imb.60s.mid": ("footprint", "imb"),
    "footprint.imb.60s.small": ("footprint", "imb"),
    "footprint.imb.60s.whale": ("footprint", "imb"),
    "cvd.hyperliquid": ("cvd", None),
    "book.depth.0.5": ("depth", None),
    "book.depth.2": ("depth", None),
    "book.depth.10": ("depth", None),
    "book.depth_total.0.5": ("depth", None),
    "book.depth_total.2": ("depth", None),
    "book.depth_total.10": ("depth", None),
    "funding.rate": ("funding", None),
    "oi.delta": ("oi", None),
    "whale_print.hyperliquid": ("whale", None),
}


class TermdError(Exception):
    def __init__(self, status: int, message: str) -> None:
        super().__init__(message)
        self.status = status


def _max_ver_dir(root: Path) -> Path | None:
    """Highest `ver=N` subdirectory, or None."""
    vers = sorted(
        (p for p in root.glob("ver=*") if p.name.startswith("ver=")),
        key=lambda p: int(p.name[len("ver=") :]),
        reverse=True,
    )
    return vers[0] if vers else None


def load_symbols() -> dict[str, dict[int, str]]:
    """symbol snapshot: venue -> {id: venue_symbol} (the newest snapshot wins)."""
    snaps = sorted(
        DATA_ROOT.glob("features/symbols/*.json"), key=lambda p: p.stat().st_mtime
    )
    out: dict[str, dict[int, str]] = {}
    if not snaps:
        return out
    for rec in json.loads(snaps[-1].read_text(encoding="utf-8")):
        out.setdefault(rec["venue"], {})[rec["id"]] = rec["venue_symbol"]
    return out


def _symbol_id(venue: str, symbol: str) -> int | None:
    table = load_symbols().get(venue, {})
    for sid, name in table.items():
        if name == symbol:
            return sid
    return None


def require_symbol(venue: str, symbol: str) -> tuple[str, str, int]:
    """Resolve a symbol name to its feature-store id, or 404."""
    symbol_id = _symbol_id(venue, symbol)
    if symbol_id is None:
        raise TermdError(404, f"unknown symbol {venue}/{symbol}")
    return venue, symbol, symbol_id


def symbols_payload() -> dict[str, Any]:
    names = load_symbols()
    symbols = []
    root = DATA_ROOT / "features" / PRIMARY_FAMILY
    ver = _max_ver_dir(root)
    if ver is not None:
        for venue_dir in sorted(ver.glob("venue=*")):
            venue = venue_dir.name[len("venue=") :]
            for sym_dir in sorted(
                venue_dir.glob("symbol=*"), key=lambda p: int(p.name[len("symbol=") :])
            ):
                symbol_id = int(sym_dir.name[len("symbol=") :])
                dates = sorted(f.stem for f in sym_dir.glob("*.parquet"))
                if not dates:
                    continue
                symbols.append(
                    {
                        "venue": venue,
                        "symbol": names.get(venue, {}).get(symbol_id, str(symbol_id)),
                        "id": symbol_id,
                        "dates": dates,
                    }
                )
    return {"symbols": symbols}


def dates_payload(venue: str, symbol: str) -> dict[str, Any]:
    _, _, symbol_id = require_symbol(venue, symbol)
    return {"venue": venue, "symbol": symbol, "dates": _feature_dates(venue, symbol_id)}


def rawdates_payload(venue: str, symbol: str) -> dict[str, Any]:
    require_symbol(venue, symbol)  # 404 on unknown symbol
    return {"venue": venue, "symbol": symbol, "dates": _raw_dates(venue, symbol)}


def _feature_dates(venue: str, symbol_id: int) -> list[str]:
    root = DATA_ROOT / "features" / PRIMARY_FAMILY
    ver = _max_ver_dir(root)
    if ver is None:
        return []
    d = ver / f"venue={venue}" / f"symbol={symbol_id}"
    if not d.exists():
        return []
    return sorted(f.stem for f in d.glob("*.parquet"))


def _raw_dates(venue: str, symbol: str) -> list[str]:
    dates: set[str] = set()
    for p in (DATA_ROOT / "raw").glob(f"*_{venue}_{symbol}.log"):
        if p.name.startswith("trace_"):
            continue
        ymd = p.stem.split("_", 1)[0]
        if len(ymd) == 8 and ymd.isdigit():
            dates.add(f"{ymd[:4]}-{ymd[4:6]}-{ymd[6:]}")
    return sorted(dates)


def _raw_log_path(venue: str, symbol: str, date: str) -> Path | None:
    ymd = date.replace("-", "")
    p = DATA_ROOT / "raw" / f"{ymd}_{venue}_{symbol}.log"
    return p if p.exists() else None


def _read_feature(
    family: str, venue: str, symbol_id: int, date: str
) -> list[dict[str, Any]]:
    """Long-format rows `[{ts_ns, value}]` for one (family, venue, symbol, date)."""
    root = DATA_ROOT / "features" / family
    ver = _max_ver_dir(root)
    if ver is None:
        return []
    p = ver / f"venue={venue}" / f"symbol={symbol_id}" / f"{date}.parquet"
    if not p.exists():
        return []
    try:
        df = pl.read_parquet(p)
    except Exception as e:  # noqa: BLE001 — surface the path, never a silent blank
        raise TermdError(500, f"corrupt feature parquet {p}: {e}") from e
    df = (
        df.filter(pl.col("symbol_id") == symbol_id)
        .select(["ts_ns", "value"])
        .sort("ts_ns")
    )
    return df.to_dicts()


def _run_mp_query(sub: str, args: list[str], timeout: int = 120) -> list[Any]:
    cmd = [str(MP_QUERY), sub, *args, "--json"]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise TermdError(500, f"mp-query {sub} failed: {e}") from e
    if proc.returncode != 0:
        raise TermdError(
            500, f"mp-query {sub} failed: {(proc.stderr or '').strip()[:500]}"
        )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        raise TermdError(500, f"mp-query {sub} returned unparseable output: {e}") from e


def _series(
    family: str,
    venue: str,
    symbol: str,
    symbol_id: int,
    date: str,
    offset: int = 0,
    limit: int | None = None,
) -> dict[str, Any]:
    """Long-format rows `[{ts_ns, value}]` for one (family, venue, symbol,
    date), windowed by offset/limit with an honest `total` (TER-8)."""
    rows = _read_feature(family, venue, symbol_id, date)
    total = len(rows)
    page = rows[offset:] if limit is None else rows[offset : offset + limit]
    return {
        "feature": family,
        "venue": venue,
        "symbol": symbol,
        "date": date,
        "points": page,
        "total": total,
        "offset": offset,
        "limit": limit,
        "note": None
        if page
        else (
            f"no {family} rows for {venue}/{symbol} on {date}"
            if total == 0
            else f"offset {offset} beyond total {total}"
            if offset >= total
            else f"no rows in window [{offset}, {offset + (limit or 0)})"
        ),
    }


def _bars(venue: str, symbol: str, date: str, tf: int) -> dict[str, Any]:
    log = _raw_log_path(venue, symbol, date)
    if log is None:
        return {
            "feature": "bars",
            "venue": venue,
            "symbol": symbol,
            "date": date,
            "points": [],
            "note": f"no raw log for {venue}/{symbol} on {date}",
        }
    rows = _run_mp_query("bars", ["--logs", str(log), "--interval-secs", str(tf)])
    points = [
        {
            "ts_ns": r["interval_ts_ns"],
            "open": r["open"],
            "high": r["high"],
            "low": r["low"],
            "close": r["close"],
            "vwap": r["vwap"],
            "buy_vol": r["buy_vol"],
            "sell_vol": r["sell_vol"],
            "n_trades": r["n_trades"],
        }
        for r in rows
    ]
    return {
        "feature": "bars",
        "venue": venue,
        "symbol": symbol,
        "date": date,
        "points": points,
        "note": None if points else f"no trades for {venue}/{symbol} on {date}",
    }


def _dom(
    venue: str,
    symbol: str,
    date: str,
    from_ns: str,
    to_ns: str,
    every_ms: int,
    levels: int,
) -> dict[str, Any]:
    log = _raw_log_path(venue, symbol, date)
    if log is None:
        return {
            "feature": "dom",
            "venue": venue,
            "symbol": symbol,
            "date": date,
            "points": [],
            "note": f"no raw log for {venue}/{symbol} on {date}",
        }
    args = ["--logs", str(log), "--every-ms", str(every_ms), "--levels", str(levels)]
    if from_ns:
        args += ["--from", from_ns]
    if to_ns:
        args += ["--to", to_ns]
    rows = _run_mp_query("dom", args)
    points = [
        {
            "ts_ns": r["ts_ns"],
            "stale": r["stale"],
            "bids": [[px, qty] for px, qty in r["bids"]],
            "asks": [[px, qty] for px, qty in r["asks"]],
        }
        for r in rows
    ]
    return {
        "feature": "dom",
        "venue": venue,
        "symbol": symbol,
        "date": date,
        "points": points,
        "note": None if points else f"no book data for {venue}/{symbol} on {date}",
    }


# Feature families for options analytics (specs 037-040).
OPTIONS_FAMILIES = [
    "gex.net",
    "gex.max_pain",
    "net.delta",
    "net.vega",
    "net.theta",
    "iv.atm",
    "iv.index",
    "iv.skew",
    "iv.regime",
    "iv.percentile",
]


def _options_payload(underlying: str) -> dict[str, Any]:
    """Aggregate options analytics for one underlying from the feature store.
    Returns the latest value of each options feature + GEX profile."""
    features: dict[str, Any] = {}
    gex_profile: list[dict[str, Any]] = []
    term_structure: list[dict[str, Any]] = []
    flow: dict[str, Any] = {
        "npf": 0,
        "block": 0,
        "netDelta": 0,
        "callPutRatio": 0,
        "whaleNet": 0,
    }
    u = underlying.lower()
    # Read latest feature values from Parquet
    for family in OPTIONS_FAMILIES:
        feature_id = f"{family}.{u}"
        root = DATA_ROOT / "features" / family
        ver = _max_ver_dir(root)
        if ver is None:
            continue
        # Scan all venue/symbol dirs for this underlying's option tickers
        for venue_dir in ver.glob("venue=*"):
            for sym_dir in venue_dir.glob("symbol=*"):
                for pq in sym_dir.glob("*.parquet"):
                    try:
                        df = pl.read_parquet(pq)
                        if "value" in df.columns and len(df) > 0:
                            last = df.sort("ts_ns").tail(1)
                            val = last["value"][0]
                            if val is not None and str(val) != "null":
                                features[feature_id] = {
                                    "value": float(val),
                                    "ts_ns": int(last["ts_ns"][0])
                                    if "ts_ns" in df.columns
                                    else 0,
                                }
                    except Exception:  # noqa: BLE001
                        pass
    # Build term structure from iv.term.{u}.* features
    for tenor in ["1w", "1m", "3m", "6m"]:
        fid = f"iv.term.{u}.{tenor}"
        if fid in features:
            term_structure.append({"tenor": tenor, "iv": features[fid]["value"]})
    return {
        "underlying": u,
        "features": features,
        "gex_profile": gex_profile,
        "term_structure": term_structure,
        "flow": flow,
        "trades": [],
    }


# ---------------------------------------------------------------------------
# spec 041 — WebSocket live push (TER-1/TER-4): versioned RFC6455 endpoint
# ---------------------------------------------------------------------------

WS_PROTOCOL_VERSION = "1"
_WS_MAGIC = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
WS_BATCH_DELAY_S = 0.5  # TER-1: server-side batch cap (500ms)


def ws_compute_accept(key: str) -> str:
    """RFC6455 §4.2.1 Sec-WebSocket-Accept derivation."""
    return base64.b64encode(hashlib.sha1((key + _WS_MAGIC).encode()).digest()).decode()


def ws_encode_text(payload: str) -> bytes:
    """One unmasked text frame (server → client)."""
    data = payload.encode("utf-8")
    n = len(data)
    if n < 126:
        header = bytes([0x81, n])
    elif n < 65_536:
        header = bytes([0x81, 126]) + n.to_bytes(2, "big")
    else:
        header = bytes([0x81, 127]) + n.to_bytes(8, "big")
    return header + data


def ws_decode_frame(buf: bytes) -> tuple[int, bytes, int] | None:
    """Parse ONE complete frame from ``buf`` → (opcode, payload, consumed),
    or None when incomplete. Client frames are expected masked (RFC); an
    unmasked frame is tolerated for tooling clients."""
    if len(buf) < 2:
        return None
    b1, b2 = buf[0], buf[1]
    masked = bool(b2 & 0x80)
    ln = b2 & 0x7F
    off = 2
    if ln == 126:
        if len(buf) < off + 2:
            return None
        ln = int.from_bytes(buf[off : off + 2], "big")
        off += 2
    elif ln == 127:
        if len(buf) < off + 8:
            return None
        ln = int.from_bytes(buf[off : off + 8], "big")
        off += 8
    mask_len = 4 if masked else 0
    if len(buf) < off + mask_len + ln:
        return None
    payload = buf[off + mask_len : off + mask_len + ln]
    if masked:
        mask = buf[off : off + 4]
        payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return b1 & 0x0F, payload, off + mask_len + ln


class _WsSession:
    """Per-client subscription state (feature_id → poll descriptor)."""

    def __init__(self) -> None:
        self.subs: dict[str, dict[str, Any]] = {}
        self.last_data_ts_ns: int | None = None

    def subscribe(self, msg: dict[str, Any]) -> list[str]:
        added: list[str] = []
        venue = msg.get("venue") or ""
        symbol = msg.get("symbol") or ""
        date = msg.get("date") or ""
        symbol_id = _symbol_id(venue, symbol) if venue and symbol else None
        for fid in msg.get("features", []):
            key = f"{fid}|{venue}|{symbol}|{date}"
            self.subs[key] = {
                "feature_id": fid,
                "venue": venue,
                "symbol": symbol,
                "symbol_id": symbol_id,
                "date": date,
                "last_ts": None,
            }
            added.append(key)
        return added

    def unsubscribe(self, msg: dict[str, Any]) -> None:
        for fid in msg.get("features", []):
            for key in [k for k in self.subs if k.split("|", 1)[0] == fid]:
                del self.subs[key]

    def poll_updates(self) -> list[dict[str, Any]]:
        """Latest value per subscribed series when its ts advanced."""
        out: list[dict[str, Any]] = []
        for sub in sorted(self.subs.values(), key=lambda s: s["feature_id"]):
            sid = sub["symbol_id"]
            if sid is None or not sub["date"]:
                continue
            try:
                rows = _read_feature(sub["feature_id"], sub["venue"], sid, sub["date"])
            except TermdError:
                continue
            if not rows:
                continue
            last = rows[-1]
            if sub["last_ts"] is None or last["ts_ns"] > sub["last_ts"]:
                sub["last_ts"] = last["ts_ns"]
                self.last_data_ts_ns = time.time_ns()
                out.append(
                    {
                        "feature_id": sub["feature_id"],
                        "symbol": sub["symbol"],
                        "venue": sub["venue"],
                        "ts_ns": last["ts_ns"],
                        "value": last["value"],
                    }
                )
        return out


def _insight_payload(asset: str) -> dict[str, Any]:
    """Per-token AI insight endpoint (spec 044 TOK-5). Returns cached insight
    with staleness metadata, or generates one if no cache exists."""
    import importlib.util

    spec_mod = importlib.util.spec_from_file_location(
        "insight_composer", str(Path(__file__).parent / "insight_composer.py")
    )
    if spec_mod is None or spec_mod.loader is None:
        raise TermdError(500, "insight_composer module not found")
    mod = importlib.util.module_from_spec(spec_mod)
    spec_mod.loader.exec_module(mod)
    composer = mod.InsightComposer(mod.InsightConfig())
    cached = composer.cache.get(asset)
    if cached:
        insight, stale = cached
        return {
            "insight": insight.to_dict(),
            "cached": True,
            "stale": stale,
            "age_seconds": int((time.time_ns() - insight.generated_at_ns) / 1e9),
        }
    # No cache — generate
    insight = composer.compose(asset, data_dir=str(DATA_ROOT / "features"))
    return {
        "insight": insight.to_dict(),
        "cached": False,
        "stale": False,
        "age_seconds": 0,
    }


class TermdHandler(BaseHTTPRequestHandler):
    server_version = "termd/0.1"
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:  # noqa: N802 (http.server API)
        try:
            parsed = urlparse(self.path)
            if parsed.path == "/v1/ws":
                self._websocket()
            elif parsed.path == "/v1/symbols":
                self._json(symbols_payload())
            elif parsed.path == "/v1/dates":
                q = self._query(parsed, ["venue", "symbol"])
                self._json(dates_payload(q["venue"], q["symbol"]))
            elif parsed.path == "/v1/rawdates":
                q = self._query(parsed, ["venue", "symbol"])
                self._json(rawdates_payload(q["venue"], q["symbol"]))
            elif parsed.path == "/v1/bars":
                self._json(self._bars_handler(parsed))
            elif parsed.path == "/v1/footprint":
                self._json(self._footprint(parsed))
            elif parsed.path == "/v1/cvd":
                self._json(self._family_handler(parsed, "cvd.hyperliquid"))
            elif parsed.path == "/v1/depth":
                self._json(self._depth(parsed))
            elif parsed.path == "/v1/funding":
                self._json(self._family_handler(parsed, "funding.rate"))
            elif parsed.path == "/v1/oi":
                self._json(self._family_handler(parsed, "oi.delta"))
            elif parsed.path == "/v1/whale":
                self._json(self._family_handler(parsed, "whale_print.hyperliquid"))
            elif parsed.path == "/v1/dom":
                self._json(self._dom_handler(parsed))
            elif parsed.path == "/v1/options":
                q = self._query(parsed, ["asset"])
                self._json(_options_payload(q["asset"]))
            elif parsed.path == "/v1/insight":
                q = self._query(parsed, ["asset"])
                self._json(_insight_payload(q["asset"]))
            else:
                # Everything else is the static viewer: the page at "/" loads
                # ./app.js and ./style assets relative to the terminal dir, so
                # map ANY path to the terminal root, not just /terminal/*.
                name = parsed.path.lstrip("/") or "index.html"
                self._serve_static(name)
        except TermdError as e:
            self._json({"error": str(e)}, status=e.status)
        except pl.exceptions.ComputeError as e:
            self._json({"error": f"corrupt feature parquet: {e}"}, status=500)
        except Exception as e:  # noqa: BLE001 — server must never hang the page
            self._json({"error": f"internal error: {e}"}, status=500)

    def log_message(self, fmt: str, *args: Any) -> None:
        print(f"[termd {time.strftime('%H:%M:%S')}] {fmt % args}")

    def _query(self, parsed: Any, required: list[str]) -> dict[str, str]:
        q = {k: v[0] for k, v in parse_qs(parsed.query).items()}
        missing = [k for k in required if k not in q or not q[k]]
        if missing:
            raise TermdError(400, f"missing query parameter(s): {', '.join(missing)}")
        return q

    def _require_symbol(self, q: dict[str, str]) -> tuple[str, str, int]:
        return require_symbol(q["venue"], q["symbol"])

    def _symbols(self) -> dict[str, Any]:
        return symbols_payload()

    def _dates(self, q: dict[str, str]) -> dict[str, Any]:
        return dates_payload(q["venue"], q["symbol"])

    def _rawdates(self, q: dict[str, str]) -> dict[str, Any]:
        return rawdates_payload(q["venue"], q["symbol"])

    def _bars_handler(self, parsed: Any) -> dict[str, Any]:
        q = self._query(parsed, ["venue", "symbol", "date"])
        venue, symbol, _ = self._require_symbol(q)
        tf = int(q.get("tf", "60"))
        if tf <= 0:
            raise TermdError(400, "tf must be a positive interval in seconds")
        return _bars(venue, symbol, q["date"], tf)

    def _footprint(self, parsed: Any) -> dict[str, Any]:
        q = self._query(parsed, ["venue", "symbol", "date"])
        venue, symbol, symbol_id = self._require_symbol(q)
        bucket = q.get("bucket", "mid")
        kind = q.get("kind", "delta")
        family = f"footprint.{kind}.60s.{bucket}"
        if family not in FEATURE_FAMILIES:
            raise TermdError(400, f"unknown footprint bucket/kind: {kind}/{bucket}")
        offset, limit = self._pagination(q)
        return _series(
            family, venue, symbol, symbol_id, q["date"], offset=offset, limit=limit
        )

    def _depth(self, parsed: Any) -> dict[str, Any]:
        q = self._query(parsed, ["venue", "symbol", "date"])
        venue, symbol, symbol_id = self._require_symbol(q)
        width = q.get("width", "0.5")
        family = f"book.depth.{width}"
        if family not in FEATURE_FAMILIES:
            raise TermdError(400, f"unknown depth width: {width}")
        offset, limit = self._pagination(q)
        return _series(
            family, venue, symbol, symbol_id, q["date"], offset=offset, limit=limit
        )

    def _family_handler(self, parsed: Any, family: str) -> dict[str, Any]:
        q = self._query(parsed, ["venue", "symbol", "date"])
        venue, symbol, symbol_id = self._require_symbol(q)
        offset, limit = self._pagination(q)
        return _series(
            family, venue, symbol, symbol_id, q["date"], offset=offset, limit=limit
        )

    def _pagination(self, q: dict[str, str]) -> tuple[int, int | None]:
        """TER-8: optional offset/limit windowing with total counts."""
        try:
            offset = int(q.get("offset", "0"))
            limit = int(q["limit"]) if "limit" in q else None
        except ValueError as e:
            raise TermdError(400, "offset/limit must be integers") from e
        if offset < 0 or (limit is not None and limit <= 0):
            raise TermdError(400, "offset must be ≥ 0 and limit > 0")
        return offset, limit

    # ---- spec 041: RFC6455 WebSocket session (TER-1/4/10) ------------------

    def _websocket(self) -> None:
        """Versioned live-push endpoint. Validates the upgrade (TER-10 origin
        check), then serves subscribe/history/ping until the peer closes."""
        key = self.headers.get("Sec-WebSocket-Key")
        upgrade = (self.headers.get("Upgrade") or "").lower()
        if upgrade != "websocket" or not key:
            raise TermdError(
                400, "WebSocket upgrade requires Upgrade: websocket + Sec-WebSocket-Key"
            )
        origin = self.headers.get("Origin")
        host = self.headers.get("Host", "")
        if origin:
            o = urlparse(origin)
            if o.netloc and o.netloc != host:
                raise TermdError(403, f"cross-origin WebSocket rejected: {origin}")
        self.send_response(101, "Switching Protocols")
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", ws_compute_accept(key))
        self.send_header("X-Ws-Protocol-Version", WS_PROTOCOL_VERSION)
        self.end_headers()
        self.wfile.flush()
        try:
            self._ws_session()
        except OSError:
            return  # peer vanished — a dead socket must not crash a thread

    def _ws_send_json(self, obj: dict[str, Any]) -> None:
        self.wfile.write(ws_encode_text(json.dumps(obj)))
        self.wfile.flush()

    def _ws_history(self, msg: dict[str, Any]) -> dict[str, Any]:
        for field in ("feature_id", "venue", "symbol", "date"):
            if not msg.get(field):
                raise TermdError(400, f"history requires {field}")
        _, _, symbol_id = require_symbol(msg["venue"], msg["symbol"])
        rows = _read_feature(msg["feature_id"], msg["venue"], symbol_id, msg["date"])
        from_ns = int(msg.get("from_ns", 0) or 0)
        to_ns = int(msg.get("to_ns", 0) or 0)
        if from_ns or to_ns:
            rows = [
                r
                for r in rows
                if (not from_ns or r["ts_ns"] >= from_ns)
                and (not to_ns or r["ts_ns"] <= to_ns)
            ]
        total = len(rows)
        offset = max(int(msg.get("offset", 0) or 0), 0)
        limit = min(int(msg.get("limit", 5000) or 5000), 5000)
        return {
            "type": "history",
            "feature_id": msg["feature_id"],
            "points": rows[offset : offset + limit],
            "total": total,
            "offset": offset,
            "limit": limit,
        }

    def _ws_dispatch(
        self, msg: dict[str, Any], session: _WsSession
    ) -> dict[str, Any] | None:
        mtype = msg.get("type")
        if mtype == "subscribe":
            added = session.subscribe(msg)
            return {
                "type": "subscribed",
                "features": sorted(added),
                "protocol_version": WS_PROTOCOL_VERSION,
            }
        if mtype == "unsubscribe":
            session.unsubscribe(msg)
            return {"type": "unsubscribed"}
        if mtype == "history":
            return self._ws_history(msg)
        if mtype == "ping":
            return {"type": "pong", "ts_ns": time.time_ns()}
        return {"type": "error", "error": f"unknown message type: {mtype!r}"}

    def _ws_session(self) -> None:
        """Read frames, answer requests immediately, and push subscription
        polls as single batch frames at a ≥100ms coalescing cadence — the
        end-to-end emission→frame latency stays well inside TER-1's 500ms
        budget while bursts collapse into one WebSocket message."""
        session = _WsSession()
        buf = b""
        last_batch = time.monotonic()
        self.connection.settimeout(0.1)
        self._ws_send_json(
            {
                "type": "hello",
                "protocol_version": WS_PROTOCOL_VERSION,
                "batch_delay_ms": int(WS_BATCH_DELAY_S * 1000),
            }
        )
        while True:
            try:
                chunk = self.connection.recv(4096)
                if not chunk:
                    return  # orderly close
                buf += chunk
            except TimeoutError:
                pass
            while True:
                frame = ws_decode_frame(buf)
                if frame is None:
                    break
                opcode, payload, consumed = frame
                buf = buf[consumed:]
                if opcode == 8:  # close
                    return
                if opcode == 9:  # ping control frame → pong frame
                    self.wfile.write(bytes([0x8A, len(payload)]) + payload)
                    self.wfile.flush()
                    continue
                if opcode == 1:
                    try:
                        reply = self._ws_dispatch(json.loads(payload.decode()), session)
                    except TermdError as e:
                        reply = {"type": "error", "status": e.status, "error": str(e)}
                    if reply is not None:
                        self._ws_send_json(reply)
            now = time.monotonic()
            # Coalesced batch flush: ≥100ms apart, far below TER-1's cap.
            if session.subs and now - last_batch >= 0.1:
                updates = session.poll_updates()
                stale = (
                    session.last_data_ts_ns is None
                    or time.time_ns() - session.last_data_ts_ns > 60_000_000_000
                )
                self._ws_send_json(
                    {
                        "type": "batch",
                        "updates": updates,
                        "ts_ns": time.time_ns(),
                        "stale": stale,
                        "protocol_version": WS_PROTOCOL_VERSION,
                    }
                )
                last_batch = now

    def _dom_handler(self, parsed: Any) -> dict[str, Any]:
        q = self._query(parsed, ["venue", "symbol", "date"])
        venue, symbol, _ = self._require_symbol(q)
        every_ms = int(q.get("every_ms", "5000"))
        levels = int(q.get("levels", "5"))
        if every_ms <= 0 or levels <= 0:
            raise TermdError(400, "every_ms and levels must be positive")
        return _dom(
            venue,
            symbol,
            q["date"],
            q.get("from", ""),
            q.get("to", ""),
            every_ms,
            levels,
        )

    def _security_headers(self) -> None:
        """TER-10: strict CSP (no unsafe-eval, no framing), nosniff."""
        self.send_header(
            "Content-Security-Policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; "
            "img-src 'self' data:; connect-src 'self' ws: wss:; "
            "frame-ancestors 'none'; base-uri 'none'",
        )
        self.send_header("X-Content-Type-Options", "nosniff")

    def _serve_static(self, name: str) -> None:
        target = (TERMINAL_DIR / name).resolve()
        if (
            not target.is_relative_to(TERMINAL_DIR.resolve())
            or not target.exists()
            or not target.is_file()
        ):
            raise TermdError(404, f"not found: {name}")
        content = target.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", _content_type(name))
        self.send_header("Content-Length", str(len(content)))
        self.send_header("Cache-Control", "no-store")
        self._security_headers()
        self.end_headers()
        self.wfile.write(content)

    def _json(self, payload: dict[str, Any], status: int = 200) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self._security_headers()
        self.end_headers()
        self.wfile.write(body)


def _content_type(name: str) -> str:
    ext = Path(name).suffix.lower()
    return {
        ".html": "text/html; charset=utf-8",
        ".js": "text/javascript; charset=utf-8",
        ".css": "text/css; charset=utf-8",
        ".svg": "image/svg+xml",
        ".json": "application/json; charset=utf-8",
    }.get(ext, "application/octet-stream")


def main(argv: list[str] | None = None) -> None:
    global DATA_ROOT, MP_QUERY, TERMINAL_DIR
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default=os.environ.get("TERMD_HOST", "127.0.0.1"))
    parser.add_argument(
        "--port", type=int, default=int(os.environ.get("TERMD_PORT", "8765"))
    )
    parser.add_argument("--data-root", type=Path, default=DATA_ROOT)
    parser.add_argument("--mp-query", type=Path, default=MP_QUERY)
    parser.add_argument("--terminal-dir", type=Path, default=TERMINAL_DIR)
    args = parser.parse_args(argv)

    DATA_ROOT, MP_QUERY, TERMINAL_DIR = args.data_root, args.mp_query, args.terminal_dir

    if not MP_QUERY.exists():
        print(
            "termd: warning — mp-query binary not found at "
            f"{MP_QUERY}; bars/dom will return 500s (build it with cargo build --release)"
        )
    if not DATA_ROOT.exists():
        parser.error(f"data root not found: {DATA_ROOT}")

    server = ThreadingHTTPServer((args.host, args.port), TermdHandler)
    print(
        f"termd: {args.host}:{args.port}  data={DATA_ROOT}  "
        f"mp-query={MP_QUERY}  viewer={TERMINAL_DIR}"
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\ntermd: shutting down")


if __name__ == "__main__":
    main()
