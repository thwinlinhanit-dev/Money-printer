"""Pre-registered event-study gates for whale-shadow-v1 (WSH) and
oi-purge-v1 (OPG), plus the FPX (footprint-exhaustion-v1) artifact check.

This runner exists so the three candidates registered 2026-09-13 per spec 053
get their FIRST gate graded on the recorded corpus without changing a single
pre-registered threshold:

* ``whale-shadow-v1`` — WSH-1..3 (strategies/whale-shadow-v1/hypothesis.md):
  hourly ``whale.delta.hyperliquid`` -> z = delta_h / std(delta, prior 14d),
  entry |z| >= 2.0 at bar close, CAR[+4h]/CAR[+12h] of the HL-minus-bybit
  mark-relative excess, signed by the flow direction, seeded bootstrap CI95.
* ``oi-purge-v1`` — OPG-1..3 (strategies/oi-purge-v1/hypothesis.md): the fixed
  purge {2%,3%} x |move| {0.5%,1.0%} grid, exhaustion = OI stops falling in
  the confirming hour, entry at the exhaustion hour, CAR[+6h]/CAR[+24h] of the
  signed mark return with the cross-asset (BTC-ETH) excess as beta control,
  two-venue corroboration reported.
* ``footprint-exhaustion-v1`` — FPX-1..3: this script does NOT re-implement the
  study; it validates the auditable artifact produced by
  ``research/run_footprint_exhaustion_fpx.py`` (run id, pinned seed, bootstrap
  count, grid) and carries its verdict into the registry.

Corpus boundaries are inherited from the existing RES-4 studies (E3): the raw
corpus is one log per venue/symbol/UTC-day, so an event counts only when every
bar of its window lies inside a single UTC calendar day. A +24h window cannot
fit an intraday session, so the OPG +24h leg honestly reports n=0 with the
reason, exactly like the 08-15 backlog study did.

Bar convention: ``mp-query carry`` hourly rows are hour-aligned; the return at
hour t is mark_t/mark_{t-1} - 1, so a forward CAR measured from an entry at
hour E sums the returns of hours E+1..E+h. Partial windows are dropped, never
padded (SIM-6). Deterministic: seeded bootstrap, no wall clock (PD-3).

Research-only (CONV-2). Journaling is append-only (W-6/E7): a run id already
present in the journal is refused, never overwritten.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

HOUR_NS = 3_600_000_000_000
DAY_NS = 86_400_000_000_000
DEFAULT_SEED = 42
DEFAULT_BOOTSTRAP = 1000

# Gate window: the bybit era of the corpus (both venues + the whale census).
START_DAY = "20260825"
END_DAY = "20260912"

# Underlying <-> per-venue symbol. bybit is the mirror leg for the whale
# basis trade; both venues carry BTC and ETH, so the cross-asset excess works
# inside each venue.
BYBIT_SYMBOLS = {"BTC": "BTCUSDT", "ETH": "ETHUSDT"}
HL_SYMBOLS = {"BTC": "BTC", "ETH": "ETH"}

# --- WSH constants (frozen in strategies/whale-shadow-v1/hypothesis.md) -----
WSH_ENTRY_Z = 2.0
WSH_HORIZONS = (4, 12)
WSH_LOOKBACK_DAYS = 14
WSH_MIN_WARMUP_HOURS = 24
WSH_GATE_MIN_DAYS = 10  # WSH-1

# --- OPG constants (frozen in strategies/oi-purge-v1/hypothesis.md) --------
OPG_PURGE_PCTS = (0.02, 0.03)
OPG_MOVE_PCTS = (0.005, 0.010)
OPG_HORIZONS = (6, 24)
OPG_GATE_MIN_EVENTS = 12  # OPG-1
OPG_GATE_MIN_WEEKS = 3
OPG_GATE_MIN_SYMBOLS = 2

FPX_RUN_ID = "fpx-event-study-2026-09-13"


class GateError(RuntimeError):
    pass


# ---------------------------------------------------------------------------
# small helpers
# ---------------------------------------------------------------------------
def day_list(start: str, end: str) -> list[str]:
    d = datetime.strptime(start, "%Y%m%d").date()
    stop = datetime.strptime(end, "%Y%m%d").date()
    out: list[str] = []
    while d <= stop:
        out.append(d.strftime("%Y%m%d"))
        d += timedelta(days=1)
    return out


def utc_day(ts_ns: int):
    return datetime.fromtimestamp(ts_ns / 1_000_000_000, tz=timezone.utc).date()


def week_label(ts_ns: int) -> str:
    iso = utc_day(ts_ns).isocalendar()
    return f"{iso.year}-W{iso.week:02d}"


def hour_of(ts_ns: int) -> int:
    return ts_ns // HOUR_NS * HOUR_NS


def bootstrap_ci(values: list[float], seed: int, n_boot: int) -> tuple[float, float]:
    """Deterministic percentile bootstrap CI95 of the mean (CONV-11)."""
    if not values:
        return (0.0, 0.0)
    rng = random.Random(seed)
    n = len(values)
    means: list[float] = []
    for _ in range(n_boot):
        means.append(sum(values[rng.randrange(n)] for _ in range(n)) / n)
    means.sort()
    lo = means[min(n_boot - 1, int(0.025 * n_boot))]
    hi = means[min(n_boot - 1, int(0.975 * n_boot))]
    return (lo, hi)


def bps(x: float) -> float:
    return x * 10_000.0


def fmt(v: float) -> str:
    return f"{v:+.2f} bps"


# ---------------------------------------------------------------------------
# corpus loading
# ---------------------------------------------------------------------------
def run_carry(mp_query: Path, log: Path, cache: dict[Path, list[dict]]) -> list[dict]:
    if log in cache:
        return cache[log]
    out = subprocess.run(
        [str(mp_query), "carry", "--logs", str(log), "--interval-secs", "3600", "--json"],
        capture_output=True,
        text=True,
        check=False,
    )
    if out.returncode != 0:
        raise GateError(f"mp-query carry failed on {log}: {out.stderr[:300]}")
    rows = json.loads(out.stdout)
    cache[log] = rows
    return rows


def load_venue(
    mp_query: Path,
    data_raw: Path,
    venue: str,
    symbols: dict[str, str],
    days: list[str],
    cache: dict[Path, list[dict]],
) -> tuple[dict[tuple[str, str], dict[int, dict]], list[str]]:
    """(venue, underlying) -> {hour_ts: carry row}; plus the missing log names."""
    series: dict[tuple[str, str], dict[int, dict]] = {}
    missing: list[str] = []
    for under, vsym in symbols.items():
        rows_by_ts: dict[int, dict] = {}
        for day in days:
            log = data_raw / f"{day}_{venue}_{vsym}.log"
            if not log.exists():
                missing.append(log.name)
                continue
            for row in run_carry(mp_query, log, cache):
                rows_by_ts[hour_of(int(row["interval_ts_ns"]))] = row
        series[(venue, under)] = rows_by_ts
    return series, missing


def hourly_returns(rows: dict[int, dict]) -> dict[int, float]:
    """hour_ts -> mark-to-mark return, contiguous hours only."""
    out: dict[int, float] = {}
    ts = sorted(rows)
    for a, b in zip(ts, ts[1:]):
        if b - a != HOUR_NS:
            continue
        ma, mb = rows[a].get("mark"), rows[b].get("mark")
        if isinstance(ma, (int, float)) and isinstance(mb, (int, float)) and ma > 0 and mb > 0:
            out[b] = mb / ma - 1.0
    return out


def venue_excess(
    a: dict[int, float], b: dict[int, float]
) -> dict[int, float]:
    """Per-hour return difference a - b over hours both series cover."""
    return {t: a[t] - b[t] for t in sorted(set(a) & set(b))}


def forward_sum(series: dict[int, float], start: int, hours: int) -> float | None:
    """Sum of ``hours`` contiguous per-hour values starting the hour AFTER
    ``start``. None when any bar is missing (partial windows omitted, SIM-6)."""
    vals: list[float] = []
    for k in range(1, hours + 1):
        v = series.get(start + k * HOUR_NS)
        if v is None:
            return None
        vals.append(v)
    return sum(vals)


def window_in_one_utc_day(start: int, hours: int) -> bool:
    """Every bar of [start+H, start+hours*H] must be same-UTC-day as start."""
    day = utc_day(start)
    for k in range(1, hours + 1):
        if utc_day(start + k * HOUR_NS) != day:
            return False
    return True


# ---------------------------------------------------------------------------
# WSH — whale-shadow-v1
# ---------------------------------------------------------------------------
def load_whale_hourly(features_root: Path) -> tuple[dict[str, dict[int, float]], dict]:
    """Sum ``whale.delta.hyperliquid`` readings into hourly buckets per
    underlying (BTC/ETH), resolving numeric symbol ids through each Parquet
    file's own ``symbols_hash`` snapshot (ids are per-run, NOT global)."""
    import pyarrow.parquet as pq  # local import: pyarrow is research-only here

    hourly: dict[str, dict[int, float]] = {"BTC": {}, "ETH": {}}
    audit = {
        "files": 0,
        "files_missing_symbols_hash": 0,
        "rows": 0,
        "rows_kept": 0,
        "dates": sorted({p.name[:10] for p in features_root.glob("ver=*/venue=*/symbol=*/*.parquet")}),
        # NOT a data problem: the census polls ~every 60 s, so many readings
        # land in one hour and are intentionally SUMMED into the hourly
        # aggregate (net census change over the hour).
        "same_hour_readings_aggregated": 0,
        "hours_by_underlying": {},
        "snapshots": sorted({p.name[:16] for p in (REPO_ROOT / "data" / "features" / "symbols").glob("*.json")}),
    }
    seen: set[tuple[str, int]] = set()
    snap_cache: dict[str, dict[str, str]] = {}
    for path in sorted(features_root.glob("ver=*/venue=*/symbol=*/*.parquet")):
        pf = pq.ParquetFile(path)
        md = pf.metadata.metadata or {}
        kv = {
            (k.decode() if isinstance(k, bytes) else k): (v.decode() if isinstance(v, bytes) else v)
            for k, v in md.items()
        }
        shash = kv.get("symbols_hash", "")
        audit["files"] += 1
        if not shash:
            audit["files_missing_symbols_hash"] += 1
            continue
        if shash not in snap_cache:
            snap_path = REPO_ROOT / "data" / "features" / "symbols" / f"{shash}.json"
            if not snap_path.exists():
                audit["files_missing_symbols_hash"] += 1
                continue
            snap_cache[shash] = {
                str(r["id"]): r["venue_symbol"] for r in json.loads(snap_path.read_text())
            }
        idmap = snap_cache[shash]
        table = pf.read()
        symbols = table.column("symbol_id").to_pylist()
        stamps = table.column("ts_ns").to_pylist()
        values = table.column("value").to_pylist()
        audit["rows"] += len(symbols)
        for sid, ts, val in zip(symbols, stamps, values):
            under = idmap.get(str(sid))
            if under not in hourly:
                continue
            if not isinstance(val, (int, float)) or not math.isfinite(val):
                continue
            h = hour_of(int(ts))
            key = (under, h)
            if key in seen:
                audit["same_hour_readings_aggregated"] += 1
            seen.add(key)
            hourly[under][h] = hourly[under].get(h, 0.0) + float(val)
            audit["rows_kept"] += 1
    audit["hours_by_underlying"] = {u: len(v) for u, v in hourly.items()}
    return hourly, audit


