"""Historical research dataset machinery (serious-research-lab roadmap 3.1).

A reproducible historical panel of binance USDT-M perp klines with a
rebuildable manifest: universe definition with inclusion rule and liquidity
threshold, source manifest/hash, downloader version, and membership journaled
per (symbol, date). ``verify`` re-downloads from the manifest and requires the
same checksums - a panel that cannot be rebuilt is not a panel.

The namespace/fidelity contract: everything here is explicitly
``external_archive`` data (roadmap 3.1) - it is NOT evidence for queue
position, maker fills, or sub-minute order-flow alpha. A reader must consume
it through this module's manifest (membership + checksums), never raw files.

Chronological partition rule (train/validation/final test) lives in the
universe file; the final test period remains untouched until hypothesis and
parameters are frozen.

Pure stdlib, deterministic (no wall clock; seeded sampling). Research-only
(CONV-2).
"""

from __future__ import annotations

import csv
import hashlib
import io
import json
import random
import urllib.error
import urllib.request
import zipfile
from dataclasses import dataclass
from datetime import date, timedelta

DEFAULT_BASE_URL = "https://data.binance.vision"
DAILY = "daily"
MONTHLY = "monthly"
INTERVAL = "1m"


class PanelError(Exception):
    pass


@dataclass(frozen=True)
class Universe:
    universe_version: str
    downloader_version: str
    source: str
    source_timezone: str
    inclusion_rule: str
    liquidity_threshold_usd: float
    daily_threshold_usd: float
    delisting_rule: str
    fidelity_label: str
    partition: dict
    notes: str
    symbols: tuple[
        dict, ...
    ]  # {"venue", "symbol", "start": "YYYY-MM-DD", "end": "YYYY-MM-DD"}


def load_universe(path) -> Universe:
    from pathlib import Path

    raw = json.loads(Path(path).read_text(encoding="utf-8"))
    return Universe(
        universe_version=raw["universe_version"],
        downloader_version=raw["downloader_version"],
        source=raw["source"],
        source_timezone=raw["source_timezone"],
        inclusion_rule=raw["inclusion_rule"],
        liquidity_threshold_usd=raw["liquidity_threshold_usd"],
        daily_threshold_usd=raw.get(
            "daily_threshold_usd", raw["liquidity_threshold_usd"] / 365.0
        ),
        delisting_rule=raw["delisting_rule"],
        fidelity_label=raw["fidelity_label"],
        partition=raw["partition"],
        notes=raw.get("notes", ""),
        symbols=tuple(raw["symbols"]),
    )


def date_range(start: str, end: str) -> list[str]:
    out: list[str] = []
    d = date.fromisoformat(start)
    stop = date.fromisoformat(end)
    while d <= stop:
        out.append(d.isoformat())
        d += timedelta(days=1)
    return out


def kline_url(
    base_url: str,
    venue: str,
    symbol: str,
    day: str,
    bucket: str = DAILY,
    interval: str = INTERVAL,
) -> str:
    assert venue == "binance", f"panel source only implements binance, got {venue}"
    if bucket == DAILY:
        return (
            f"{base_url}/data/futures/um/daily/klines/{symbol}/{interval}/"
            f"{symbol}-{interval}-{day}.zip"
        )
    return (
        f"{base_url}/data/futures/um/monthly/klines/{symbol}/{interval}/"
        f"{symbol}-{interval}-{day[:7]}.zip"
    )


def _sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _kline_stats(zip_bytes: bytes) -> tuple[int, float]:
    """(rows, quote_volume_usd) from a binance kline zip. Fails loudly on a
    malformed archive - an unparsed panel file is untrusted."""
    with zipfile.ZipFile(io.BytesIO(zip_bytes)) as zf:
        names = [n for n in zf.namelist() if n.endswith(".csv")]
        if len(names) != 1:
            raise PanelError(f"zip has {len(names)} csv files, expected exactly 1")
        with zf.open(names[0]) as f:
            rows = 0
            volume = 0.0
            for row in csv.reader(io.TextIOWrapper(f)):
                if not row or len(row) < 8:
                    continue
                rows += 1
                if rows > 1:
                    volume += float(row[7])  # quote volume (USDT)
        return rows - 1, volume


