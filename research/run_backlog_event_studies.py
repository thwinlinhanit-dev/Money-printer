"""Backlog alpha idea event studies (RES-4).

Grades backlog ideas over the recorded corpus BEFORE any hypothesis is
written (the funnel's cheapest gate). Batch 1 (2026-08-15):
oi-purge-continuation, listing-flow-v1, weekend-liquidity-v1. Batch 2
(2026-08-16): funding-arb-v1 (cross-venue funding spread) and basis-carry-v1
(dated-future vs perp basis; perp-vs-oracle proxy graded, dated-future leg
NOT TESTABLE by construction).

Data source: ``mp-query carry --logs <raw>.log --interval-secs 3600 --json``
— hourly mark / OI / funding directly from raw logs (no compaction needed,
which is quarantine-blocked on the known stale-burst days anyway). Returns are
mark-to-mark hourly returns; the oi-purge study uses cross-asset excess
(BTC − ETH) so a market-wide move does not look like an edge. The batch-2
studies grade gap dynamics (cross-venue funding spread / perp-vs-oracle
basis): event = wide gap, series = hourly change in |gap|, so a negative
CAR[+24h] reads as mean reversion toward 0.

Cross-venue funding rates are annualized per venue cadence before comparing
(hyperliquid hourly × 8760, bybit/binance 8h × 1095) — units never mixed.

Honest verdicts recorded to ``runs/index.jsonl`` (SIM-10 pattern). An idea
with no testable events in the corpus is recorded as NOT TESTABLE (n=0 with
the reason), never as a silent pass or a fabricated sample (PD-6/INT-1).
Every study row reports ``n_days`` (distinct UTC days contributing events)
plus ``ci_reliable``; rows with < 3 distinct days carry the explicit
"block-bootstrap CI is unreliable" caveat (E2). The oi-purge study gates
events to same-UTC-day windows (E3), and ``--run-id`` duplicates are refused
(append-only tracker, E7).

Deterministic: seeded bootstrap (CONV-11); no wall clock (PD-3).
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from event_study import Event, bootstrap_ci, car_by_regime, run_study  # noqa: E402

HOUR_NS = 3_600_000_000_000
DAY_NS = 86_400_000_000_000

# Corpus: hyperliquid BTC + ETH, 08-08..08-15 (both legs for cross-asset
# excess). 08-08/09 and 08-15 are weekend days (Sat/Sun UTC).
DAYS = [f"202608{d:02d}" for d in range(8, 17)]

# Funding cadence (periods per year) per venue — annualization multipliers
# for the funding-arb spread (units never mixed, spec 003).
FUNDING_PERIODS_PER_YEAR = {
    "hyperliquid": 8_760,  # funds hourly
    "bybit": 1_095,  # funds every 8h
    "binance": 1_095,  # funds every 8h
}

# funding-arb-v1: same-underlying cross-venue overlap days in the corpus.
# (venue_a, symbol_a, venue_b, symbol_b, day) — hyperliquid vs binance on
# 07-19 + 08-08 (BTC and ETH), hyperliquid vs bybit on 08-14..08-16 (BTC;
# the 08-16 bybit days drained 2026-08-17 — the third bybit overlap day,
# which unblocks the FARB-2 re-gate; 08-16 also pairs HL-ETH vs bybit-ETHUSDT).
CROSS_VENUE_PAIRS = [
    ("hyperliquid", "BTC", "binance", "BTCUSDT", "20260719"),
    ("hyperliquid", "BTC", "binance", "BTCUSDT", "20260808"),
    ("hyperliquid", "ETH", "binance", "ETHUSDT", "20260808"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260814"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260815"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260816"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260816"),
    # FARB-2 re-gate (2026-09-12): the bybit drain grew the overlap corpus
    # from 2 to 15 same-day pairs per symbol. 07-19 is intentionally NOT
    # repeated for bybit (the bybit leg is a 60 KB collector-start stub with
    # ~0 funding hours; the 07-19 calendar day is already graded via the
    # binance pair above).
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260825"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260826"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260827"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260828"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260829"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260830"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260831"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260901"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260902"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260906"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260907"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260908"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260909"),
    ("hyperliquid", "BTC", "bybit", "BTCUSDT", "20260910"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260825"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260826"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260827"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260828"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260829"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260830"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260831"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260901"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260902"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260906"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260907"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260908"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260909"),
    ("hyperliquid", "ETH", "bybit", "ETHUSDT", "20260910"),
]

# basis-carry-v1: perp-vs-oracle basis thresholds (bps) — |basis| wide = the
# venue's mark diverged from its own index (the only testable leg; dated
# futures are not recorded).
BASIS_THRESH_BPS = (4.0, 5.0, 6.0)

# funding-arb-v1: |annualized cross-venue spread| thresholds, bps/yr.
SPREAD_THRESH_BPS = (500.0, 1000.0)


def run_carry(mp_query: Path, log: Path) -> list[dict]:
    """One day's hourly carry rows from the raw log (mark/OI/funding)."""
    out = subprocess.run(
        [
            str(mp_query),
            "carry",
            "--logs",
            str(log),
            "--interval-secs",
            "3600",
            "--json",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if out.returncode != 0:
        raise RuntimeError(f"mp-query carry failed on {log}: {out.stderr[:300]}")
    rows = json.loads(out.stdout)
    return rows


def load_series(mp_query: Path, data_raw: Path) -> dict[str, dict[int, dict]]:
    """symbol -> {interval_ts_ns: row} across all corpus days."""
    series: dict[str, dict[int, dict]] = {"BTC": {}, "ETH": {}}
    for day in DAYS:
        for sym in ("BTC", "ETH"):
            log = data_raw / f"{day}_hyperliquid_{sym}.log"
            if not log.exists():
                print(f"  skip (missing): {log.name}")
                continue
            for row in run_carry(mp_query, log):
                series[sym][row["interval_ts_ns"]] = row
    return series


def hourly_returns(series: dict[str, dict[int, dict]]) -> dict[str, dict[int, float]]:
    """symbol -> {hour_start_ns: mark-to-mark hourly return}."""
    out: dict[str, dict[int, float]] = {}
    for sym, rows in series.items():
        ts = sorted(rows)
        rets: dict[int, float] = {}
        for a, b in zip(ts, ts[1:]):
            if b - a == HOUR_NS:  # contiguous hours only (no gap padding)
                ma, mb = rows[a]["mark"], rows[b]["mark"]
                if ma > 0.0 and mb > 0.0:
                    rets[b] = mb / ma - 1.0
        out[sym] = rets
    return out


def excess_rets(
    btc_rets: dict[int, float], eth_rets: dict[int, float]
) -> tuple[dict[int, float], dict[int, float]]:
    """Cross-asset excess returns: BTC−ETH for BTC events, ETH−BTC for ETH."""
    hours = sorted(set(btc_rets) & set(eth_rets))
    btc_ex = {h: btc_rets[h] - eth_rets[h] for h in hours}
    eth_ex = {h: eth_rets[h] - btc_rets[h] for h in hours}
    return btc_ex, eth_ex


def is_weekend_hour(ts_ns: int) -> bool:
    dt = datetime.fromtimestamp(ts_ns / 1_000_000_000, tz=timezone.utc)
    return dt.weekday() >= 5  # Sat, Sun


def _utc_day(ts_ns: int) -> datetime.date:
    """UTC calendar date of a timestamp (deterministic — no wall clock)."""
    return datetime.fromtimestamp(ts_ns / 1_000_000_000, tz=timezone.utc).date()


def _window_within_utc_day(ts_ns: int, bar_ns: int, pre: int, post: int) -> bool:
    """True when every bar offset in ``[-pre, +post]`` around ``ts_ns`` starts
    on the same UTC calendar day as the event.

    The corpus is per-UTC-day sessions (one log per day, 00:00-23:00 buckets),
    so an event whose window crosses a day boundary would mix sessions — the
    pre-window baseline could come from the previous day and the +post window
    would contain the next day's data. Such events are excluded, never counted
    with a cross-day window (E3).
    """
    day = _utc_day(ts_ns)
    return _utc_day(ts_ns - pre * bar_ns) == day == _utc_day(ts_ns + post * bar_ns)


# --------------------------------------------------------------------------
# Study A — oi-purge-continuation
# --------------------------------------------------------------------------
def study_oi_purge(
    series: dict[str, dict[int, dict]],
    btc_ex: dict[int, float],
    eth_ex: dict[int, float],
    seed: int,
) -> dict:
    """Quadrant-4 OI purges (OI down + price down = longs flushed) then CAR of
    forward excess returns. Event = hour where OI fell >= `thresh` vs the
    prior hour AND mark fell (the purge leg). Excess = cross-asset
    (market-neutral). Reported across 1/2/3% thresholds so a knife-edge
    threshold cannot masquerade as a verdict.

    Session-purity boundary (E3): the corpus is intraday-only (per-UTC-day
    sessions), so an event is counted ONLY when the full [-6h, +24h] window
    lies within a single UTC calendar day — never a previous-day OI baseline
    and never a +24h window containing the next day's data. A 31-bar window
    cannot fit inside a 24h session, so every candidate event is excluded by
    construction and the study honestly reports NOT TESTABLE (n=0) with the
    reason; this is the documented choice (the series is NOT treated as
    crossing days). `n_events_raw` keeps the pre-gate detections for
    transparency.
    """
    ex = {"BTC": btc_ex, "ETH": eth_ex}
    results: dict[str, dict] = {}
    for thresh in (0.01, 0.02, 0.03):
        per_sym: dict[str, dict] = {}
        for sym in ("BTC", "ETH"):
            events: list[Event] = []
            raw_detected = 0
            rows = series[sym]
            ts = sorted(rows)
            for a, b in zip(ts, ts[1:]):
                if b - a != HOUR_NS:
                    continue
                oi_a, oi_b = rows[a]["total_oi"], rows[b]["total_oi"]
                ma, mb = rows[a]["mark"], rows[b]["mark"]
                if oi_a <= 0.0 or ma <= 0.0:
                    continue
                oi_chg = oi_b / oi_a - 1.0
                price_chg = mb / ma - 1.0
                # Quadrant-4: OI down >= thresh AND price down (longs flushed).
                if oi_chg <= -thresh and price_chg < 0.0:
                    raw_detected += 1
                    # Boundary gate (E3): the OI baseline pair a->b must be
                    # same-session too (a midnight event's 'before' bucket
                    # belongs to the previous day), and the full pre+post
                    # window must fit one UTC calendar day.
                    if _utc_day(a) == _utc_day(b) and _window_within_utc_day(
                        b, HOUR_NS, pre=6, post=24
                    ):
                        events.append(Event(b))
            rec = run_study(
                f"oi-purge-continuation-{sym}-{int(thresh * 100)}pct",
                events,
                ex[sym],
                HOUR_NS,
                pre=6,
                post=24,
                seed=seed,
            )
            per_sym[sym] = {
                "n_events_raw": raw_detected,
                **{
                    k: rec.__dict__[k]
                    for k in (
                        "name",
                        "n_events",
                        "pre",
                        "post",
                        "seed",
                        "ci_lo",
                        "ci_hi",
                        "n_days",
                        "ci_reliable",
                    )
                },
                "car_terminal": rec.car[-1] if rec.car else 0.0,
            }
        results[f"oi_drop_{int(thresh * 100)}pct"] = per_sym
    results["reason"] = (
        "no OI-purge hour's full [-6h,+24h] window lies within a single UTC "
        "calendar day — the corpus is per-day sessions (intraday-only), and a "
        "31-bar window cannot fit inside a 24h session without mixing days; "
        "NOT TESTABLE at post=24 by construction (E3 boundary fix)"
    )
    return results


# --------------------------------------------------------------------------
# Study B — listing-flow-v1
# --------------------------------------------------------------------------
def study_listing() -> dict:
    """Listing-flow verdict: no Listing event exists in the recorded corpus —
    no collector subscribes a listing feed (spec 002/031), so the honest
    result is NOT TESTABLE, not a silent pass. The reason IS the whole
    function: there is no corpus iteration to perform over events that cannot
    exist (E6 — no dead loop over an always-empty Listing set)."""
    return {
        "n_events": 0,
        "reason": "no collector subscribes a listing feed; "
        "corpus has no Listing events by construction (spec 002/031)",
    }


# --------------------------------------------------------------------------
# Study C — weekend-liquidity-v1
# --------------------------------------------------------------------------
def study_weekend(
    btc_ex: dict[int, float],
    eth_ex: dict[int, float],
    series: dict[str, dict[int, dict]],
    seed: int,
) -> dict:
    """Weekend vs weekday tape: realized |excess| vol per hour and mean OI,
    plus mark-vs-index basis (the direct thin-liquidity proxy — a wide basis
    is the venue's mark diverging from fair value). The idea is a *filter*
    candidate (weekend risk-off), so we grade whether weekend hours are
    measurably thinner — vol, OI, basis — than weekday hours."""
    hours = sorted(set(btc_ex) | set(eth_ex))
    events = [Event(h, "weekend" if is_weekend_hour(h) else "weekday") for h in hours]
    # Vol proxy series: |BTC−ETH| excess per hour (thinner tape = noisier).
    abs_ex = {h: abs(v) for h, v in btc_ex.items()}
    by_regime = car_by_regime(events, abs_ex, HOUR_NS, pre=0, post=23)
    oi_by_regime: dict[str, list[float]] = {"weekend": [], "weekday": []}
    basis_by_regime: dict[str, list[float]] = {"weekend": [], "weekday": []}
    for h in hours:
        key = "weekend" if is_weekend_hour(h) else "weekday"
        for sym in ("BTC", "ETH"):
            if h in series[sym]:
                if series[sym][h]["total_oi"] > 0.0:
                    oi_by_regime[key].append(series[sym][h]["total_oi"])
                basis_by_regime[key].append(abs(series[sym][h]["basis_bps"]))
    lo, hi = bootstrap_ci(events, abs_ex, HOUR_NS, 0, 23, seed=seed)
    n_days = len({_utc_day(e.ts_ns) for e in events})
    return {
        "n_weekend_hours": sum(1 for e in events if e.regime == "weekend"),
        "n_weekday_hours": sum(1 for e in events if e.regime == "weekday"),
        "n_days": n_days,
        "ci_reliable": n_days >= 3,
        "car_24h_weekend": by_regime.get("weekend", [0.0])[-1],
        "car_24h_weekday": by_regime.get("weekday", [0.0])[-1],
        "ci95_terminal": [lo, hi],
        "mean_oi_weekend": sum(oi_by_regime["weekend"])
        / max(1, len(oi_by_regime["weekend"])),
        "mean_oi_weekday": sum(oi_by_regime["weekday"])
        / max(1, len(oi_by_regime["weekday"])),
        "mean_abs_basis_bps_weekend": sum(basis_by_regime["weekend"])
        / max(1, len(basis_by_regime["weekend"])),
        "mean_abs_basis_bps_weekday": sum(basis_by_regime["weekday"])
        / max(1, len(basis_by_regime["weekday"])),
    }


# --------------------------------------------------------------------------
# Study D — funding-arb-v1 (cross-venue funding spread)
# --------------------------------------------------------------------------
def annualized_funding_bps(rows: list[dict], venue: str) -> dict[int, float]:
    """hour_start_ns -> funding rate annualized to bps/yr (cadence-aware)."""
    periods = FUNDING_PERIODS_PER_YEAR.get(venue)
    if periods is None:
        raise ValueError(f"funding cadence unknown for venue {venue!r}")
    out: dict[int, float] = {}
    for r in rows:
        rate = r["funding_rate"]
        if math.isfinite(rate):
            out[r["interval_ts_ns"]] = rate * periods * 1e4
    return out


def gap_change_series(gap: dict[int, float]) -> dict[int, float]:
    """hourly change in |gap| (convergence = negative). Value at hour ``b`` is
    |gap|(b) − |gap|(a) for contiguous hours a→b."""
    ts = sorted(gap)
    out: dict[int, float] = {}
    for a, b in zip(ts, ts[1:]):
        if b - a == HOUR_NS:
            out[b] = abs(gap[b]) - abs(gap[a])
    return out


def study_funding_arb(mp_query: Path, data_raw: Path, seed: int) -> dict:
    """funding-arb-v1: per overlap day, the cross-venue annualized funding
    spread on the same underlying. Event = hour where |spread| >= threshold;
    series = hourly change in |spread| (bps/yr per hour). Negative CAR[+24h]
    = the spread mean-reverts (the carry window closes fast); ~0 = the spread
    persists (harvestable carry); positive = divergence risk."""
    results: dict[str, dict] = {}
    for v_a, sym_a, v_b, sym_b, day in CROSS_VENUE_PAIRS:
        log_a = data_raw / f"{day}_{v_a}_{sym_a}.log"
        log_b = data_raw / f"{day}_{v_b}_{sym_b}.log"
        key = f"{v_a}-{sym_a}-vs-{v_b}-{sym_b}-{day}"
        if not log_a.exists() or not log_b.exists():
            results[key] = {
                "n_overlap_hours": 0,
                "reason": f"missing leg: {log_a.name} / {log_b.name}",
            }
            continue
        fund_a = annualized_funding_bps(run_carry(mp_query, log_a), v_a)
        fund_b = annualized_funding_bps(run_carry(mp_query, log_b), v_b)
        hours = sorted(set(fund_a) & set(fund_b))
        spread = {h: fund_a[h] - fund_b[h] for h in hours}
        change = gap_change_series(spread)
        rec: dict[str, dict] = {
            "n_overlap_hours": len(hours),
            "mean_spread_bps_yr": sum(spread.values()) / max(1, len(spread)),
            "mean_abs_spread_bps_yr": sum(abs(v) for v in spread.values())
            / max(1, len(spread)),
        }
        # post=12 is the longest horizon the single-day overlaps (15-17h) can
        # fully support; post=24 is the standard read and honestly reports
        # n=0 when the overlap cannot fill a 25-bar window.
        for post in (12, 24):
            for thresh in SPREAD_THRESH_BPS:
                events = [Event(h) for h in hours if abs(spread[h]) >= thresh]
                study = run_study(
                    f"funding-arb-{key}-{int(thresh)}bpsyr-p{post}h",
                    events,
                    change,
                    HOUR_NS,
                    pre=0,
                    post=post,
                    seed=seed,
                )
                rec[f"spread_ge_{int(thresh)}bpsyr_p{post}h"] = {
                    "n_events": study.n_events,
                    "n_events_raw": len(events),
                    "n_days": study.n_days,
                    "ci_reliable": study.ci_reliable,
                    "car_terminal": study.car[-1] if study.car else 0.0,
                    "ci_lo": study.ci_lo,
                    "ci_hi": study.ci_hi,
                    # Mean |spread| over the +post window across complete-
                    # window events — the carry actually collected if the
                    # spread persists.
                    "mean_abs_spread_after_bps_yr": _mean_gap_after(
                        spread, events, post
                    ),
                }
        results[key] = rec
    return results


def _mean_gap_after(gap: dict[int, float], events: list[Event], post: int) -> float:
    """Mean |gap| over offsets 0..+post across events with complete windows."""
    vals: list[float] = []
    for e in events:
        window = [gap.get(e.ts_ns + k * HOUR_NS) for k in range(post + 1)]
        if any(v is None for v in window):
            continue
        vals.append(sum(abs(v) for v in window) / (post + 1))
    return sum(vals) / len(vals) if vals else 0.0


# --------------------------------------------------------------------------
# Study E — basis-carry-v1 (perp-vs-oracle basis dynamics)
# --------------------------------------------------------------------------
def study_basis_carry(series: dict[str, dict[int, dict]], seed: int) -> dict:
    """basis-carry-v1: the dated-future leg has no corpus data (NOT TESTABLE
    by construction — no OKX/Binance quarterly collector); the testable
    proxy is the perp-vs-oracle basis (mark vs the venue's own index). Event
    = hour where |basis| >= threshold; series = hourly change in |basis|
    (bps/hr). Negative CAR[+24h] = wide basis mean-reverts (tight oracle
    tracking); ~0 = persistent divergence."""
    per_sym: dict[str, dict] = {}
    for sym in ("BTC", "ETH"):
        basis: dict[int, float] = {}
        for ts, row in series[sym].items():
            b = row.get("basis_bps")
            if isinstance(b, (int, float)) and math.isfinite(b):
                basis[ts] = float(b)
        change = gap_change_series(basis)
        rec: dict[str, dict] = {
            "n_hours": len(basis),
            "mean_basis_bps": sum(basis.values()) / max(1, len(basis)),
            "mean_abs_basis_bps": sum(abs(v) for v in basis.values())
            / max(1, len(basis)),
        }
        for thresh in BASIS_THRESH_BPS:
            events = [Event(h) for h in basis if abs(basis[h]) >= thresh]
            study = run_study(
                f"basis-carry-{sym}-{int(thresh)}bps",
                events,
                change,
                HOUR_NS,
                pre=0,
                post=24,
                seed=seed,
            )
            rec[f"abs_basis_ge_{int(thresh)}bps"] = {
                "n_events": study.n_events,
                "n_events_raw": len(events),
                "n_days": study.n_days,
                "ci_reliable": study.ci_reliable,
                "car_terminal": study.car[-1] if study.car else 0.0,
                "ci_lo": study.ci_lo,
                "ci_hi": study.ci_hi,
                "mean_abs_basis_after24h_bps": _mean_gap_after(basis, events, 24),
            }
        per_sym[sym] = rec
    return {
        "dated_future_leg": {
            "n_events": 0,
            "reason": "no dated-future (OKX/Binance quarterly) collector; "
            "corpus records perps only, so the tradable dated-vs-perp basis "
            "is NOT TESTABLE by construction (spec 002)",
        },
        "perp_oracle_proxy": per_sym,
    }


def _existing_run_ids(runs_dir: Path) -> set[str]:
    """run_ids already journaled in ``<runs_dir>/index.jsonl`` (SIM-10
    tracker). Unparseable lines carry no identity and are skipped."""
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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="backlog idea event studies (RES-4)")
    parser.add_argument(
        "--mp-query", type=Path, default=Path("target/release/mp-query.exe")
    )
    parser.add_argument("--data-raw", type=Path, default=Path("data/raw"))
    parser.add_argument("--runs-dir", type=Path, default=Path("runs"))
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument(
        "--run-id",
        default="backlog-event-studies-2026-08-16",
        help=(
            "journal record id (re-gates pass a distinct id, e.g. -r2). "
            "MUST be new: a run-id already present in runs/index.jsonl is "
            "refused (append-only tracker — a duplicate corrupts the "
            "'have we tried this?' identity)"
        ),
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help=(
            "acknowledge the append-only tracker: a duplicate --run-id is "
            "STILL refused (overwrite is never allowed); pass a NEW run id"
        ),
    )
    args = parser.parse_args(argv)

    # E7/F8: fail fast on a duplicate run-id — the tracker is append-only
    # (SIM-10, W-6), so a re-run must use a fresh id, never append a second
    # record under the same identity. --force does not permit overwrite.
    if args.run_id in _existing_run_ids(args.runs_dir):
        hint = f"pass a fresh run id (e.g. ULID-style, or '{args.run_id}-r2')"
        if args.force:
            print(
                f"error: run-id '{args.run_id}' already exists in "
                f"{args.runs_dir / 'index.jsonl'} — append-only tracker: "
                f"--force does not permit overwrite; {hint}",
                file=sys.stderr,
            )
        else:
            print(
                f"error: run-id '{args.run_id}' already exists in "
                f"{args.runs_dir / 'index.jsonl'} — refusing to append a "
                f"duplicate record; {hint}",
                file=sys.stderr,
            )
        return 2

    print("loading hourly carry series (mark/OI/funding) from raw logs ...")
    series = load_series(args.mp_query, args.data_raw)
    rets = hourly_returns(series)
    btc_rets, eth_rets = rets["BTC"], rets["ETH"]
    btc_ex, eth_ex = excess_rets(btc_rets, eth_rets)
    print(
        f"  BTC hours={len(btc_rets)} ETH hours={len(eth_rets)} "
        f"overlapping={len(set(btc_rets) & set(eth_rets))}"
    )

    results: dict[str, dict] = {
        "oi_purge_continuation": study_oi_purge(series, btc_ex, eth_ex, args.seed),
        "listing_flow": study_listing(),
        "weekend_liquidity": study_weekend(btc_ex, eth_ex, series, args.seed),
        "funding_arb": study_funding_arb(args.mp_query, args.data_raw, args.seed),
        "basis_carry": study_basis_carry(series, args.seed),
    }

    print(
        "\n=== oi-purge-continuation (CAR of BTC-ETH excess, purge = OI down + price down) ==="
    )
    for thresh_key, per_sym in results["oi_purge_continuation"].items():
        if thresh_key == "reason":
            print(f"  {per_sym}")
            continue
        for sym, r in per_sym.items():
            print(
                f"  {thresh_key} {sym}: n={r['n_events']} (raw {r['n_events_raw']}) "
                f"n_days={r['n_days']} CAR[+24h]={r['car_terminal']:+.5f} "
                f"CI95=[{r['ci_lo']:+.5f},{r['ci_hi']:+.5f}]"
            )
            if not r["ci_reliable"]:
                print(
                    "    CI from <3 distinct days — block-bootstrap CI is "
                    "unreliable at this corpus breadth"
                )
    print("=== listing-flow-v1 ===")
    print(f"  {results['listing_flow']}")
    print("=== weekend-liquidity-v1 (|excess| vol + OI by regime) ===")
    w = results["weekend_liquidity"]
    print(f"  weekend hours={w['n_weekend_hours']} weekday={w['n_weekday_hours']}")
    if not w["ci_reliable"]:
        print(
            "  CI from <3 distinct days — block-bootstrap CI is unreliable "
            "at this corpus breadth"
        )
    print(
        f"  cum |excess| 24h: weekend={w['car_24h_weekend']:+.5f} weekday={w['car_24h_weekday']:+.5f}"
    )
    print(
        f"  mean OI: weekend={w['mean_oi_weekend']:.0f} weekday={w['mean_oi_weekday']:.0f}"
    )
    print(
        f"  mean |basis| bps: weekend={w['mean_abs_basis_bps_weekend']:.3f} "
        f"weekday={w['mean_abs_basis_bps_weekday']:.3f}"
    )
    print("=== funding-arb-v1 (cross-venue annualized funding spread, bps/yr) ===")
    for key, r in results["funding_arb"].items():
        if "reason" in r:
            print(f"  {key}: NOT TESTABLE — {r['reason']}")
            continue
        print(
            f"  {key}: overlap_hours={r['n_overlap_hours']} "
            f"mean spread={r['mean_spread_bps_yr']:+.0f} "
            f"mean |spread|={r['mean_abs_spread_bps_yr']:.0f}"
        )
        for k, s in r.items():
            if not k.startswith("spread_ge"):
                continue
            print(
                f"    {k}: n={s['n_events']} (raw {s['n_events_raw']}) "
                f"n_days={s['n_days']} "
                f"d|spread|/h CAR[+{k.rsplit('p', 1)[1]}]={s['car_terminal']:+.1f} "
                f"CI95=[{s['ci_lo']:+.1f},{s['ci_hi']:+.1f}] "
                f"mean |spread| after={s['mean_abs_spread_after_bps_yr']:.0f}"
            )
            if not s["ci_reliable"]:
                print(
                    "    CI from <3 distinct days — block-bootstrap CI is "
                    "unreliable at this corpus breadth"
                )
    print("=== basis-carry-v1 (dated-future leg + perp-vs-oracle proxy) ===")
    bc = results["basis_carry"]
    print(f"  dated-future leg: {bc['dated_future_leg']}")
    for sym, r in bc["perp_oracle_proxy"].items():
        print(
            f"  {sym}: n_hours={r['n_hours']} mean basis={r['mean_basis_bps']:+.2f} "
            f"mean |basis|={r['mean_abs_basis_bps']:.2f} bps"
        )
        for k, s in r.items():
            if not k.startswith("abs_basis_ge"):
                continue
            print(
                f"    {k}: n={s['n_events']} (raw {s['n_events_raw']}) "
                f"n_days={s['n_days']} "
                f"d|basis| CAR[+24h]={s['car_terminal']:+.3f} bps "
                f"CI95=[{s['ci_lo']:+.3f},{s['ci_hi']:+.3f}] "
                f"mean |basis| after 24h={s['mean_abs_basis_after24h_bps']:.2f}"
            )
            if not s["ci_reliable"]:
                print(
                    "    CI from <3 distinct days — block-bootstrap CI is "
                    "unreliable at this corpus breadth"
                )

    # Journal (SIM-10 pattern) — append-only, W-6.
    args.runs_dir.mkdir(parents=True, exist_ok=True)
    corpus_days = sorted(
        set(DAYS)
        | {
            p[4]
            for p in CROSS_VENUE_PAIRS
            if (args.data_raw / f"{p[4]}_{p[0]}_{p[1]}.log").exists()
        }
    )
    record = {
        "run_id": args.run_id,
        "kind": "res4_event_study",
        "seed": args.seed,
        "corpus_days": corpus_days,
        "results": results,
    }
    with (args.runs_dir / "index.jsonl").open("a", encoding="utf-8") as f:
        f.write(json.dumps(record, sort_keys=True) + "\n")
    print(f"\njournaled -> {args.runs_dir / 'index.jsonl'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
