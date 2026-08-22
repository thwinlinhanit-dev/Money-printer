"""termd — terminal Slice 1 read-only HTTP API (spec 011, proto-History
Protocol v1). Serves the recorded feature store + raw-log derivations to the
local research viewer (`terminal/`). Local-only (127.0.0.1), read-only, no
venue keys ever (UI-2), lives in `research/` (CONV-2 — never on live paths).

Endpoints (all GET, JSON):
  /v1/symbols                     venue/symbol/id + feature-backed dates
  /v1/dates?venue=&symbol=        dates with features for the symbol
  /v1/rawdates?venue=&symbol=     dates with a raw log file (bars/dom source)
  /v1/bars?venue=&symbol=&date=&tf=60s
  /v1/footprint?venue=&symbol=&date=&bucket=mid|small|whale&kind=delta|imb
  /v1/cvd?venue=&symbol=&date=
  /v1/depth?venue=&symbol=&date=
  /v1/funding?venue=&symbol=&date=
  /v1/oi?venue=&symbol=&date=
  /v1/whale?venue=&symbol=&date=
  /v1/dom?venue=&symbol=&date=&from=&to=&every_ms=&levels=
  /                                       static viewer (terminal/index.html)

Errors: 400 malformed query, 404 unknown symbol/date, 500 corrupt parquet or
failed derivation. Empty symbol-days return `[]` + a `note` — the explicit
empty state (UI-5), never a silent blank chart.
"""

from __future__ import annotations

import argparse
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
    family: str, venue: str, symbol: str, symbol_id: int, date: str
) -> dict[str, Any]:
    points = _read_feature(family, venue, symbol_id, date)
    return {
        "feature": family,
        "venue": venue,
        "symbol": symbol,
        "date": date,
        "points": points,
        "note": None if points else f"no {family} rows for {venue}/{symbol} on {date}",
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


class TermdHandler(BaseHTTPRequestHandler):
    server_version = "termd/0.1"
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:  # noqa: N802 (http.server API)
        try:
            parsed = urlparse(self.path)
            if parsed.path == "/v1/symbols":
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
        return _series(family, venue, symbol, symbol_id, q["date"])

    def _depth(self, parsed: Any) -> dict[str, Any]:
        q = self._query(parsed, ["venue", "symbol", "date"])
        venue, symbol, symbol_id = self._require_symbol(q)
        width = q.get("width", "0.5")
        family = f"book.depth.{width}"
        if family not in FEATURE_FAMILIES:
            raise TermdError(400, f"unknown depth width: {width}")
        return _series(family, venue, symbol, symbol_id, q["date"])

    def _family_handler(self, parsed: Any, family: str) -> dict[str, Any]:
        q = self._query(parsed, ["venue", "symbol", "date"])
        venue, symbol, symbol_id = self._require_symbol(q)
        return _series(family, venue, symbol, symbol_id, q["date"])

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
        self.end_headers()
        self.wfile.write(content)

    def _json(self, payload: dict[str, Any], status: int = 200) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
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
