#!/usr/bin/env python3
"""Correlate scorecard margins: worst_gap_ns vs stale_bursts vs coverage.

Answers the spec 024 question: do the numeric bar (coverage >= 0.995) and the
Phase-0 burst-free window condition measure the same degradation, or are they
independent signals?

  - coverage        = aggregate completeness (driven by total gap time)
  - worst_gap_ns    = longest single recv-clock hole (> 120s tolerance)
  - stale_bursts    = count of stale-event groups (late-arriving buffered data,
                      grouped at 90s) -- the Phase-0 window condition

All three live on each hyperliquid recording in data/scorecards/*.json
(gitignored).  The script reads every recording across every scorecard and
reports Pearson + Spearman correlations plus the critical |r| for p < 0.05 at
the current n, so small-sample noise is visible.

Usage:
  py -3 ops/scripts/margin_correlation.py            # all hyperliquid recordings
  py -3 ops/scripts/margin_correlation.py -Pairs     # also print the sorted pairs
"""

import argparse
import glob
import json
import math
import os
import statistics
import sys

ROOT = os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
)
SCORECARDS = os.path.join(ROOT, "data", "scorecards")

# Two-tailed critical |r| for p < 0.05, Pearson with df = n - 2.
# Table: df -> critical |r| (df 1..30; beyond 30 use the 0.349 asymptote).
CRITICAL_R = {
    1: 0.997,
    2: 0.950,
    3: 0.878,
    4: 0.811,
    5: 0.754,
    6: 0.707,
    7: 0.666,
    8: 0.632,
    9: 0.602,
    10: 0.576,
    11: 0.553,
    12: 0.532,
    13: 0.514,
    14: 0.497,
    15: 0.482,
    16: 0.468,
    17: 0.456,
    18: 0.444,
    19: 0.433,
    20: 0.423,
    22: 0.404,
    24: 0.388,
    26: 0.374,
    28: 0.361,
    30: 0.349,
}


def load_rows():
    rows = []
    for path in sorted(glob.glob(os.path.join(SCORECARDS, "2026-*.json"))):
        if path.endswith("_bursts.json"):
            continue
        data = json.load(open(path, encoding="utf-8-sig"))
        for rec in data.get("recordings", []):
            if rec.get("venue") != "hyperliquid":
                continue
            rows.append(
                {
                    "day": data["date"],
                    "sym": rec["symbol"],
                    "cov": rec["coverage"],
                    "gap_s": rec.get("worst_gap_ns", 0) / 1e9,
                    "bursts": rec.get("stale_bursts", 0),
                    "clean": rec.get("clean", False),
                }
            )
    return rows


def pearson(xs, ys):
    n = len(xs)
    mx, my = statistics.mean(xs), statistics.mean(ys)
    cov = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    sx = math.sqrt(sum((x - mx) ** 2 for x in xs))
    sy = math.sqrt(sum((y - my) ** 2 for y in ys))
    return cov / (sx * sy) if sx and sy else float("nan")


def rank(vals):
    # Average-rank ties; standard for Spearman.
    order = sorted(range(len(vals)), key=lambda i: vals[i])
    ranks = [0.0] * len(vals)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and vals[order[j + 1]] == vals[order[i]]:
            j += 1
        avg = (i + j) / 2 + 1
        for k in range(i, j + 1):
            ranks[order[k]] = avg
        i = j + 1
    return ranks


def critical_r(n):
    """Two-tailed critical |r| for Pearson at p < 0.05, df = n - 2."""
    df = n - 2
    if df <= 0:
        return float("nan")
    keys = sorted(CRITICAL_R)
    if df >= keys[-1]:
        return CRITICAL_R[keys[-1]]
    # Linear interpolate between table rows (df steps are small enough).
    lo = max(k for k in keys if k <= df)
    hi = min(k for k in keys if k > df)
    t = (df - lo) / (hi - lo)
    return CRITICAL_R[lo] + t * (CRITICAL_R[hi] - CRITICAL_R[lo])


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "-Pairs", action="store_true", help="also print the sorted per-recording table"
    )
    args = ap.parse_args()

    rows = load_rows()
    if not rows:
        print("No hyperliquid recordings found in", SCORECARDS)
        return 1

    n = len(rows)
    print(
        f"n = {n} hyperliquid recordings across {len(set(r['day'] for r in rows))} days"
    )
    crit = critical_r(n)
    print(f"critical |r| for p<0.05 at df={n - 2}: {crit:.3f}\n")

    pairs = [
        ("worst_gap vs stale_bursts", "gap_s", "bursts"),
        ("coverage vs stale_bursts", "cov", "bursts"),
        ("coverage vs worst_gap", "cov", "gap_s"),
    ]
    for label, a, b in pairs:
        xs = [r[a] for r in rows]
        ys = [r[b] for r in rows]
        rp = pearson(xs, ys)
        rs = pearson(rank(xs), rank(ys))
        sig = "p<0.05" if abs(rp) >= crit else "n.s."
        print(f"{label:<30} Pearson r={rp:+.3f}  Spearman rho={rs:+.3f}  {sig}")

    if args.Pairs:
        print("\nPer-recording table (sorted by worst_gap):")
        for r in sorted(rows, key=lambda r: r["gap_s"]):
            state = "CLEAN" if r["clean"] else "dirty"
            print(
                f"  {r['day']} {r['sym']:<3} gap={r['gap_s']:5.0f}s bursts={r['bursts']:2d} cov={r['cov']:.4f} {state}"
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
