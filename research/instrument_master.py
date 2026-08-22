"""Point-in-time instrument master (serious-research-lab roadmap Phase 3.3).

A versioned record of symbol identity, venue, instrument type, contract
multiplier, tick/step size, listing interval, quote and margin asset, funding
schedule, fee tier, and known migrations. A historical or live run resolves
every traded symbol to the exact instrument definition in force at its
decision timestamp; unresolved or changed metadata BLOCKS the run.

The master records an *observed* validity window (``observed_from_ns`` ..)
per venue/symbol, sourced from corpus evidence - a listing date that was not
verified is never invented. Fields that come from venue docs (tick sizes,
fee tiers, margin assets) are marked "unverified, verify before use" in the
record notes.

Pure stdlib, deterministic (no wall clock); research-only (CONV-2).
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from datetime import datetime, timezone

_LOG_RE = re.compile(r"^(\d{8})_([a-z]+)_([A-Z0-9]+)\.log$")


class InstrumentUnknown(Exception):
    """The (venue, symbol, timestamp) does not resolve - the run is blocked."""


@dataclass(frozen=True)
class Instrument:
    venue: str
    symbol: str
    instrument_type: str  # perp | spot | dated-future
    contract_multiplier: float
    tick_size: float
    step_size_usd: float
    quote_asset: str
    margin_asset: str
    funding_period_hours: int | None
    funding_settlements_per_day: int | None
    fee_tier: str
    taker_bps: float
    maker_bps: float
    observed_from_ns: int
    observed_to_ns: int | None = None  # None = still listed/observed
    migration: str | None = None  # "venue:symbol" the instrument migrated to
    notes: str = ""


def parse_ts_ns(iso: str) -> int:
    """ISO-8601 UTC timestamp (``2026-08-08T00:00:00Z``) to ns since epoch."""
    ts = datetime.fromisoformat(iso.replace("Z", "+00:00"))
    assert ts.tzinfo is not None and ts.utcoffset() == timezone.utc.utcoffset(None)
    return int(ts.timestamp() * 1_000_000_000)


def format_ts_ns(ts_ns: int) -> str:
    return datetime.fromtimestamp(ts_ns / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")


def load_master(path) -> dict[tuple[str, str], list[Instrument]]:
    from pathlib import Path

    out: dict[tuple[str, str], list[Instrument]] = {}
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        inst = Instrument(**json.loads(line))
        out.setdefault((inst.venue, inst.symbol), []).append(inst)
    for key in out:
        out[key].sort(key=lambda i: i.observed_from_ns)
    return out


def resolve(master, venue: str, symbol: str, ts_ns: int) -> Instrument:
    """The instrument definition in force at ``ts_ns`` - or the run is blocked.
    Versions are sorted by ``observed_from_ns``; the LAST version whose window
    contains the timestamp wins (a newer definition supersedes an older open
    window)."""
    found = None
    for inst in master.get((venue, symbol), []):
        if inst.observed_from_ns <= ts_ns and (
            inst.observed_to_ns is None or ts_ns < inst.observed_to_ns
        ):
            found = inst
    if found is None:
        raise InstrumentUnknown(
            f"no instrument definition for {venue}:{symbol} at {ts_ns}"
        )
    return found


def corpus_logs(raw_dir) -> list[tuple[str, str, str]]:
    """(venue, symbol, YYYYMMDD) for every market log in the raw corpus.
    Trace/watchdog/census files are not instruments and are ignored."""
    from pathlib import Path

    out: list[tuple[str, str, str]] = []
    for name in sorted(Path(raw_dir).glob("*.log")):
        m = _LOG_RE.match(name.name)
        if not m or m.group(3) == "positions":  # whale census is not an instrument
            continue
        out.append((m.group(2), m.group(3), m.group(1)))
    return out


def day_start_ns(yyyymmdd: str) -> int:
    return parse_ts_ns(f"{yyyymmdd[:4]}-{yyyymmdd[4:6]}-{yyyymmdd[6:]}T00:00:00Z")


def check(master, logs) -> list[str]:
    """Every corpus (venue, symbol, day) resolves; problems block the run."""
    problems: list[str] = []
    for venue, symbol, day in logs:
        try:
            resolve(master, venue, symbol, day_start_ns(day))
        except InstrumentUnknown as exc:
            problems.append(f"{day} {exc}")
    return problems


def render(master) -> str:
    rows = []
    for (venue, symbol), versions in sorted(master.items()):
        for inst in versions:
            to = format_ts_ns(inst.observed_to_ns) if inst.observed_to_ns else "open"
            rows.append(
                f"| {venue}:{symbol} | {inst.instrument_type} | "
                f"{format_ts_ns(inst.observed_from_ns)}..{to} | "
                f"{inst.taker_bps}/{inst.maker_bps} bps | {inst.notes} |"
            )
    lines = [
        "# Point-in-time instrument master (roadmap Phase 3.3)",
        "",
        "| venue:symbol | type | observed window | taker/maker | notes |",
        "|---|---|---|---|---|",
        *rows,
    ]
    return "\n".join(lines) + "\n"