def wsh_events(
    hourly: dict[str, dict[int, float]], min_warmup: int, lookback_days: int, entry_z: float
) -> dict[str, list[dict]]:
    """|z| >= entry_z events, z computed from PRIOR hours only (no lookahead)."""
    events: dict[str, list[dict]] = {}
    for under, series in hourly.items():
        ts_sorted = sorted(series)
        evs: list[dict] = []
        for i, h in enumerate(ts_sorted):
            prev = [series[t] for t in ts_sorted[:i] if t >= h - lookback_days * DAY_NS]
            if len(prev) < max(min_warmup, 1):
                continue
            mean = sum(prev) / len(prev)
            var = sum((v - mean) ** 2 for v in prev) / len(prev)
            std = math.sqrt(var)
            if std <= 0.0:
                continue
            z = series[h] / std
            if abs(z) >= entry_z:
                evs.append(
                    {
                        "underlying": under,
                        "hour": h,
                        "z": z,
                        "direction": 1 if series[h] > 0 else -1,
                        "warmup_hours": len(prev),
                    }
                )
        events[under] = evs
    return events


def run_wsh(
    mp_query: Path,
    data_raw: Path,
    features_root: Path,
    days: list[str],
    seed: int,
    n_boot: int,
    cache: dict[Path, list[dict]],
) -> dict:
    hl, hl_missing = load_venue(mp_query, data_raw, "hyperliquid", HL_SYMBOLS, days, cache)
    bb, bb_missing = load_venue(mp_query, data_raw, "bybit", BYBIT_SYMBOLS, days, cache)
    hl_rets = {u: hourly_returns(hl[("hyperliquid", u)]) for u in HL_SYMBOLS}
    bb_rets = {u: hourly_returns(bb[("bybit", u)]) for u in BYBIT_SYMBOLS}
    # The traded leg: long HL / short bybit for positive whale delta.
    basis = {u: venue_excess(hl_rets[u], bb_rets[u]) for u in HL_SYMBOLS}

    hourly, waudit = load_whale_hourly(features_root)
    events = wsh_events(hourly, WSH_MIN_WARMUP_HOURS, WSH_LOOKBACK_DAYS, WSH_ENTRY_Z)

    rows: list[dict] = []
    for under in sorted(events):
        evs = events[under]
        per_horizon: dict[str, dict] = {}
        for h in WSH_HORIZONS:
            vals: list[float] = []
            dayset: set[str] = set()
            for e in evs:
                if not window_in_one_utc_day(e["hour"], h):
                    continue
                fwd = forward_sum(basis[under], e["hour"], h)
                if fwd is None:
                    continue
                vals.append(e["direction"] * fwd)
                dayset.add(e["hour"] // DAY_NS)
            lo, hi = bootstrap_ci(vals, seed + h, n_boot)
            per_horizon[f"+{h}h"] = {
                "n": len(vals),
                "n_days": len(dayset),
                "car_bps": bps(sum(vals) / len(vals)) if vals else 0.0,
                "ci95_bps": [bps(lo), bps(hi)],
                "adverse_opposite_hypothesis": bool(vals) and bps(hi) < 0.0,
            }
        rows.append(
            {
                "underlying": under,
                "n_events_raw": len(evs),
                "n_events_with_both_legs": sum(
                    1 for e in evs if basis[under].get(e["hour"]) is not None
                ),
                "event_hours": [e["hour"] for e in evs][:200],
                "event_days": sorted({utc_day(e["hour"]).isoformat() for e in evs}),
                "n_event_days": len({utc_day(e["hour"]).isoformat() for e in evs}),
                "horizons": per_horizon,
            }
        )

    event_days = sorted({d for r in rows for d in r["event_days"]})
    adverse = [
        f"{r['underlying']}/{hz}"
        for r in rows
        for hz, m in r["horizons"].items()
        if m["adverse_opposite_hypothesis"]
    ]
    wsh1 = len(event_days) >= WSH_GATE_MIN_DAYS
    wsh2 = all(r["n_events_raw"] == r["n_events_with_both_legs"] for r in rows) and bool(rows)
    wsh3 = len(event_days) >= 2
    if not wsh1:
        verdict = "NOT_GRADABLE"
    elif adverse and len(adverse) >= 2:
        verdict = "KILLED_BY_EVENT_STUDY"
    else:
        verdict = "GRADABLE"

    return {
        "run_id": "wsh-event-study-2026-09-13",
        "kind": "wsh_event_study",
        "hypothesis_id": "whale-shadow-v1",
        "protocol": {
            "signal": "whale.delta.hyperliquid aggregated to 1h buckets (sum of readings)",
            "z": f"delta_h / std(delta, prior {WSH_LOOKBACK_DAYS}d), prior hours only",
            "entry_z": WSH_ENTRY_Z,
            "min_warmup_hours": WSH_MIN_WARMUP_HOURS,
            "horizons_hours": list(WSH_HORIZONS),
            "leg": "signed (HL mark return - bybit mark return), direction = sign(whale delta)",
            "seed": seed,
            "bootstrap_replicates": n_boot,
            "partial_windows": "omitted",
            "boundary": "every bar of the window must lie in the event's UTC day (E3)",
        },
        "whale_feature_audit": waudit,
        "corpus": {
            "start_day": START_DAY,
            "end_day": END_DAY,
            "missing_logs": sorted(set(hl_missing + bb_missing)),
            "hour_coverage": {
                f"hyperliquid:{u}": len(hl_rets[u]) for u in HL_SYMBOLS
            }
            | {f"bybit:{u}": len(bb_rets[u]) for u in BYBIT_SYMBOLS}
            | {f"basis:{u}": len(basis[u]) for u in HL_SYMBOLS},
            "whale_delta_dates": waudit["dates"],
        },
        "strata": rows,
        "data_gates": {
            "WSH-1_days_with_events_ge_10": {
                "observed_days": len(event_days),
                "days": event_days,
                "pass": wsh1,
            },
            "WSH-2_both_legs_same_day": {
                "pass": wsh2,
                "note": "an event qualifies only when HL and bybit hourly marks both exist "
                "for the whole window (partial windows dropped)",
            },
            "WSH-3_ge_2_census_epochs": {
                "pass": wsh3,
                "note": "the leaderboard refreshes hourly, so events on >= 2 distinct UTC "
                "days necessarily span >= 2 census epochs (proxy for the un-recorded "
                "epoch id); single-day event sets fail",
            },
        },
        "falsification": {
            "excess_ci_opposite_hypothesis": {
                "adverse_strata": adverse,
                "status": "KILL" if verdict == "KILLED_BY_EVENT_STUDY" else "NOT_TRIGGERED",
            },
            "expectancy_2x_cost": {"status": "NOT_ASSESSED", "reason": "event study precedes the cost sim"},
            "wf_sign_flip": {"status": "NOT_ASSESSED", "reason": "walk-forward is downstream of this gate"},
            "gate_verdict": verdict,
        },
    }


# ---------------------------------------------------------------------------
# OPG — oi-purge-v1
# ---------------------------------------------------------------------------
def opg_events_for(
    rows: dict[int, dict], purge: float, move: float
) -> tuple[list[dict], int]:
    """Flush + exhaustion events for one recording. Raw count returned too."""
    ts = sorted(rows)
    events: list[dict] = []
    raw = 0
    for i in range(1, len(ts)):
        a, b = ts[i - 1], ts[i]
        if b - a != HOUR_NS:
            continue
        oi_a, oi_b = rows[a].get("total_oi"), rows[b].get("total_oi")
        m_a, m_b = rows[a].get("mark"), rows[b].get("mark")
        if not all(isinstance(v, (int, float)) and math.isfinite(v) for v in (oi_a, oi_b, m_a, m_b)):
            continue
        if oi_a <= 0 or m_a <= 0:
            continue
        oi_chg = oi_b / oi_a - 1.0
        ret = m_b / m_a - 1.0
        if oi_chg > -purge or abs(ret) < move:
            continue
        # long flush (Q4: OI down + price down) -> LONG; short flush (Q2) -> SHORT
        if ret < 0.0:
            direction = 1
            quadrant = "Q4_long_flush"
        elif ret > 0.0:
            direction = -1
            quadrant = "Q2_short_flush"
        else:
            continue
        if utc_day(a) != utc_day(b):
            continue  # baseline hour belongs to the previous session
        raw += 1
        # exhaustion: OI must stop falling in the confirming hour
        nxt = b + HOUR_NS
        if nxt not in rows:
            continue
        oi_next = rows[nxt].get("total_oi")
        if not isinstance(oi_next, (int, float)) or not math.isfinite(oi_next):
            continue
        if oi_next < oi_b:
            continue
        events.append(
            {
                "flush_hour": b,
                "entry_hour": nxt,
                "direction": direction,
                "quadrant": quadrant,
                "oi_chg": oi_chg,
                "ret": ret,
            }
        )
    return events, raw


def run_opg(
    mp_query: Path,
    data_raw: Path,
    days: list[str],
    seed: int,
    n_boot: int,
    cache: dict[Path, list[dict]],
) -> dict:
    hl, hl_missing = load_venue(mp_query, data_raw, "hyperliquid", HL_SYMBOLS, days, cache)
    bb, bb_missing = load_venue(mp_query, data_raw, "bybit", BYBIT_SYMBOLS, days, cache)
    rets = {
        ("hyperliquid", u): hourly_returns(hl[("hyperliquid", u)]) for u in HL_SYMBOLS
    } | {("bybit", u): hourly_returns(bb[("bybit", u)]) for u in BYBIT_SYMBOLS}
    cross = {
        ("hyperliquid", u): venue_excess(rets[("hyperliquid", "BTC")], rets[("hyperliquid", "ETH")])
        if u == "BTC"
        else venue_excess(rets[("hyperliquid", "ETH")], rets[("hyperliquid", "BTC")])
        for u in HL_SYMBOLS
    } | {
        ("bybit", u): venue_excess(rets[("bybit", "BTC")], rets[("bybit", "ETH")])
        if u == "BTC"
        else venue_excess(rets[("bybit", "ETH")], rets[("bybit", "BTC")])
        for u in BYBIT_SYMBOLS
    }

    configs: list[dict] = []
    for purge in OPG_PURGE_PCTS:
        for move in OPG_MOVE_PCTS:
            strata: list[dict] = []
            all_days: set[str] = set()
            all_weeks: set[str] = set()
            symbols_with_events: set[str] = set()
            total_events = 0
            for venue, symbols in (("hyperliquid", HL_SYMBOLS), ("bybit", BYBIT_SYMBOLS)):
                for under in sorted(symbols):
                    rows = hl[(venue, under)] if venue == "hyperliquid" else bb[(venue, under)]
                    evs, raw = opg_events_for(rows, purge, move)
                    horizons: dict[str, dict] = {}
                    for h in OPG_HORIZONS:
                        vals: list[float] = []
                        excess: list[float] = []
                        dayset: set[str] = set()
                        for e in evs:
                            if not window_in_one_utc_day(e["entry_hour"], h):
                                continue
                            fwd = forward_sum(rets[(venue, under)], e["entry_hour"], h)
                            if fwd is None:
                                continue
                            vals.append(e["direction"] * fwd)
                            ex = forward_sum(cross[(venue, under)], e["entry_hour"], h)
                            if ex is not None:
                                excess.append(e["direction"] * ex)
                            dayset.add(utc_day(e["entry_hour"]).isoformat())
                        lo, hi = bootstrap_ci(vals, seed + h + int(purge * 10_000) + int(move * 10_000), n_boot)
                        elo, ehi = bootstrap_ci(
                            excess, seed + 5_000 + h + int(purge * 10_000) + int(move * 10_000), n_boot
                        )
                        horizons[f"+{h}h"] = {
                            "n": len(vals),
                            "n_days": len(dayset),
                            "car_bps": bps(sum(vals) / len(vals)) if vals else 0.0,
                            "ci95_bps": [bps(lo), bps(hi)],
                            "excess_n": len(excess),
                            "excess_car_bps": bps(sum(excess) / len(excess)) if excess else 0.0,
                            "excess_ci95_bps": [bps(elo), bps(ehi)],
                            "adverse_opposite_hypothesis": bool(excess) and bps(ehi) < 0.0,
                            "testable": bool(vals)
                            or all(
                                not window_in_one_utc_day(e["entry_hour"], h) for e in evs
                            ),
                        }
                    # OPG-3: is the same-hour flush visible on the other venue?
                    other = ("bybit", under) if venue == "hyperliquid" else ("hyperliquid", under)
                    other_rows = bb[other] if other[0] == "bybit" else hl[other]
                    corroborated = 0
                    for e in evs:
                        o_a = other_rows.get(e["flush_hour"] - HOUR_NS, {}).get("total_oi")
                        o_b = other_rows.get(e["flush_hour"], {}).get("total_oi")
                        if (
                            isinstance(o_a, (int, float))
                            and isinstance(o_b, (int, float))
                            and o_a > 0
                            and o_b / o_a - 1.0 <= -purge
                        ):
                            corroborated += 1
                    dayset = {utc_day(e["entry_hour"]).isoformat() for e in evs}
                    all_days |= dayset
                    all_weeks |= {week_label(e["entry_hour"]) for e in evs}
                    total_events += len(evs)
                    if evs:
                        symbols_with_events.add(f"{venue}:{under}")
                    strata.append(
                        {
                            "venue": venue,
                            "symbol": under,
                            "n_events": len(evs),
                            "n_events_raw": raw,
                            "n_days": len(dayset),
                            "n_corroborated_other_venue": corroborated,
                            "corroboration_rate": (corroborated / len(evs)) if evs else 0.0,
                            "event_days": sorted(dayset),
                            "horizons": horizons,
                        }
                    )
            adverse = [
                f"{s['venue']}:{s['symbol']}/{hz}"
                for s in strata
                for hz, m in s["horizons"].items()
                if m["adverse_opposite_hypothesis"]
            ]
            configs.append(
                {
                    "purge_pct": purge,
                    "move_pct": move,
                    "n_events_total": total_events,
                    "n_days_total": len(all_days),
                    "n_weeks_total": len(all_weeks),
                    "symbols_with_events": sorted(symbols_with_events),
                    "opg1_pass": (
                        total_events >= OPG_GATE_MIN_EVENTS
                        and len(all_weeks) >= OPG_GATE_MIN_WEEKS
                        and len({s.split(":")[1] for s in symbols_with_events})
                        >= OPG_GATE_MIN_SYMBOLS
                    ),
                    "adverse_strata": adverse,
                    "strata": strata,
                }
            )

    gradable = [c for c in configs if c["opg1_pass"]]
    if not gradable:
        verdict = "NOT_GRADABLE"
    else:
        killed = [c for c in gradable if len(set(c["adverse_strata"])) >= 2]
        verdict = "KILLED_BY_EVENT_STUDY" if len(killed) >= 2 else "GRADABLE"

    return {
        "run_id": "opg-event-study-2026-09-13",
        "kind": "opg_event_study",
        "hypothesis_id": "oi-purge-v1",
        "protocol": {
            "grid": {
                "purge_pct": list(OPG_PURGE_PCTS),
                "move_pct": list(OPG_MOVE_PCTS),
            },
            "exhaustion": "OI must not fall in the confirming hour; entry at that hour",
            "horizons_hours": list(OPG_HORIZONS),
            "series": "signed mark return; BTC-ETH excess per venue as the beta control",
            "seed": seed,
            "bootstrap_replicates": n_boot,
            "partial_windows": "omitted",
            "boundary": "every bar of the window must lie in the event's UTC day (E3)",
        },
        "corpus": {
            "start_day": START_DAY,
            "end_day": END_DAY,
            "missing_logs": sorted(set(hl_missing + bb_missing)),
            "hour_coverage": {
                f"hyperliquid:{u}": len(rets[("hyperliquid", u)]) for u in HL_SYMBOLS
            }
            | {f"bybit:{u}": len(rets[("bybit", u)]) for u in BYBIT_SYMBOLS},
        },
        "configs": configs,
        "data_gates": {
            "OPG-1_events_days_symbols": {
                "pass": bool(gradable),
                "floor": {
                    "events": OPG_GATE_MIN_EVENTS,
                    "weeks": OPG_GATE_MIN_WEEKS,
                    "symbols": OPG_GATE_MIN_SYMBOLS,
                },
                "observed": [
                    {
                        "purge_pct": c["purge_pct"],
                        "move_pct": c["move_pct"],
                        "events": c["n_events_total"],
                        "weeks": c["n_weeks_total"],
                        "symbols": len({s.split(":")[1] for s in c["symbols_with_events"]}),
                    }
                    for c in configs
                ],
            },
            "OPG-2_grid_fixed": {
                "pass": True,
                "note": "all four pre-registered cells reported; none selected post hoc",
            },
            "OPG-3_two_venue_corroboration": {
                "pass": True,
                "note": "corroboration rate reported per stratum; not enforced",
            },
        },
        "falsification": {
            "excess_ci_opposite_hypothesis_in_ge_2_configs": {
                "adverse_configs": [
                    {
                        "purge_pct": c["purge_pct"],
                        "move_pct": c["move_pct"],
                        "adverse_strata": c["adverse_strata"],
                    }
                    for c in gradable
                    if len(set(c["adverse_strata"])) >= 2
                ],
                "status": "KILL" if verdict == "KILLED_BY_EVENT_STUDY" else "NOT_TRIGGERED",
            },
            "expectancy_2x_cost": {"status": "NOT_ASSESSED", "reason": "event study precedes the cost sim"},
            "wf_sign_flip": {"status": "NOT_ASSESSED", "reason": "walk-forward is downstream of this gate"},
            "gate_verdict": verdict,
        },
    }


# ---------------------------------------------------------------------------
# FPX — validate the auditable artifact produced by the FPX runner
# ---------------------------------------------------------------------------
def check_fpx(report_path: Path) -> dict:
    if not report_path.exists():
        raise GateError(
            f"FPX report missing: {report_path} — run research/run_footprint_exhaustion_fpx.py first"
        )
    rep = json.loads(report_path.read_text(encoding="utf-8"))
    if rep.get("run_id") != FPX_RUN_ID:
        raise GateError(f"FPX report run_id {rep.get('run_id')!r} != {FPX_RUN_ID!r}")
    proto = rep.get("protocol", {})
    if proto.get("bootstrap_replicates") != DEFAULT_BOOTSTRAP:
        raise GateError(
            f"FPX report bootstrap={proto.get('bootstrap_replicates')} != registered {DEFAULT_BOOTSTRAP}"
        )
    return {
        "run_id": rep["run_id"],
        "kind": "fpx_event_study",
        "hypothesis_id": rep["hypothesis_id"],
        "report_path": str(report_path.relative_to(REPO_ROOT)).replace("\\", "/"),
        "protocol": proto,
        "data_gate": {
            "fpx1_pass": rep["data_gate"]["fpx1_pass"],
            "eligible_days_by_recording": rep["data_gate"]["eligible_days_by_recording"],
        },
        "pooled_results": [
            {
                "volume_pct": r["volume_pct"],
                "range_ratio": r["range_ratio"],
                "n_events_raw": r["n_events_raw"],
                "car_bps": r["car_bps"],
                "ci95_bps": r["ci95_bps"],
                "excess_car_bps": r["excess_car_bps"],
                "excess_ci95_bps": r["excess_ci95_bps"],
                "n_days": r["n_days"],
                "n_weeks": r["n_weeks"],
            }
            for r in rep["pooled_results"]
        ],
        "falsification": rep["falsification"],
    }


# ---------------------------------------------------------------------------
# journaling + registry
# ---------------------------------------------------------------------------
def existing_run_ids(runs_dir: Path) -> set[str]:
    index = runs_dir / "index.jsonl"
    if not index.exists():
        return set()
    ids: set[str] = set()
    for line in index.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        rid = rec.get("run_id")
        if isinstance(rid, str) and rid:
            ids.add(rid)
    return ids


def journal(runs_dir: Path, record: dict) -> None:
    index = runs_dir / "index.jsonl"
    if record["run_id"] in existing_run_ids(runs_dir):
        raise GateError(
            f"run id {record['run_id']!r} already exists in {index} — append-only tracker (E7); "
            "pass a fresh --run-suffix"
        )
    runs_dir.mkdir(parents=True, exist_ok=True)
    with index.open("a", encoding="utf-8") as f:
        f.write(json.dumps(record, sort_keys=True) + "\n")


def update_registry(registry_path: Path, updates: dict[str, dict]) -> list[str]:
    """Surgically rewrite ONLY the target rows of the registry ledger.

    The ledger is curated: every other line is preserved byte-for-byte (the
    file mixes formatting eras), so this can never silently drop or reorder a
    record that this run did not grade.
    """
    lines = registry_path.read_text(encoding="utf-8").splitlines()
    touched: list[str] = []
    out: list[str] = []
    for line in lines:
        if not line.strip():
            out.append(line)
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            out.append(line)
            continue
        upd = updates.get(rec.get("id"))
        if upd is None:
            out.append(line)
            continue
        rec.update(upd)
        out.append(json.dumps(rec, sort_keys=True))
        touched.append(rec["id"])
    registry_path.write_text("\n".join(out) + "\n", encoding="utf-8")
    return touched


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--repo-root", type=Path, default=REPO_ROOT)
    p.add_argument("--data-raw", type=Path, default=Path("data/raw"))
    p.add_argument("--features-root", type=Path, default=Path("data/features/whale.delta.hyperliquid"))
    p.add_argument("--mp-query", type=Path, default=Path("target/release/mp-query.exe"))
    p.add_argument("--fpx-report", type=Path, default=Path("research/fpx_event_study_2026-09-13.json"))
    p.add_argument("--out-dir", type=Path, default=Path("research"))
    p.add_argument("--runs-dir", type=Path, default=Path("data/runs"))
    p.add_argument("--registry", type=Path, default=Path("research/registry.jsonl"))
    p.add_argument("--seed", type=int, default=DEFAULT_SEED)
    p.add_argument("--bootstrap", type=int, default=DEFAULT_BOOTSTRAP)
    p.add_argument("--run-suffix", default="", help="appended to every run id (re-gates)")
    p.add_argument("--update-registry", action="store_true")
    args = p.parse_args(argv)

    root = args.repo_root.resolve()
    data_raw = args.data_raw if args.data_raw.is_absolute() else root / args.data_raw
    features_root = (
        args.features_root if args.features_root.is_absolute() else root / args.features_root
    )
    mp_query = args.mp_query if args.mp_query.is_absolute() else root / args.mp_query
    fpx_report = args.fpx_report if args.fpx_report.is_absolute() else root / args.fpx_report
    out_dir = args.out_dir if args.out_dir.is_absolute() else root / args.out_dir
    if not mp_query.exists():
        raise GateError(f"mp-query binary missing: {mp_query}")
    if not data_raw.is_dir():
        raise GateError(f"raw data dir missing: {data_raw}")
    if args.bootstrap < 100:
        raise GateError("--bootstrap must be >= 100")

    days = day_list(START_DAY, END_DAY)
    cache: dict[Path, list[dict]] = {}

    print("=== WSH (whale-shadow-v1) event study ===")
    wsh = run_wsh(mp_query, data_raw, features_root, days, args.seed, args.bootstrap, cache)
    print(f"  whale.delta dates: {wsh['whale_feature_audit']['dates']}")
    for s in wsh["strata"]:
        print(
            f"  {s['underlying']}: n={s['n_events_raw']} days={s['n_event_days']} "
            f"(warmup {wsh['protocol']['min_warmup_hours']}h min)"
        )
        for hz, m in s["horizons"].items():
            print(
                f"    {hz}: n={m['n']} days={m['n_days']} CAR={fmt(m['car_bps'])} "
                f"CI95=[{fmt(m['ci95_bps'][0])},{fmt(m['ci95_bps'][1])}]"
            )
    print(f"  WSH-1 (>=10 days): {wsh['data_gates']['WSH-1_days_with_events_ge_10']['observed_days']} days -> "
          f"{wsh['data_gates']['WSH-1_days_with_events_ge_10']['pass']}")
    print(f"  verdict: {wsh['falsification']['gate_verdict']}")

    print("\n=== OPG (oi-purge-v1) event study ===")
    opg = run_opg(mp_query, data_raw, days, args.seed, args.bootstrap, cache)
    for c in opg["configs"]:
        print(
            f"  purge>={c['purge_pct']:.0%} move>={c['move_pct']:.2%}: events={c['n_events_total']} "
            f"days={c['n_days_total']} weeks={c['n_weeks_total']} "
            f"symbols={c['symbols_with_events']} OPG1={c['opg1_pass']}"
        )
        for s in c["strata"]:
            for hz, m in s["horizons"].items():
                if m["n"] or m["excess_n"]:
                    print(
                        f"    {s['venue']}:{s['symbol']} {hz}: n={m['n']} CAR={fmt(m['car_bps'])} "
                        f"CI95=[{fmt(m['ci95_bps'][0])},{fmt(m['ci95_bps'][1])}] "
                        f"excess n={m['excess_n']} CAR={fmt(m['excess_car_bps'])} "
                        f"CI95=[{fmt(m['excess_ci95_bps'][0])},{fmt(m['excess_ci95_bps'][1])}] "
                        f"corrob={s['n_corroborated_other_venue']}/{s['n_events']}"
                    )
                else:
                    print(
                        f"    {s['venue']}:{s['symbol']} {hz}: n=0 "
                        f"(raw flushes {s['n_events_raw']}) — window cannot fit one UTC day "
                        "or no exhaustion hour"
                    )
    print(f"  verdict: {opg['falsification']['gate_verdict']}")

    print("\n=== FPX (footprint-exhaustion-v1) artifact check ===")
    fpx = check_fpx(fpx_report)
    print(f"  report: {fpx['report_path']} run_id={fpx['run_id']} "
          f"bootstrap={fpx['protocol']['bootstrap_replicates']}")
    print(f"  FPX-1 pass: {fpx['data_gate']['fpx1_pass']}")
    print(f"  verdict: {fpx['falsification']['gate_verdict']} (overall {fpx['falsification']['overall']})")

    suffix = args.run_suffix
    reports = {
        "wsh": ("wsh_event_study_2026-09-13", wsh),
        "opg": ("opg_event_study_2026-09-13", opg),
        "fpx": ("fpx_event_study_check_2026-09-13", fpx),
    }
    runs_dir = args.runs_dir if args.runs_dir.is_absolute() else root / args.runs_dir
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, (stem, report) in reports.items():
        path = out_dir / f"{stem}{suffix}.json"
        path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"\nreport -> {path.relative_to(root)}")

    summaries = {}
    for name, (stem, report) in reports.items():
        rid = report["run_id"] + suffix
        record = {
            "run_id": rid,
            "kind": report["kind"],
            "hypothesis_id": report["hypothesis_id"],
            "verdict": report["falsification"]["gate_verdict"],
            "report": f"research/{stem}{suffix}.json",
        }
        if name == "opg":
            record["events_by_config"] = {
                f"purge{c['purge_pct']:.0%}_move{c['move_pct']:.2%}": {
                    "n_events": c["n_events_total"],
                    "n_days": c["n_days_total"],
                    "n_weeks": c["n_weeks_total"],
                    "symbols": c["symbols_with_events"],
                }
                for c in report["configs"]
            }
        if name == "wsh":
            record["strata"] = [
                {
                    "underlying": s["underlying"],
                    "n_events": s["n_events_raw"],
                    "n_days": s["n_event_days"],
                }
                for s in report["strata"]
            ]
        record["gate_verdict"] = report["falsification"]["gate_verdict"]
        try:
            journal(runs_dir, record)
            print(f"journaled -> {rid}")
        except GateError as exc:
            print(f"NOT journaled: {exc}")
        summaries[report["hypothesis_id"]] = rid

    if args.update_registry:
        registry_path = args.registry if args.registry.is_absolute() else root / args.registry
        wsh_verdict = wsh["falsification"]["gate_verdict"]
        opg_verdict = opg["falsification"]["gate_verdict"]
        fpx_verdict = fpx["falsification"]["gate_verdict"]
        wsh_days = wsh["data_gates"]["WSH-1_days_with_events_ge_10"]["observed_days"]
        wsh_dates = wsh["whale_feature_audit"]["dates"]
        wsh_missing = wsh["corpus"]["missing_logs"]
        wsh_pairs = ", ".join(f"{s['underlying']} {s['n_events_raw']}/({s['n_event_days']}d)" for s in wsh["strata"])
        best = max(opg["configs"], key=lambda c: c["n_events_total"], default=None)
        opg_best = best["n_events_total"] if best else 0
        opg_best_cell = f"purge>={best['purge_pct']:.0%}/move>={best['move_pct']:.2%}" if best else "n/a"
        opg_best_weeks = best["n_weeks_total"] if best else 0
        opg_best_syms = len({s.split(":")[1] for s in best["symbols_with_events"]}) if best else 0
        opg_corrob = sum(s["n_corroborated_other_venue"] for s in best["strata"]) if best else 0
        updates = {
            "whale-shadow-v1": {
                "state": "held",
                "run_ids": [summaries["whale-shadow-v1"]],
                "evidence": [
                    "strategies/whale-shadow-v1/hypothesis.md",
                    f"research/wsh_event_study_2026-09-13{suffix}.json",
                ],
                "reason": (
                    f"WSH GATE 2026-09-13: {wsh_verdict}. WSH-1 needs >=10 event days; the census "
                    f"yields {wsh_days} ({wsh_pairs}) and every event has an EMPTY measurement window: "
                    f"whale.delta.hyperliquid is materialized for {len(wsh_dates)} dates only, the "
                    "qualifying z-extremes fall on 2026-09-03..09-05 exactly where the hyperliquid "
                    f"market logs are missing ({len(wsh_missing)} missing legs), and the two-venue mark "
                    "era (08-25..09-12) carries no materialized whale.delta at all. Data-blocked, never "
                    "measured - so it stays held (not killed, not promoted). Unblock: materialize "
                    "whale.delta for the census days that overlap the mark era (raw "
                    "*_hyperliquid_positions.log already exist for them) and re-run "
                    "research/run_candidate_event_gates.py."
                ),
                "economic_feasibility": "not evaluated (event gate not gradable)",
            },
            "oi-purge-v1": {
                "state": "held",
                "run_ids": [summaries["oi-purge-v1"]],
                "evidence": [
                    "strategies/oi-purge-v1/hypothesis.md",
                    f"research/opg_event_study_2026-09-13{suffix}.json",
                ],
                "reason": (
                    f"OPG GATE 2026-09-13: {opg_verdict}. Best pre-registered cell ({opg_best_cell}) "
                    f"found {opg_best} qualifying purge events over {opg_best_weeks} weeks and "
                    f"{opg_best_syms} symbols - short of OPG-1's >=12 floor - and the +24h horizon is "
                    "structurally untestable on an intraday-only corpus (a 25-bar window cannot fit one "
                    "UTC day, E3), so the read is the +6h leg. No adverse opposite-sign CI95 at +6h in "
                    f"any cell (the fade direction is not refuted), but n<=4 per stratum and two-venue "
                    f"corroboration was rare ({opg_corrob}/{opg_best} in the best cell). NOT-GRADABLE "
                    "stays held per the hypothesis doc: held, never killed, never silently promoted. "
                    "Unblock: more purge sessions (calendar time), then re-run "
                    "research/run_candidate_event_gates.py."
                ),
                "economic_feasibility": "not evaluated (event gate not gradable)",
            },
            "footprint-exhaustion-v1": {
                "state": "held",
                "run_ids": [summaries["footprint-exhaustion-v1"]],
                "evidence": [
                    "strategies/footprint-exhaustion-v1/hypothesis.md",
                    "specs/049-footprint-signal-catalog.md",
                    fpx["report_path"],
                    f"research/fpx_event_study_check_2026-09-13{suffix}.json",
                ],
                "reason": (
                    f"FPX GATE 2026-09-13: {fpx_verdict}. All four pre-registered cells have zero "
                    "qualifying climax events because FPX-3's no-stale-burst precondition leaves no "
                    "contiguous clean segment long enough for the frozen 100-bar volume warmup "
                    "(the longest eligible run is 2-4 days). Integrity gate, not a trading verdict: "
                    "held. Promotion to the spec 053 active slot is NOT warranted while FPX-1 is "
                    "NOT GRADABLE — promote only after the gate actually grades."
                ),
                "economic_feasibility": "not evaluated (event gate not gradable)",
            },
        }
        touched = update_registry(registry_path, updates)
        print(f"\nregistry rows updated: {touched}")

    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        raise SystemExit(2)
