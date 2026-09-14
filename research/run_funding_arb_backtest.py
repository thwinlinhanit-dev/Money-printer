"""funding-arb-v1 full-cost backtest (pre-registered experiment).

Implements the falsification protocol written in
``strategies/funding-arb-v1/hypothesis.md`` BEFORE any backtest, over the
FARB-2-cleared corpus (14 same-day bybit<->hyperliquid pairs per symbol,
2026-09-12 re-gate record ``backlog-event-studies-2026-09-12-farb2``):

- Signal: annualized cross-venue funding spread (FARB-3 cadence convention,
  hyperliquid x8760 / bybit x1095, bps/yr) on the same underlying, computed
  exactly like ``run_backlog_event_studies.annualized_funding_bps``.
- Entry (no lookahead): flat and |spread| >= entry threshold at hour h-1
  (signal known at the prior bar close) -> open at h. Direction: short the
  positive-funding venue, long the negative-funding one (net-flat perp-perp).
- Exit (same no-lookahead rule): flat and |spread| < exit threshold at h-1
  -> close at h; OR the 14-day time stop; OR end of the contiguously
  recorded segment (forced exit, flagged honestly as ``data_end``).
- Funding accrual (the honest cash-flow leg, distinct from the annualized
  signal): cash to a SHORT leg = +settlement rate, to a LONG leg = -rate,
  at each venue's own cadence with RAW per-period rates (never annualized):
  hyperliquid settles hourly; bybit settles every 8h (00/08/16 UTC) at the
  last recorded bybit frame at/before each boundary. Constant unit notional
  per leg; reported in bps of notional.
- Costs: registry figures for this strategy (RT 29.0 bps base / 42.5 bps
  stressed, both legs entry+exit). The pre-registered kill check runs on
  the 2x-cost column (RT 58/85).

Falsification (from the hypothesis, evaluated and reported verbatim):
- kill if expectancy <= 0 in the 2x-cost column,
- kill if the edge concentrates in < 3 distinct calendar windows,
- kill if walk-forward OOS flips sign vs in-sample in >= 2 of 3 windows.

Deterministic (CONV-11/PD-3): no bootstrap (every trade enumerated), sorted
iteration everywhere, no wall clock in outputs. The journal record is
append-only with a fresh --run-id (E7).

Pure stdlib; research-only (CONV-2).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from run_backlog_event_studies import (  # noqa: E402
    CROSS_VENUE_PAIRS,
    run_carry,
)

HOUR_NS = 3_600_000_000_000
DAY_NS = 86_400_000_000_000
EIGHT_H_NS = 8 * HOUR_NS
MAX_HOLD_HOURS = 14 * 24  # registered time stop

# Registry cost figures (bps of notional, per round trip across BOTH legs).
COST_RT_BPS = {"base": 29.0, "stressed": 42.5, "base_2x": 58.0, "stressed_2x": 85.0}

# (entry_bps_yr, exit_bps_yr) configs from the pre-registered design.
CONFIGS = ((500.0, 250.0), (1000.0, 500.0))

PERIODS_PER_YEAR = {"hyperliquid": 8_760, "bybit": 1_095, "binance": 1_095}


def load_leg(
    mp_query: Path, data_raw: Path, venue: str, sym: str, days: list[str]
) -> dict[int, float]:
    """hour_ts_ns -> raw funding rate (decimal, per venue's own period).

    Rows carry the LAST Funding event rate in the hour bucket (see
    storage/src/analytics.rs carry_series): hyperliquid's hourly settlements
    and bybit's hourly predicted-rate frames. Marks are unused: accrual is
    per unit notional.
    """
    out: dict[int, float] = {}
    for day in sorted(days):
        log = data_raw / f"{day}_{venue}_{sym}.log"
        if not log.exists():
            continue
        for r in run_carry(mp_query, log):
            rate = r.get("funding_rate")
            if rate is None or rate != rate:  # None or NaN
                continue
            out[r["interval_ts_ns"]] = rate
    return out


def annualized_spread_bps_yr(
    fund_a: dict[int, float],
    periods_a: int,
    fund_b: dict[int, float],
    periods_b: int,
) -> dict[int, float]:
    """hour_ts_ns -> spread in bps/yr (FARB-3 cadence-aware annualization)."""
    return {
        h: (fund_a[h] * periods_a - fund_b[h] * periods_b) * 1e4
        for h in sorted(set(fund_a) & set(fund_b))
    }


def contiguous_segments(hours: list[int]) -> list[list[int]]:
    """Split the sorted hourly series into contiguously-recorded runs."""
    segs: list[list[int]] = []
    cur: list[int] = []
    for h in hours:
        if cur and h - cur[-1] != HOUR_NS:
            segs.append(cur)
            cur = []
        cur.append(h)
    if cur:
        segs.append(cur)
    return segs


def settlement_rates(
    fund: dict[int, float], seg_start: int, seg_end: int
) -> dict[int, float]:
    """8h settlement boundary ts -> last recorded rate at/before it.

    Boundaries are 00:00/08:00/16:00 UTC. A boundary inside [seg_start,
    seg_end) settles if any recorded frame covers it within the prior 8h.
    """
    day0 = seg_start // DAY_NS * DAY_NS
    out: dict[int, float] = {}
    b = day0
    while b < seg_end:
        if b >= seg_start:
            # The frame settling at b is the last prediction STRICTLY before
            # b: a frame received in bucket b arrives after the settlement
            # and predicts the NEXT boundary. Search back within the 8h
            # cadence window; no frame in it = no accrual (honest gap).
            for off in range(1, 9):
                cand = b - off * HOUR_NS
                if cand in fund:
                    out[b] = fund[cand]
                    break
        b += EIGHT_H_NS
    return out


def run_episode(
    seg: list[int],
    spread: dict[int, float],
    fund_a: dict[int, float],
    periods_a: int,
    fund_b: dict[int, float],
    periods_b: int,
    entry: float,
    exit_thresh: float,
) -> list[dict]:
    """One venue-pair segment -> episodes with accrual cash flows (bps).

    Convention: ``side`` = +1 long A / -1 short A; leg B is the opposite.
    Cash to a leg per settlement = -side_leg * raw_rate (short receives a
    positive-rate settlement). A-separator: A is the venue whose funding
    frames arrive hourly (hyperliquid) in this corpus's pairs; the 8h venue
    accrues only at its settlement boundaries.
    """
    a_hourly = periods_a > periods_b  # hyperliquid (8760) funds hourly
    if a_hourly:
        settle_8h = settlement_rates(fund_b, seg[0], seg[-1] + HOUR_NS)
    else:
        settle_8h = settlement_rates(fund_a, seg[0], seg[-1] + HOUR_NS)

    episodes: list[dict] = []
    pos: dict | None = None

    def accrue(p: dict, h: int) -> None:
        if a_hourly:
            if h in fund_a:  # HL hourly settlement: cash = -side * raw rate
                p["acc_a"] += -p["side"] * fund_a[h]
                p["n_settle_a"] += 1
            if h in settle_8h:  # B is the 8h venue; leg B side = -side
                p["acc_b"] += p["side"] * settle_8h[h]
                p["n_settle_b"] += 1
        else:
            if h in settle_8h:  # A is the 8h venue
                p["acc_a"] += -p["side"] * settle_8h[h]
                p["n_settle_a"] += 1
            if h in fund_b:  # B hourly; leg B side = -side
                p["acc_b"] += p["side"] * fund_b[h]
                p["n_settle_b"] += 1

    for i in range(1, len(seg)):
        h = seg[i]
        prev = seg[i - 1]
        s_prev = spread[prev]
        if pos is None:
            if abs(s_prev) >= entry:
                # spread = ann_a - ann_b. spread>0 -> A over-funded -> short A.
                side = -1.0 if s_prev > 0 else 1.0
                pos = {
                    "side": side,
                    "entry_ts": h,
                    "entry_spread": s_prev,
                    "acc_a": 0.0,
                    "acc_b": 0.0,
                    "n_settle_a": 0,
                    "n_settle_b": 0,
                }
                accrue(pos, h)  # entry hour's settlements apply
            continue
        # holding: accrue this hour's settlements, then evaluate the exit
        accrue(pos, h)
        held_h = (h - pos["entry_ts"]) // HOUR_NS
        if abs(s_prev) < exit_thresh:
            pos["exit_ts"] = h
            pos["exit_reason"] = "normalized"
            pos["exit_spread"] = s_prev
            episodes.append(pos)
            pos = None
        elif held_h >= MAX_HOLD_HOURS:
            pos["exit_ts"] = h
            pos["exit_reason"] = "time_stop"
            pos["exit_spread"] = spread[h]
            episodes.append(pos)
            pos = None
    if pos is not None:
        pos["exit_ts"] = seg[-1]
        pos["exit_reason"] = "data_end"
        pos["exit_spread"] = spread[seg[-1]]
        episodes.append(pos)
    return episodes


def finalize(
    episodes: list[dict], seg_key: str, pair: str, config: str
) -> list[dict]:
    out = []
    for p in episodes:
        carry_bps = (p["acc_a"] + p["acc_b"]) * 1e4
        out.append(
            {
                "pair": pair,
                "segment": seg_key,
                "config": config,
                "entry_ts": p["entry_ts"],
                "exit_ts": p["exit_ts"],
                "held_hours": (p["exit_ts"] - p["entry_ts"]) // HOUR_NS,
                "side": "short_A_long_B" if p["side"] < 0 else "long_A_short_B",
                "entry_spread_bps_yr": round(p["entry_spread"], 1),
                "exit_spread_bps_yr": round(p["exit_spread"], 1),
                "exit_reason": p["exit_reason"],
                "carry_bps": round(carry_bps, 3),
                "n_settle_a": p["n_settle_a"],
                "n_settle_b": p["n_settle_b"],
            }
        )
    return out


def distinct_windows(episodes: list[dict], cluster_ns: int = 48 * HOUR_NS) -> int:
    """Calendar windows: episode starts clustering within 48h count as ONE."""
    if not episodes:
        return 0
    starts = sorted(e["entry_ts"] for e in episodes)
    windows = 1
    anchor = starts[0]
    for s in starts[1:]:
        if s - anchor > cluster_ns:
            windows += 1
            anchor = s
    return windows


def walk_forward_signflips(episodes: list[dict], key: str) -> dict:
    """3 contiguous time thirds; each split 50/50 by start time into
    in-sample / out-of-sample halves; count OOS-vs-IS expectancy sign flips.
    Vacuous splits are reported honestly, never fabricated."""
    if len(episodes) < 6:
        return {
            "n_windows_evaluated": 0,
            "sign_flips": 0,
            "verdict": "insufficient episodes (need >= 6 for 3 windows x 2 halves)",
        }
    eps = sorted(episodes, key=lambda e: e["entry_ts"])
    n = len(eps)
    thirds = [eps[: n // 3], eps[n // 3 : 2 * n // 3], eps[2 * n // 3 :]]
    flips = 0
    evaluated = 0
    details = []
    for w in thirds:
        if len(w) < 2:
            continue
        half = len(w) // 2
        is_half, oos_half = w[:half], w[half:]
        if not is_half or not oos_half:
            continue
        is_mean = sum(e[key] for e in is_half) / len(is_half)
        oos_mean = sum(e[key] for e in oos_half) / len(oos_half)
        flip = (is_mean > 0) != (oos_mean > 0)
        flips += int(flip)
        evaluated += 1
        details.append(
            {
                "n": len(w),
                "is_expectancy_bps": round(is_mean, 3),
                "oos_expectancy_bps": round(oos_mean, 3),
                "sign_flip": flip,
            }
        )
    return {
        "n_windows_evaluated": evaluated,
        "sign_flips": flips,
        "windows": details,
        "verdict": "kill" if flips >= 2 else "pass",
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="funding-arb-v1 full-cost backtest")
    ap.add_argument(
        "--mp-query", type=Path, default=Path("target/release/mp-query.exe")
    )
    ap.add_argument("--data-raw", type=Path, default=Path("data/raw"))
    ap.add_argument("--runs-dir", type=Path, default=Path("runs"))
    ap.add_argument(
        "--report",
        type=Path,
        default=Path("docs/research/BACKTEST-funding-arb-v1-2026-09-13.md"),
    )
    ap.add_argument(
        "--run-id",
        default="farb2-backtest-2026-09-13",
        help="journal record id (must be new; append-only tracker)",
    )
    args = ap.parse_args(argv)

    # group pair days by venue-pair+symbol, and leg days by (venue, symbol)
    groups: dict[tuple[str, str, str, str], list[str]] = {}
    leg_days: dict[tuple[str, str], set[str]] = {}
    for v_a, sym_a, v_b, sym_b, day in CROSS_VENUE_PAIRS:
        groups.setdefault((v_a, sym_a, v_b, sym_b), []).append(day)
        leg_days.setdefault((v_a, sym_a), set()).add(day)
        leg_days.setdefault((v_b, sym_b), set()).add(day)

    fund_cache = {
        (v, s): load_leg(args.mp_query, args.data_raw, v, s, sorted(ds))
        for (v, s), ds in sorted(leg_days.items())
    }

    all_episodes: list[dict] = []
    pair_stats = {}
    for (v_a, sym_a, v_b, sym_b), days in sorted(groups.items()):
        fund_a = fund_cache[(v_a, sym_a)]
        fund_b = fund_cache[(v_b, sym_b)]
        periods_a = PERIODS_PER_YEAR[v_a]
        periods_b = PERIODS_PER_YEAR[v_b]
        spread = annualized_spread_bps_yr(fund_a, periods_a, fund_b, periods_b)
        pair = f"{v_a}-{sym_a}-vs-{v_b}-{sym_b}"
        pair_eps: list[dict] = []
        for seg in contiguous_segments(sorted(spread)):
            for entry, exit_t in CONFIGS:
                eps = run_episode(
                    seg, spread, fund_a, periods_a, fund_b, periods_b, entry, exit_t
                )
                tag = f"{int(entry)}entry/{int(exit_t)}exit"
                pair_eps.extend(
                    finalize(eps, f"{seg[0]}..{seg[-1]}", pair, tag)
                )
        pair_stats[pair] = {
            "n_overlap_hours_total": len(spread),
            "n_segments": len(contiguous_segments(sorted(spread))),
            "n_episodes_all_configs": len(pair_eps),
        }
        all_episodes.extend(pair_eps)

    # per-config verdicts
    results = {}
    for cost_key in ("base", "base_2x"):
        for entry, exit_t in CONFIGS:
            tag = f"{int(entry)}entry/{int(exit_t)}exit"
            eps = [e for e in all_episodes if e["config"] == tag]
            if not eps:
                results[f"{tag}|{cost_key}"] = {
                    "n_episodes": 0,
                    "expectancy_bps": 0.0,
                    "verdict": "no episodes in corpus",
                }
                continue
            key = f"net_{cost_key}_bps"
            for e in eps:
                e[key] = round(e["carry_bps"] - COST_RT_BPS[cost_key], 3)
            exp = sum(e[key] for e in eps) / len(eps)
            wins = sum(1 for e in eps if e[key] > 0)
            results[f"{tag}|{cost_key}"] = {
                "n_episodes": len(eps),
                "expectancy_bps": round(exp, 3),
                "win_rate": round(wins / len(eps), 3),
                "total_bps": round(sum(e[key] for e in eps), 3),
                "windows": distinct_windows(eps),
                "walkforward": walk_forward_signflips(eps, key),
            }

    # pre-registered falsification, evaluated on the 2x-cost column
    falsification = {}
    for entry, exit_t in CONFIGS:
        tag = f"{int(entry)}entry/{int(exit_t)}exit"
        r = results[f"{tag}|base_2x"]
        checks = {
            "expectancy_le_0_at_2x_cost": r["n_episodes"] > 0
            and r["expectancy_bps"] <= 0,
            "edge_in_lt_3_calendar_windows": r["n_episodes"] > 0 and r["windows"] < 3,
            "wf_oos_signflip_ge_2_of_3": r["walkforward"].get("sign_flips", 0) >= 2,
        }
        falsification[tag] = {"checks": checks, "killed": any(checks.values())}

    summary = {
        "run_id": args.run_id,
        "kind": "farb2_full_cost_backtest",
        "costs_rt_bps": COST_RT_BPS,
        "pair_stats": pair_stats,
        "results": results,
        "falsification": falsification,
        "episodes": sorted(all_episodes, key=lambda e: (e["config"], e["entry_ts"])),
    }

    # journal (append-only, E7)
    runs_dir = args.runs_dir
    runs_dir.mkdir(parents=True, exist_ok=True)
    with (runs_dir / "index.jsonl").open("a", encoding="utf-8") as f:
        f.write(
            json.dumps(
                {
                    "run_id": args.run_id,
                    "kind": "farb2_full_cost_backtest",
                    "date": "2026-09-13",
                    "strategy": "funding-arb-v1",
                    "n_episodes": len(all_episodes),
                    "results": {
                        k: {kk: vv for kk, vv in v.items() if kk != "walkforward"}
                        for k, v in results.items()
                    },
                    "falsification": falsification,
                }
            )
            + "\n"
        )

    # JSON artifact (deterministic; no wall clock)
    out_json = Path(f"research/funding_arb_backtest_{args.run_id}.json")
    out_json.write_text(json.dumps(summary, indent=1, sort_keys=True), encoding="utf-8")

    # markdown report
    lines = [
        "# funding-arb-v1 — full-cost backtest (pre-registered protocol)",
        "",
        f"Run: `{args.run_id}` · corpus: FARB-2-cleared pairs "
        f"(`run_backlog_event_studies.CROSS_VENUE_PAIRS`) · deterministic.",
        "",
        "Accrual: hyperliquid hourly settlements + bybit 8h settlements (last",
        "recorded frame at/before each 00/08/16 UTC boundary), RAW per-period",
        "rates, cash = −side × rate; signal annualized per FARB-3; entries AND",
        "exits act on the prior bar's signal (no lookahead); forced exits at",
        "segment end flagged `data_end`.",
        "",
        "| config | cost column | n | expectancy bps | win rate | windows | WF flips |",
        "|---|---|---|---|---|---|---|",
    ]
    for k, r in results.items():
        wf = r.get("walkforward", {})
        cfg, cost = k.split("|")
        lines.append(
            f"| {cfg} | {cost} | {r['n_episodes']} | {r['expectancy_bps']} "
            f"| {r.get('win_rate', '-')} | {r.get('windows', '-')} "
            f"| {wf.get('sign_flips', '-')} |"
        )
    lines += ["", "## Falsification (2x-cost column, per hypothesis)"]
    for tag, f_ in falsification.items():
        checks = ", ".join(f"{k}={v}" for k, v in f_["checks"].items())
        lines.append(f"- **{tag}**: killed={f_['killed']} ({checks})")
    lines += [
        "",
        "## Episodes (all, net of base and 2x costs)",
        "| config | pair | entry_ts | held_h | side | entry bps/yr | exit | carry | net base | net 2x |",
        "|---|---|---|---|---|---|---|---|---|---|",
    ]
    for e in sorted(all_episodes, key=lambda e: (e["config"], e["entry_ts"])):
        lines.append(
            f"| {e['config']} | {e['pair']} | {e['entry_ts']} | {e['held_hours']} "
            f"| {e['side']} | {e['entry_spread_bps_yr']} | {e['exit_reason']} "
            f"| {e['carry_bps']} | {e.get('net_base_bps', '-')} "
            f"| {e.get('net_base_2x_bps', '-')} |"
        )
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text("\n".join(lines) + "\n", encoding="utf-8")

    print(f"episodes={len(all_episodes)}")
    for k, r in results.items():
        print(f"  {k}: n={r['n_episodes']} expectancy={r['expectancy_bps']}bps")
    for tag, f_ in falsification.items():
        print(f"  falsification[{tag}]: killed={f_['killed']}")
    print(f"report: {args.report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