def fetch_kline(
    base_url: str,
    venue: str,
    symbol: str,
    day: str,
    bucket: str = DAILY,
    interval: str = INTERVAL,
) -> tuple[bytes, str, int, float]:
    """(zip_bytes, url, rows, quote_volume_usd). Raises PanelError on 404."""
    url = kline_url(base_url, venue, symbol, day, bucket, interval)
    try:
        with urllib.request.urlopen(url, timeout=60) as resp:
            data = resp.read()
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            raise PanelError(f"404: {url}") from exc
        raise
    except urllib.error.URLError as exc:
        # file:// fixtures and plain connection errors surface as URLError;
        # a missing source file is a data-missing record, never a crash.
        raise PanelError(f"unreachable: {url} ({exc.reason})") from exc
    return data, url, *_kline_stats(data)


def _manifest_path(panel_dir, universe_version: str):
    return panel_dir / "manifests" / f"{universe_version}.jsonl"


def load_manifest(panel_dir, universe_version: str) -> dict[tuple[str, str, str], dict]:
    from pathlib import Path

    path = _manifest_path(Path(panel_dir), universe_version)
    out: dict[tuple[str, str, str], dict] = {}
    if path.exists():
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                rec = json.loads(line)
                out[(rec["venue"], rec["symbol"], rec["date"])] = rec
    return out


def append_manifest(panel_dir, universe_version: str, records: list[dict]) -> None:
    from pathlib import Path

    path = _manifest_path(Path(panel_dir), universe_version)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a", encoding="utf-8") as f:
        for rec in records:
            f.write(json.dumps(rec, sort_keys=True) + "\n")


def membership_record(
    universe: Universe,
    venue: str,
    symbol: str,
    day: str,
    stats: tuple[bytes, str, int, float] | None,
) -> dict:
    """A manifest row: source hash + membership per the universe rule."""
    rec = {
        "universe_version": universe.universe_version,
        "downloader_version": universe.downloader_version,
        "venue": venue,
        "symbol": symbol,
        "date": day,
        "fidelity_label": universe.fidelity_label,
    }
    if stats is None:
        rec.update(
            {
                "status": "missing",
                "zip_sha256": None,
                "rows": 0,
                "quote_volume_usd": 0.0,
                "membership_eligible": False,
                "membership_reason": f"no panel data for {day} (missing on source)",
            }
        )
        return rec
    data, url, rows, volume = stats
    eligible = volume >= universe.daily_threshold_usd
    rec.update(
        {
            "status": "ok",
            "url": url,
            "zip_sha256": _sha256_bytes(data),
            "zip_bytes": len(data),
            "rows": rows,
            "quote_volume_usd": volume,
            "membership_eligible": eligible,
            "membership_reason": (
                f"quote volume {volume:,.0f} >= daily threshold {universe.daily_threshold_usd:,.0f}"
                if eligible
                else f"quote volume {volume:,.0f} < daily threshold {universe.daily_threshold_usd:,.0f}"
            ),
        }
    )
    return rec


def verify_sample(
    panel_dir, universe_version: str, base_url: str, sample: int, seed: int
) -> list[str]:
    """Re-download ``sample`` manifest entries (seeded, deterministic) and
    require byte-identical zips - the rebuild proof. Returns problems."""
    from pathlib import Path

    entries = sorted(load_manifest(Path(panel_dir), universe_version).items())
    pick = (
        random.Random(seed).sample(entries, min(sample, len(entries)))
        if entries
        else []
    )
    problems: list[str] = []
    for (venue, symbol, day), rec in pick:
        if rec.get("status") != "ok":
            continue
        try:
            data, _url, rows, _vol = fetch_kline(base_url, venue, symbol, day)
        except (PanelError, zipfile.BadZipFile, KeyError):
            problems.append(
                f"{day} {venue}:{symbol} now missing or unparseable on source"
            )
            continue
        if _sha256_bytes(data) != rec["zip_sha256"]:
            problems.append(
                f"{day} {venue}:{symbol} sha256 MISMATCH "
                f"({rec['zip_sha256'][:12]}... != {_sha256_bytes(data)[:12]}...)"
            )
        if rows != rec["rows"]:
            problems.append(
                f"{day} {venue}:{symbol} row count {rows} != manifest {rec['rows']}"
            )
    return problems


