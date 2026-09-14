"""FPX-1..3 event-study gate for footprint-exhaustion-v1.

This is intentionally an event study only: no backtest, no parameter selection,
and no promotion. It follows the hypothesis frozen in
strategies/footprint-exhaustion-v1/hypothesis.md:

* 1h trade-tape bars from mp-query, only INT-4-clean/non-stale-burst days;
* volume percentile {80, 90} x prior-20-bar ATR range ratio {0.4, 0.6};
* mirrored absorption condition (positive delta on a down climax, negative
  delta on an up climax) and two prior bars in the preceding direction;
* signed mark CAR at +1h/+2h/+4h, plus BTC/ETH excess control;
* deterministic percentile bootstrap CI95, seed pinned in the report.

The bar feature implementation is deliberately explicit here so the study is
reproducible without importing private Rust feature state. Volume percentile
matches VolumeBubble: the current bar is included in a trailing 100-bar window
and rank is count(values strictly below current) / 100 * 100. ATR is computed
from the 20 bars immediately before the candidate bar; this prevents the
candidate bar's range from entering its own denominator.

Research-only. The event study cannot adjudicate cost expectancy or WF sign
flips; those are reported as NOT ASSESSED and require the next funnel stage.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import subprocess
import sys
from dataclasses import dataclass, asdict
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Iterable

BAR_NS = 3_600_000_000_000
DAY_NS = 86_400_000_000_000
VOLUME_WINDOW = 100
ATR_WINDOW = 20
HORIZONS = (1, 2, 4)
VOLUME_PCTS = (80.0, 90.0)
RANGE_RATIOS = (0.4, 0.6)
DEFAULT_SEED = 42
DEFAULT_BOOTSTRAPS = 1000
START_DAY = date(2026, 8, 25)
END_DAY = date(2026, 9, 12)

VENUE_SYMBOLS = {
    "bybit": ("BTCUSDT", "ETHUSDT"),
    "hyperliquid": ("BTC", "ETH"),
}

# The existing audit treats stale_stream/coverage_gap as warnings, but FPX-3
# explicitly says no stale-burst/known-dirty days. Keep that stricter study
# boundary here instead of using the more permissive promotion predicate.
AUDIT_WARNING_CODES = {"recv_time_reversal", "stale_stream", "coverage_gap"}


@dataclass(frozen=True)
class Bar:
    ts_ns: int
    open: float
    high: float
    low: float
    close: float
    buy_vol: float
    sell_vol: float
    n_trades: int

    @property
    def volume(self) -> float:
        return self.buy_vol + self.sell_vol

    @property
    def delta(self) -> float:
        return self.buy_vol - self.sell_vol


@dataclass(frozen=True)
class Event:
    venue: str
    symbol: str
    ts_ns: int
    direction: int  # +1 long after a down-climax; -1 short after an up-climax
    volume_pct: float
    range_ratio: float
    week: str
    day: str
    climax_close: float


@dataclass
class ConfigResult:
    volume_pct: float
    range_ratio: float
    n_events_raw: int
    n_events_complete_1h: int
    n_events_complete_2h: int
    n_events_complete_4h: int
    n_days: int
    n_weeks: int
    venues: list[str]
    symbols: list[str]
    car_bps: dict[str, float]
    ci95_bps: dict[str, list[float]]
    excess_car_bps: dict[str, float]
    excess_ci95_bps: dict[str, list[float]]
    raw_positive_horizons: int
    excess_positive_horizons: int
    event_days: list[str]


class StudyError(RuntimeError):
    pass


def utc_day(ts_ns: int) -> date:
    return datetime.fromtimestamp(ts_ns / 1_000_000_000, tz=timezone.utc).date()


def week_label(day: date) -> str:
    iso = day.isocalendar()
    return f"{iso.year}-W{iso.week:02d}"


def day_strings() -> list[str]:
    out: list[str] = []
    d = START_DAY
    while d <= END_DAY:
        out.append(d.strftime("%Y%m%d"))
        d += timedelta(days=1)
    return out


def log_path(data_raw: Path, venue: str, symbol: str, day: str) -> Path:
    return data_raw / f"{day}_{venue}_{symbol}.log"


def run_json_command(argv: list[str], env: dict[str, str], cwd: Path) -> object:
    proc = subprocess.run(
        argv, capture_output=True, text=True, env=env, cwd=str(cwd), check=False
    )
    if proc.returncode != 0:
        raise StudyError(
            f"command failed ({proc.returncode}): {' '.join(argv)}\n"
            f"stderr: {proc.stderr[-1000:]}"
        )
    text = proc.stdout.strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        raise StudyError(
            f"command did not emit JSON: {' '.join(argv)}\n"
            f"stdout: {text[-1000:]}\nstderr: {proc.stderr[-1000:]}"
        ) from exc


def audit_one(mp_ops: Path, data_root: Path, data_raw: Path, venue: str, symbol: str, day: str) -> dict:
    # mp-ops resolves data/ relative to cwd. Pin cwd to the repository so the
    # audit is against this study's raw corpus, not an ambient caller path.
    env = dict(__import__("os").environ)
    env["RUST_LOG"] = "off"
    raw = run_json_command(
        [
            str(mp_ops),
            "audit",
            "--date",
            day,
            "--venue",
            venue,
            "--symbol",
            symbol,
        ],
        env,
        data_root,
    )
    if not isinstance(raw, dict):
        raise StudyError(f"audit returned non-object for {venue}:{symbol}:{day}")
    return raw


def blocking_codes(audit: dict) -> list[str]:
    codes = []
    for finding in audit.get("findings", []):
        code = finding.get("code")
        if isinstance(code, str) and code not in AUDIT_WARNING_CODES:
            codes.append(code)
    return sorted(set(codes))


def is_fpx_clean(audit: dict) -> bool:
    """FPX-3 strict eligibility: INT-4 clean and zero stale bursts."""
    count = int(audit.get("event_count", 0) or 0)
    coverage = float(audit.get("coverage", 0.0) or 0.0)
    stale_bursts = audit.get("stale_bursts") or []
    return count > 0 and coverage >= 0.995 and not stale_bursts and not blocking_codes(audit)


def parse_bars(raw: object, source: str) -> list[Bar]:
    if not isinstance(raw, list):
        raise StudyError(f"bars returned non-list for {source}")
    bars: list[Bar] = []
    for row in raw:
        if not isinstance(row, dict):
            continue
        try:
            b = Bar(
                ts_ns=int(row["interval_ts_ns"]),
                open=float(row["open"]),
                high=float(row["high"]),
                low=float(row["low"]),
                close=float(row["close"]),
                buy_vol=float(row["buy_vol"]),
                sell_vol=float(row["sell_vol"]),
                n_trades=int(row["n_trades"]),
            )
        except (KeyError, TypeError, ValueError) as exc:
            raise StudyError(f"malformed bar for {source}: {row}") from exc
        values = (b.open, b.high, b.low, b.close, b.buy_vol, b.sell_vol)
        if not all(math.isfinite(x) for x in values):
            continue
        if b.high < b.low or b.close <= 0 or b.volume <= 0 or b.n_trades <= 0:
            continue
        bars.append(b)
    bars.sort(key=lambda b: b.ts_ns)
    return bars


def query_bars(mp_query: Path, repo_root: Path, path: Path) -> list[Bar]:
    env = dict(__import__("os").environ)
    env["RUST_LOG"] = "off"
    raw = run_json_command(
        [
            str(mp_query),
            "bars",
            "--logs",
            str(path),
            "--interval-secs",
            "3600",
            "--json",
        ],
        env,
        repo_root,
    )
    return parse_bars(raw, str(path))


def true_range(current: Bar, previous_close: float | None) -> float | None:
    values = (current.high, current.low, current.close)
    if not all(math.isfinite(x) for x in values):
        return None
    hl = current.high - current.low
    if hl < 0 or not math.isfinite(hl):
        return None
    if previous_close is None:
        result = hl
    else:
        result = max(hl, abs(current.high - previous_close), abs(current.low - previous_close))
    return result if math.isfinite(result) and result >= 0 else None


def prior_atr(bars: list[Bar], index: int) -> float | None:
    if index < ATR_WINDOW:
        return None
    window = bars[index - ATR_WINDOW : index]
    trs: list[float] = []
    for j, bar in enumerate(window):
        previous_close = window[j - 1].close if j else None
        tr = true_range(bar, previous_close)
        if tr is None:
            return None
        trs.append(tr)
    value = sum(trs) / ATR_WINDOW
    return value if math.isfinite(value) and value > 0 else None


def volume_percentile(bars: list[Bar], index: int) -> float | None:
    if index + 1 < VOLUME_WINDOW:
        return None
    window = bars[index + 1 - VOLUME_WINDOW : index + 1]
    volumes = [bar.volume for bar in window]
    if not all(math.isfinite(v) and v > 0 for v in volumes):
        return None
    current = volumes[-1]
    return sum(v < current for v in volumes) / VOLUME_WINDOW * 100.0


def contiguous(a: Bar, b: Bar) -> bool:
    return b.ts_ns - a.ts_ns == BAR_NS


def split_contiguous(bars: list[Bar]) -> list[list[Bar]]:
    if not bars:
        return []
    segments: list[list[Bar]] = [[bars[0]]]
    for bar in bars[1:]:
        if contiguous(segments[-1][-1], bar):
            segments[-1].append(bar)
        else:
            segments.append([bar])
    return segments


def detect_events(
    venue: str,
    symbol: str,
    bars: Iterable[Bar],
    volume_pct: float,
    range_ratio: float,
) -> list[Event]:
    bars = list(bars)
    events: list[Event] = []
    # The signal is evaluated on a CLOSED climax bar. The return starts after
    # that close; this is the no-lookahead boundary for the event study.
    for i in range(max(VOLUME_WINDOW - 1, ATR_WINDOW, 2), len(bars)):
        current = bars[i]
        previous = bars[i - 1]
        previous_two = bars[i - 2]
        pct = volume_percentile(bars, i)
        atr = prior_atr(bars, i)
        if pct is None or atr is None:
            continue
        spread = current.high - current.low
        if spread <= 0 or spread > range_ratio * atr:
            continue
        if pct < volume_pct:
            continue
        down_climax = (
            current.close < current.open
            and previous.close > previous.open
            and previous_two.close > previous_two.open
            and current.delta >= 0.0
        )
        up_climax = (
            current.close > current.open
            and previous.close < previous.open
            and previous_two.close < previous_two.open
            and current.delta <= 0.0
        )
        if not (down_climax or up_climax):
            continue
        direction = 1 if down_climax else -1
        d = utc_day(current.ts_ns)
        events.append(
            Event(
                venue=venue,
                symbol=symbol,
                ts_ns=current.ts_ns,
                direction=direction,
                volume_pct=pct,
                range_ratio=spread / atr,
                week=week_label(d),
                day=d.isoformat(),
                climax_close=current.close,
            )
        )
    return events


def close_return(bars_by_ts: dict[int, Bar], start_ts: int, horizon: int) -> float | None:
    start = bars_by_ts.get(start_ts)
    end = bars_by_ts.get(start_ts + horizon * BAR_NS)
    if start is None or end is None or start.close <= 0:
        return None
    result = end.close / start.close - 1.0
    return result if math.isfinite(result) else None


def bootstrap_ci(values: list[float], seed: int, n_boot: int) -> tuple[float, float]:
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
    return lo, hi


def collect_event_returns(
    events: list[Event],
    bars_by_key: dict[tuple[str, str], dict[int, Bar]],
    horizon: int,
) -> tuple[list[float], list[float], list[Event]]:
    signed: list[float] = []
    excess: list[float] = []
    complete: list[Event] = []
    for event in events:
        asset = bars_by_key[(event.venue, event.symbol)]
        benchmark_symbol = "ETHUSDT" if event.symbol == "BTCUSDT" else "BTCUSDT" if event.symbol == "ETHUSDT" else "ETH" if event.symbol == "BTC" else "BTC"
        benchmark = bars_by_key.get((event.venue, benchmark_symbol))
        asset_return = close_return(asset, event.ts_ns, horizon)
        benchmark_return = close_return(benchmark, event.ts_ns, horizon) if benchmark else None
        if asset_return is None or benchmark_return is None:
            continue
        signed_return = event.direction * asset_return
        excess_return = event.direction * (asset_return - benchmark_return)
        signed.append(signed_return)
        excess.append(excess_return)
        complete.append(event)
    return signed, excess, complete


def make_result(
    events: list[Event],
    bars_by_key: dict[tuple[str, str], dict[int, Bar]],
    volume_pct: float,
    range_ratio: float,
    seed: int,
    n_boot: int,
) -> ConfigResult:
    car_bps: dict[str, float] = {}
    ci95_bps: dict[str, list[float]] = {}
    excess_car_bps: dict[str, float] = {}
    excess_ci95_bps: dict[str, list[float]] = {}
    complete_counts: dict[int, int] = {}
    all_complete: list[Event] = []
    raw_positive_horizons = 0
    excess_positive_horizons = 0
    for horizon in HORIZONS:
        signed, excess, complete = collect_event_returns(events, bars_by_key, horizon)
        complete_counts[horizon] = len(complete)
        all_complete = complete if horizon == HORIZONS[-1] else all_complete
        mean_signed = sum(signed) / len(signed) if signed else 0.0
        mean_excess = sum(excess) / len(excess) if excess else 0.0
        raw_ci = bootstrap_ci(signed, seed + int(volume_pct) * 100 + int(range_ratio * 10) + horizon, n_boot)
        excess_ci = bootstrap_ci(excess, seed + 10_000 + int(volume_pct) * 100 + int(range_ratio * 10) + horizon, n_boot)
        key = f"+{horizon}h"
        car_bps[key] = mean_signed * 10_000.0
        ci95_bps[key] = [raw_ci[0] * 10_000.0, raw_ci[1] * 10_000.0]
        excess_car_bps[key] = mean_excess * 10_000.0
        excess_ci95_bps[key] = [excess_ci[0] * 10_000.0, excess_ci[1] * 10_000.0]
        if mean_signed > 0:
            raw_positive_horizons += 1
        if mean_excess > 0:
            excess_positive_horizons += 1
    days = sorted({event.day for event in events})
    weeks = sorted({event.week for event in events})
    return ConfigResult(
        volume_pct=volume_pct,
        range_ratio=range_ratio,
        n_events_raw=len(events),
        n_events_complete_1h=complete_counts[1],
        n_events_complete_2h=complete_counts[2],
        n_events_complete_4h=complete_counts[4],
        n_days=len(days),
        n_weeks=len(weeks),
        venues=sorted({event.venue for event in events}),
        symbols=sorted({event.symbol for event in events}),
        car_bps=car_bps,
        ci95_bps=ci95_bps,
        excess_car_bps=excess_car_bps,
        excess_ci95_bps=excess_ci95_bps,
        raw_positive_horizons=raw_positive_horizons,
        excess_positive_horizons=excess_positive_horizons,
        event_days=days,
    )


def fmt_bps(value: float) -> str:
    return f"{value:+.2f} bps"


def build_report(
    audit_records: list[dict],
    eligible: dict[tuple[str, str], list[str]],
    events_by_config: dict[tuple[float, float], list[Event]],
    pooled_results: list[ConfigResult],
    strata_results: dict[str, list[ConfigResult]],
    seed: int,
    n_boot: int,
) -> dict:
    # FPX-1 is a corpus sufficiency gate, not a post-hoc parameter choice:
    # use the least restrictive cell that was already declared in FPX-2
    # (80th percentile / 0.6 ATR) to determine whether each recording has
    # enough events. Every other fixed cell remains fully reported below.
    reference_key = (80.0, 0.6)
    reference_events = events_by_config.get(reference_key, [])
    grouped_reference: dict[tuple[str, str], list[Event]] = {}
    for event in reference_events:
        grouped_reference.setdefault((event.venue, event.symbol), []).append(event)
    fpx1_recordings = [
        (venue, symbol)
        for venue, symbols in VENUE_SYMBOLS.items()
        for symbol in symbols
    ]
    reference_rows = []
    for venue, symbol in fpx1_recordings:
        group = grouped_reference.get((venue, symbol), [])
        reference_rows.append(
            {
                "venue": venue,
                "symbol": symbol,
                "n_events": len(group),
                "n_days": len({e.day for e in group}),
                "n_weeks": len({e.week for e in group}),
                "days": sorted({e.day for e in group}),
            }
        )
    fpx1_pass = bool(reference_rows) and all(
        row["n_events"] >= 20 and row["n_weeks"] >= 3 for row in reference_rows
    )

    fpx1_by_config: dict[str, dict] = {}
    for key, events in events_by_config.items():
        vp, rr = key
        grouped: dict[tuple[str, str], list[Event]] = {}
        for event in events:
            grouped.setdefault((event.venue, event.symbol), []).append(event)
        rows = []
        for venue, symbol in fpx1_recordings:
            group = grouped.get((venue, symbol), [])
            rows.append(
                {
                    "venue": venue,
                    "symbol": symbol,
                    "n_events": len(group),
                    "n_days": len({e.day for e in group}),
                    "n_weeks": len({e.week for e in group}),
                    "days": sorted({e.day for e in group}),
                }
            )
        fpx1_by_config[f"pct{int(vp)}_range{rr:.1f}"] = {
            "thresholds": {"volume_percentile": vp, "range_atr_ratio": rr},
            "by_recording": rows,
            "is_reference_for_fpx1": key == reference_key,
            "all_recordings_meet_20_events_and_3_weeks": bool(rows)
            and all(r["n_events"] >= 20 and r["n_weeks"] >= 3 for r in rows),
        }

    # Event-study kill leg: opposite means negative signed/excess CAR. A
    # config is adverse when its excess CI excludes zero below zero at >=2/3
    # horizons. This is the exact FPX falsification language operationalized
    # without claiming that a sparse/non-gradable sample is a kill.
    adverse_configs: list[str] = []
    for result in pooled_results:
        adverse_horizons = [
            horizon
            for horizon, ci in result.excess_ci95_bps.items()
            if ci[1] < 0.0
        ]
        if len(adverse_horizons) >= 2:
            adverse_configs.append(f"pct{int(result.volume_pct)}_range{result.range_ratio:.1f}")
    event_study_kill = len(adverse_configs) >= 2 and fpx1_pass

    return {
        "run_id": "fpx-event-study-2026-09-13",
        "kind": "fpx_event_study",
        "hypothesis_id": "footprint-exhaustion-v1",
        "protocol": {
            "bar_interval": "1h",
            "volume_window_bars": VOLUME_WINDOW,
            "atr_window_bars": ATR_WINDOW,
            "volume_percentiles": list(VOLUME_PCTS),
            "range_atr_ratios": list(RANGE_RATIOS),
            "horizons_hours": list(HORIZONS),
            "seed": seed,
            "bootstrap_replicates": n_boot,
            "event_timestamp": "climax-bar-close; +1h starts with the next closed bar",
            "atr_denominator": "20 bars immediately before candidate; candidate excluded",
            "delta": "buy_vol - sell_vol from mp-query bars; mirrored sign condition",
            "integrity": "coverage >= 0.995, no blocking INT-4 findings, zero stale_bursts; segments reset at any non-eligible/missing day",
            "partial_windows": "omitted",
            "costs": "not applied in event study; cost/WF criteria require backtest",
        },
        "data_gate": {
            "start_day": START_DAY.isoformat(),
            "end_day": END_DAY.isoformat(),
            "eligible_days_by_recording": {
                f"{venue}:{symbol}": days for (venue, symbol), days in sorted(eligible.items())
            },
            "audit_records": audit_records,
            "fpx1_by_config": fpx1_by_config,
            "fpx1_pass": fpx1_pass,
        },
        "pooled_results": [asdict(result) for result in pooled_results],
        "strata_results": {
            name: [asdict(result) for result in values]
            for name, values in sorted(strata_results.items())
        },
        "falsification": {
            "event_study_excess_ci_opposite_at_two_or_more_horizons_in_two_or_more_configs": {
                "status": "KILL" if event_study_kill else "NOT_TRIGGERED",
                "adverse_configs": adverse_configs,
                "requires_fpx1": True,
            },
            "expectancy_2x_cost": {"status": "NOT_ASSESSED", "reason": "event study precedes full-cost sim"},
            "base_net_negative_every_config": {"status": "NOT_ASSESSED", "reason": "event study has no execution/fill cost model"},
            "profitable_episodes_fewer_than_three_weeks": {"status": "NOT_ASSESSED", "reason": "episode P&L is a backtest-stage measure"},
            "walk_forward_sign_flip": {"status": "NOT_ASSESSED", "reason": "walk-forward is downstream of FPX event gate"},
            "overall": "KILLED_BY_EVENT_STUDY" if event_study_kill else "NOT_KILLED_BY_EVENT_STUDY",
            "gate_verdict": "GRADABLE" if fpx1_pass else "NOT_GRADABLE",
        },
    }


def print_summary(report: dict) -> None:
    print("=== footprint-exhaustion-v1 FPX event study ===")
    print(f"run_id={report['run_id']} seed={report['protocol']['seed']} bootstrap={report['protocol']['bootstrap_replicates']}")
    print(f"FPX-1 fully gradable: {report['data_gate']['fpx1_pass']}")
    print("Eligible recording days:")
    for key, days in report["data_gate"]["eligible_days_by_recording"].items():
        print(f"  {key}: {len(days)} days ({', '.join(days)})")
    print("\nPooled directional CAR (signed mark return; bps):")
    for row in report["pooled_results"]:
        label = f"pct{int(row['volume_pct'])}/range{row['range_ratio']:.1f}"
        raw = ", ".join(
            f"{h} {fmt_bps(row['car_bps'][h])} CI95 [{fmt_bps(row['ci95_bps'][h][0])}, {fmt_bps(row['ci95_bps'][h][1])}]"
            for h in ("+1h", "+2h", "+4h")
        )
        excess = "; ".join(
            f"{h} {fmt_bps(row['excess_car_bps'][h])} CI95 [{fmt_bps(row['excess_ci95_bps'][h][0])}, {fmt_bps(row['excess_ci95_bps'][h][1])}]"
            for h in ("+1h", "+2h", "+4h")
        )
        print(f"  {label}: n_raw={row['n_events_raw']} n_complete=({row['n_events_complete_1h']},{row['n_events_complete_2h']},{row['n_events_complete_4h']}) days={row['n_days']} weeks={row['n_weeks']}")
        print(f"    raw: {raw}")
        print(f"    BTC/ETH excess: {excess}")
    f = report["falsification"]
    print("\nFalsification status:")
    print(f"  FPX-1 data gate: {f['gate_verdict']}")
    print(f"  FPX event-study opposite-CI criterion: {f['event_study_excess_ci_opposite_at_two_or_more_horizons_in_two_or_more_configs']['status']} ({f['event_study_excess_ci_opposite_at_two_or_more_horizons_in_two_or_more_configs']['adverse_configs']})")
    print("  full-cost expectancy / base-net / week concentration / WF: NOT ASSESSED (no backtest run)")
    print(f"  overall: {f['overall']}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=Path("."))
    parser.add_argument("--data-raw", type=Path, default=Path("data/raw"))
    parser.add_argument("--mp-query", type=Path, default=Path("target/release/mp-query.exe"))
    parser.add_argument("--mp-ops", type=Path, default=Path("target/release/mp-ops.exe"))
    parser.add_argument("--output", type=Path, default=Path("research/fpx_event_study_2026-09-13.json"))
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--bootstrap", type=int, default=DEFAULT_BOOTSTRAPS)
    args = parser.parse_args(argv)

    repo_root = args.repo_root.resolve()
    data_raw = (repo_root / args.data_raw).resolve() if not args.data_raw.is_absolute() else args.data_raw.resolve()
    mp_query = (repo_root / args.mp_query).resolve() if not args.mp_query.is_absolute() else args.mp_query.resolve()
    mp_ops = (repo_root / args.mp_ops).resolve() if not args.mp_ops.is_absolute() else args.mp_ops.resolve()
    output = (repo_root / args.output).resolve() if not args.output.is_absolute() else args.output.resolve()
    if not data_raw.is_dir():
        raise StudyError(f"raw data directory missing: {data_raw}")
    if not mp_query.exists() or not mp_ops.exists():
        raise StudyError(f"required binaries missing: {mp_query} / {mp_ops}")
    if args.bootstrap < 100:
        raise StudyError("--bootstrap must be >=100")

    audits: list[dict] = []
    eligible: dict[tuple[str, str], list[str]] = {}
    bars_by_key: dict[tuple[str, str], dict[int, Bar]] = {}
    # First pass audits every available candidate file. No bars are queried
    # before FPX-3 integrity eligibility is known.
    for venue, symbols in VENUE_SYMBOLS.items():
        for symbol in symbols:
            key = (venue, symbol)
            eligible[key] = []
            raw_by_day: dict[str, list[Bar]] = {}
            for day in day_strings():
                path = log_path(data_raw, venue, symbol, day)
                if not path.exists():
                    audits.append({"venue": venue, "symbol": symbol, "day": day, "present": False, "eligible": False, "reason": "missing log"})
                    continue
                audit = audit_one(mp_ops, repo_root, data_raw, venue, symbol, day)
                clean = is_fpx_clean(audit)
                audits.append({
                    "venue": venue,
                    "symbol": symbol,
                    "day": day,
                    "present": True,
                    "eligible": clean,
                    "event_count": audit.get("event_count", 0),
                    "coverage": audit.get("coverage", 0.0),
                    "stale_bursts": len(audit.get("stale_bursts") or []),
                    "blocking_codes": blocking_codes(audit),
                    "findings": audit.get("findings", []),
                })
                if not clean:
                    continue
                raw_by_day[day] = query_bars(mp_query, repo_root, path)
                if raw_by_day[day]:
                    eligible[key].append(day)
            # Build only contiguous eligible segments. A dirty/missing day is a
            # hard reset: no ATR/percentile state may leak through it.
            ordered_days = sorted(raw_by_day)
            segments: list[list[Bar]] = []
            current: list[Bar] = []
            previous_day: date | None = None
            for day in ordered_days:
                d = datetime.strptime(day, "%Y%m%d").date()
                if previous_day is None or d - previous_day != timedelta(days=1):
                    if current:
                        segments.append(current)
                    current = []
                day_bars = raw_by_day[day]
                if current and not contiguous(current[-1], day_bars[0]):
                    segments.append(current)
                    current = []
                current.extend(day_bars)
                previous_day = d
            if current:
                segments.append(current)
            # Keep one map for returns, while detection below uses segments to
            # enforce warmup/reset boundaries.
            merged: dict[int, Bar] = {}
            for segment in segments:
                merged.update({bar.ts_ns: bar for bar in segment})
            bars_by_key[key] = merged
            # Store segments temporarily as an attribute-like local map.
            # Recompute event lists below from the same segment boundaries.
            # (The explicit map keeps this script stdlib-only and auditable.)
            eligible[key] = sorted(eligible[key])
            # Attach via a private side map outside the serialized report.
            if "_segments" not in locals():
                _segments = {}
            _segments[key] = segments

    events_by_config: dict[tuple[float, float], list[Event]] = {
        (vp, rr): [] for vp in VOLUME_PCTS for rr in RANGE_RATIOS
    }
    for key, segments in _segments.items():
        venue, symbol = key
        for segment in segments:
            for vp in VOLUME_PCTS:
                for rr in RANGE_RATIOS:
                    events_by_config[(vp, rr)].extend(detect_events(venue, symbol, segment, vp, rr))

    pooled_results = [
        make_result(events_by_config[(vp, rr)], bars_by_key, vp, rr, args.seed, args.bootstrap)
        for vp in VOLUME_PCTS
        for rr in RANGE_RATIOS
    ]
    # Preserve venue/symbol strata in the artifact so a pooled result cannot
    # hide a venue-specific reversal or a single-symbol concentration.
    strata_results: dict[str, list[ConfigResult]] = {}
    for venue, symbols in VENUE_SYMBOLS.items():
        for symbol in symbols:
            name = f"{venue}:{symbol}"
            values = []
            for vp in VOLUME_PCTS:
                for rr in RANGE_RATIOS:
                    events = [e for e in events_by_config[(vp, rr)] if e.venue == venue and e.symbol == symbol]
                    values.append(make_result(events, bars_by_key, vp, rr, args.seed, args.bootstrap))
            strata_results[name] = values

    report = build_report(
        audits,
        eligible,
        events_by_config,
        pooled_results,
        strata_results,
        args.seed,
        args.bootstrap,
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print_summary(report)
    print(f"\nreport -> {output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except StudyError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        raise SystemExit(2)
