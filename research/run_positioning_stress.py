"""Positioning-stress confluence study (RES-4 gate).

Grades the confluence hypothesis behind the positioning-stress idea: when
funding stress AND open-interest purge AND one-sided liquidation pressure
align in the SAME hour, does the asset's excess return behave differently
over the next hours? Cascade-fade research found per-asset behavior (SOL
fades strong, ETH fragile, BTC dead — fills dominate once cascades start,
arXiv 2608.03616), and funding standalone mean-reversion was NOT validated —
only confluence (funding percentile x delta-OI x liq imbalance) is worth
grading.

Data source (same pattern as ``run_backlog_event_studies.py``):
- ``mp-query carry --logs <hl.log> --interval-secs 3600 --json`` — hourly
  mark / OI / funding from raw hyperliquid logs (08-14..08-17).
- ``mp-query liq --logs <bybit.log> --interval-secs 3600 --json`` — hourly
  signed liquidation notional from bybit logs (the corpus's only native
  liquidation source, spec 029 COL-29). Intervals line up on
  ``interval_ts_ns`` for a cross-leg join.

Event = hour where ALL THREE legs stress on the same symbol:
  funding: annualized |funding| >= threshold (HL funds hourly, x8760)
  OI:      |delta-OI hour-over-hour| >= threshold (purge or build)
  liq:     |signed liquidation notional| >= threshold (one-sided flush)
Episode tags: "long_flush" (funding>0, OI down, sell-dominant liq — longs
get liquidated) vs "short_squeeze" (funding<0, OI down, buy-dominant liq).

Series = cross-asset hourly excess returns (BTC-ETH for BTC events, ETH-BTC
for ETH events) so a market-wide move never looks like an edge. Forward CAR
at +1/+3/+6h with seeded bootstrap CI (CONV-11).

Honest verdicts journaled to ``runs/index.jsonl`` (SIM-10 pattern): an idea
with no confluence events in the corpus is NOT TESTABLE (n=0 + reason), never
a silent pass. Every row discloses ``n_days`` / ``ci_reliable`` (<3 distinct
UTC days = unreliable block-bootstrap, E2). Windows stay within one UTC
calendar day (E3 — the corpus is intraday sessions; pre=1/post=6 needs 8
bars, which fits). ``--run-id`` duplicates are refused (append-only, E7).

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

from event_study import Event, run_study  # noqa: E402

HOUR_NS = 3_600_000_000_000
HL_PERIODS_PER_YEAR = 8_760  # hyperliquid funds hourly

# Corpus: the days with BOTH hyperliquid carry and bybit liquidation logs.
# (bybit liq logs exist 08-14..08-17; HL carry for BTC/ETH exists 08-08..17.)
DAYS = [f"202608{d:02d}" for d in range(14, 18)]

# Per-symbol stress thresholds (see module docstring). The corpus funding
# range on the overlap days is ~[-1350, +1095] bps/yr (no true stress
# episode), so 800/1200 are the CORPUS-TAIL bands (honest reference: funding
# percentiles are measured against the recorded corpus, never a synthetic
# 30d window) and 2000/3000 are the absolute-stress bands that report
# NOT TESTABLE (n=0 + reason) when no such hour exists.
FUNDING_THRESH_BPS_YR = (800.0, 1200.0, 2000.0, 3000.0)
OI_CHG_THRESH = (0.005, 0.01)  # |delta-OI| hour-over-hour
LIQ_THRESH_USD = {"BTCUSDT": 250_000.0, "ETHUSDT": 100_000.0}
# Highest |funding| observed across the corpus overlap days (for the
# NOT TESTABLE reason text — measured, never assumed).
CORPUS_MAX_ABS_FUNDING_BPS_YR = 1352.0


def run_mp_query(mp_query: Path, sub: str, logs: list[Path]) -> list[dict]:
    out = subprocess.run(
        [
            str(mp_query),
            sub,
            "--interval-secs",
            "3600",
            "--json",
            *[a for log in logs for a in ("--logs", str(log))],
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if out.returncode != 0:
        raise RuntimeError(f"mp-query {sub} failed: {out.stderr[:300]}")
    return json.loads(out.stdout)


def annualized_funding_bps(rate: float) -> float:
    """HL hourly funding rate -> bps/yr (cadence-aware; units never mixed)."""
    return rate * HL_PERIODS_PER_YEAR * 1e4


def _utc_day(ts_ns: int) -> datetime.date:
    """UTC calendar date of a timestamp (deterministic — no wall clock)."""
    return datetime.fromtimestamp(ts_ns / 1_000_000_000, tz=timezone.utc).date()


def _window_within_utc_day(ts_ns: int, bar_ns: int, pre: int, post: int) -> bool:
    """Every bar offset in ``[-pre, +post]`` starts on the same UTC day as the
    event — the corpus is per-UTC-day sessions, so crossing a day boundary
    would mix sessions (E3; same rule as the backlog studies)."""
    day = _utc_day(ts_ns)
    return _utc_day(ts_ns - pre * bar_ns) == day == _utc_day(ts_ns + post * bar_ns)


def load_carry(mp_query: Path, data_raw: Path) -> dict[str, dict[int, dict]]:
    """symbol -> {hour_start_ns: carry row} across corpus days (HL only)."""
    series: dict[str, dict[int, dict]] = {"BTC": {}, "ETH": {}}
    for day in DAYS:
        for sym in ("BTC", "ETH"):
            log = data_raw / f"{day}_hyperliquid_{sym}.log"
            if not log.exists():
                print(f"  skip (missing): {log.name}")
                continue
            for row in run_mp_query(mp_query, "carry", [log]):
                series[sym][row["interval_ts_ns"]] = row
    return series


def load_liq(mp_query: Path, data_raw: Path) -> dict[str, dict[int, float]]:
    """bybit symbol -> {hour_start_ns: signed net liquidation notional}."""
    out: dict[str, dict[int, float]] = {}
    for day in DAYS:
        for sym in ("BTCUSDT", "ETHUSDT"):
            log = data_raw / f"{day}_bybit_{sym}.log"
            if not log.exists():
                print(f"  skip (missing): {log.name}")
                continue
            for row in run_mp_query(mp_query, "liq", [log]):
                out.setdefault(sym, {})[row["interval_ts_ns"]] = row["net_notional"]
    return out


def hourly_returns(carry: dict[str, dict[int, dict]]) -> dict[str, dict[int, float]]:
    """symbol -> {hour_start_ns: mark-to-mark hourly return} (contiguous hours)."""
    out: dict[str, dict[int, float]] = {}
    for sym, rows in carry.items():
        ts = sorted(rows)
        rets: dict[int, float] = {}
        for a, b in zip(ts, ts[1:]):
            if b - a == HOUR_NS:
                ma, mb = rows[a]["mark"], rows[b]["mark"]
                if ma > 0.0 and mb > 0.0:
                    rets[b] = mb / ma - 1.0
        out[sym] = rets
    return out


def excess_rets(
    btc_rets: dict[int, float], eth_rets: dict[int, float]
) -> tuple[dict[int, float], dict[int, float]]:
    """Cross-asset excess: BTC-ETH for BTC events, ETH-BTC for ETH events."""
    hours = sorted(set(btc_rets) & set(eth_rets))
    btc_ex = {h: btc_rets[h] - eth_rets[h] for h in hours}
    eth_ex = {h: eth_rets[h] - btc_rets[h] for h in hours}
    return btc_ex, eth_ex


def oi_change_at(rows: dict[int, dict], ts_ns: int) -> float | None:
    """Hour-over-hour total_oi relative change ending at ``ts_ns``; None when
    the previous contiguous hour is missing or OI is non-positive."""
    if ts_ns - HOUR_NS not in rows:
        return None
    prev, cur = rows[ts_ns - HOUR_NS]["total_oi"], rows[ts_ns]["total_oi"]
    if prev <= 0.0:
        return None
    return cur / prev - 1.0


def study_positioning_stress(
    carry: dict[str, dict[int, dict]],
    liq: dict[str, dict[int, float]],
    ex: dict[str, dict[int, float]],
    seed: int,
) -> dict:
    """Confluence stress hours -> forward CAR of cross-asset excess returns.

    Event = same-hour funding stress AND OI purge AND one-sided liquidation
    pressure; regime tags distinguish long-flush vs short-squeeze episodes.
    Reported across threshold combinations so a knife-edge threshold cannot
    masquerade as a verdict.
    """
    results: dict[str, dict] = {}
    for funding_thresh in FUNDING_THRESH_BPS_YR:
        for oi_thresh in OI_CHG_THRESH:
            key = f"fund_ge_{int(funding_thresh)}bpsyr_oi_ge_{int(oi_thresh * 100)}pct"
            per_sym: dict[str, dict] = {}
            for sym, hl_sym in (("BTCUSDT", "BTC"), ("ETHUSDT", "ETH")):
                rows = carry[hl_sym]
                liq_rows = liq.get(sym, {})
                events: list[Event] = []
                raw_candidates = 0
                for ts_ns in sorted(rows):
                    f = rows[ts_ns].get("funding_rate")
                    if not isinstance(f, (int, float)) or not math.isfinite(f):
                        continue
                    fund_bps = annualized_funding_bps(float(f))
                    oi_chg = oi_change_at(rows, ts_ns)
                    liq_net = liq_rows.get(ts_ns)
                    if abs(fund_bps) < funding_thresh or oi_chg is None:
                        continue
                    if abs(oi_chg) < oi_thresh or liq_net is None:
                        continue
                    if abs(liq_net) < LIQ_THRESH_USD[sym]:
                        continue
                    raw_candidates += 1
                    # Episode tag: long-flush = longs pay + get dumped;
                    # short-squeeze = shorts pay + get bought back.
                    if f > 0.0 and oi_chg < 0.0 and liq_net < 0.0:
                        regime = "long_flush"
                    elif f < 0.0 and oi_chg < 0.0 and liq_net > 0.0:
                        regime = "short_squeeze"
                    else:
                        regime = "mixed"
                    # E3: the full [-1h, +6h] window must fit one UTC day.
                    if _window_within_utc_day(ts_ns, HOUR_NS, pre=1, post=6):
                        events.append(Event(ts_ns, regime))
                study = run_study(
                    f"positioning-stress-{sym}-{key}",
                    events,
                    ex[hl_sym],
                    HOUR_NS,
                    pre=1,
                    post=6,
                    seed=seed,
                )
                rec: dict = {
                    "n_candidates_raw": raw_candidates,
                    "n_events": study.n_events,
                    "n_days": study.n_days,
                    "ci_reliable": study.ci_reliable,
                    "car_1h": study.car[2] if study.car else 0.0,
                    "car_3h": study.car[4] if study.car else 0.0,
                    "car_6h": study.car[-1] if study.car else 0.0,
                    "ci_lo": study.ci_lo,
                    "ci_hi": study.ci_hi,
                    "regimes": {
                        r: sum(1 for e in events if e.regime == r)
                        for r in ("long_flush", "short_squeeze", "mixed")
                    },
                }
                if raw_candidates == 0 and funding_thresh >= 2000.0:
                    rec["reason"] = (
                        "no hour in the corpus overlap days (08-14..08-17) has "
                        f"annualized |funding| >= {int(funding_thresh)} bps/yr; "
                        f"the corpus funding range is "
                        f"[-{CORPUS_MAX_ABS_FUNDING_BPS_YR:.0f}, "
                        f"+{CORPUS_MAX_ABS_FUNDING_BPS_YR:.0f}] bps/yr — no true "
                        "funding-stress episode occurred in the window, so the "
                        "absolute-stress confluence is NOT TESTABLE on this "
                        "corpus (the 800/1200 bands grade the corpus-tail)"
                    )
                per_sym[sym] = rec
            results[key] = per_sym
    return results


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="positioning-stress confluence event study (RES-4)"
    )
    parser.add_argument(
        "--mp-query", type=Path, default=Path("target/release/mp-query.exe")
    )
    parser.add_argument("--data-raw", type=Path, default=Path("data/raw"))
    parser.add_argument("--runs-dir", type=Path, default=Path("runs"))
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument(
        "--run-id",
        default="positioning-stress-confluence-2026-08-18",
        help=(
            "journal record id (re-gates pass a distinct id). MUST be new: a "
            "run-id already present in runs/index.jsonl is refused (append-"
            "only tracker — a duplicate corrupts the 'have we tried this?' "
            "identity, E7)"
        ),
    )
    args = parser.parse_args(argv)

    runs_dir = args.runs_dir
    index = runs_dir / "index.jsonl"
    if index.exists():
        existing = {
            rec.get("run_id")
            for rec in (
                json.loads(line)
                for line in index.read_text(encoding="utf-8").splitlines()
                if line.strip()
            )
            if isinstance(rec, dict) and isinstance(rec.get("run_id"), str)
        }
        if args.run_id in existing:
            print(
                f"error: run-id '{args.run_id}' already exists in {index} — "
                "refusing to append a duplicate record; pass a fresh run id "
                "(e.g. ULID-style)",
                file=sys.stderr,
            )
            return 2

    print("loading hourly carry (mark/OI/funding) from hyperliquid logs ...")
    carry = load_carry(args.mp_query, args.data_raw)
    print("loading hourly liquidation notional from bybit logs ...")
    liq = load_liq(args.mp_query, args.data_raw)
    rets = hourly_returns(carry)
    btc_ex, eth_ex = excess_rets(rets["BTC"], rets["ETH"])
    print(
        f"  BTC hours={len(rets['BTC'])} ETH hours={len(rets['ETH'])} "
        f"overlapping={len(set(rets['BTC']) & set(rets['ETH']))} "
        f"bybit-liq BTC hours={len(liq.get('BTCUSDT', {}))} "
        f"ETH hours={len(liq.get('ETHUSDT', {}))}"
    )

    results = study_positioning_stress(
        carry, liq, {"BTC": btc_ex, "ETH": eth_ex}, args.seed
    )

    print("\n=== positioning-stress confluence (CAR of cross-asset excess) ===")
    for key, per_sym in results.items():
        print(f"  [{key}]")
        for sym, r in per_sym.items():
            print(
                f"    {sym}: n={r['n_events']} (candidates {r['n_candidates_raw']}) "
                f"n_days={r['n_days']} CAR=[{r['car_1h']:+.5f},"
                f"{r['car_3h']:+.5f},{r['car_6h']:+.5f}] "
                f"CI95(6h)=[{r['ci_lo']:+.5f},{r['ci_hi']:+.5f}] "
                f"regimes={r['regimes']}"
            )
            if "reason" in r:
                print(f"      NOT TESTABLE — {r['reason']}")
            elif not r["ci_reliable"]:
                print(
                    "      CI from <3 distinct days — block-bootstrap CI is "
                    "unreliable at this corpus breadth"
                )

    runs_dir.mkdir(parents=True, exist_ok=True)
    record = {
        "run_id": args.run_id,
        "kind": "res4_event_study",
        "seed": args.seed,
        "corpus_days": DAYS,
        "results": results,
    }
    with (runs_dir / "index.jsonl").open("a", encoding="utf-8") as f:
        f.write(json.dumps(record, sort_keys=True) + "\n")
    print(f"\njournaled -> {runs_dir / 'index.jsonl'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