def check_complete(panel_dir, universe_version: str, universe: Universe) -> list[str]:
    """Every (symbol, day) in the universe window has a manifest entry."""
    from pathlib import Path

    manifest = load_manifest(Path(panel_dir), universe_version)
    problems: list[str] = []
    for sym in universe.symbols:
        if sym["venue"] != "binance":
            problems.append(
                f"universe symbol {sym['symbol']}: only binance is supported"
            )
            continue
        for day in date_range(sym["start"], sym["end"]):
            if (sym["venue"], sym["symbol"], day) not in manifest:
                problems.append(
                    f"{day} {sym['venue']}:{sym['symbol']} missing from manifest"
                )
    return problems


def month_range(start: str, end: str) -> list[str]:
    """YYYY-MM for each month in [start, end] (both YYYY-MM, inclusive)."""
    out: list[str] = []
    y0, m0 = (int(x) for x in start.split("-"))
    y1, m1 = (int(x) for x in end.split("-"))
    y, m = y0, m0
    while (y, m) <= (y1, m1):
        out.append(f"{y:04d}-{m:02d}")
        m += 1
        if m == 13:
            y += 1
            m = 1
    return out


def _kline_manifest_path(panel_dir, universe_version: str):
    return panel_dir / "manifests" / f"{universe_version}-klines.jsonl"


def load_kline_manifest(
    panel_dir, universe_version: str
) -> dict[tuple[str, str, str, str | None], dict]:
    """Kline ledger keyed by (symbol, interval, year_month[, day]); ``day`` is
    None for monthly-bucket records. Append-only; the checksum is the identity
    — re-download must rebuild byte-identical."""
    from pathlib import Path

    path = _kline_manifest_path(Path(panel_dir), universe_version)
    out: dict[tuple[str, str, str, str | None], dict] = {}
    if path.exists():
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                rec = json.loads(line)
                out[
                    (rec["symbol"], rec["interval"], rec["year_month"], rec.get("day"))
                ] = rec
    return out


def append_kline_manifest(
    panel_dir, universe_version: str, records: list[dict]
) -> None:
    from pathlib import Path

    path = _kline_manifest_path(Path(panel_dir), universe_version)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a", encoding="utf-8") as f:
        for rec in records:
            f.write(json.dumps(rec, sort_keys=True) + "\n")


def download_klines(
    base_url: str,
    venue: str,
    symbol: str,
    interval: str,
    year_month: str,
) -> tuple[bytes, str, int, float]:
    """Monthly kline zip for (symbol, interval, YYYY-MM) — 4h/daily for the
    swing panel. Same contract as fetch_kline: (zip_bytes, url, rows,
    quote_volume_usd), PanelError on 404/unreachable."""
    return fetch_kline(base_url, venue, symbol, year_month, MONTHLY, interval)


def kline_record(
    symbol: str,
    interval: str,
    year_month: str,
    stats: tuple[bytes, str, int, float] | None,
    day: str | None = None,
) -> dict:
    """One kline ledger row: source hash + rows + quote volume. ``day`` set
    for daily-bucket records (tail month not yet in the monthly bucket)."""
    rec = {
        "venue": "binance",
        "symbol": symbol,
        "interval": interval,
        "year_month": year_month,
        "fidelity_label": "external_archive:binance-um-klines-1m-v1",
        "source_bucket": "daily" if day else "monthly",
    }
    if day:
        rec["day"] = day
    if stats is None:
        rec.update(
            {
                "status": "missing",
                "zip_sha256": None,
                "rows": 0,
                "quote_volume_usd": 0.0,
            }
        )
        return rec
    data, url, rows, volume = stats
    rec.update(
        {
            "status": "ok",
            "url": url,
            "zip_sha256": _sha256_bytes(data),
            "zip_bytes": len(data),
            "rows": rows,
            "quote_volume_usd": volume,
        }
    )
    return rec
