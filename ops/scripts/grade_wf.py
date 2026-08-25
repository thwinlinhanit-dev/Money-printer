#!/usr/bin/env python3
"""Grade the liq-fade-v1 walk-forward falsification rule (machine-checkable).

Implements the EXACT procedure in strategies/liq-fade-v1/hypothesis.md
("Grade procedure — the >=2-of-3-windows rule"). Consumes captured `sim wf`
stdout (one file per day) plus the per-day G1 2x-cost expectancies, and emits
the per-window classification, the three falsification criteria, and the
overall machine verdict. Deterministic: no randomness, no network.

Usage:
  python ops/scripts/grade_wf.py DAY=PATH [DAY=PATH ...] \
      --stress2x DAY=VALUE [--stress2x DAY=VALUE ...] \
      --day-trades DAY=N [--day-trades DAY=N ...]

  DAY=PATH        captured `sim wf` stdout for one recording day; DAY is the
                  calendar day label (e.g. 20260814). Order on the command
                  line is the chronological order windows are graded in.
  --stress2x      the day's G1 full-day run stress_expectancy_2x (criterion
                  #1 input).
  --day-trades    the day's total trades in the full-day G1 run (criterion
                  #2 input: how many distinct calendar days the edge traded).

Exit codes: 0 = grade completed (verdict on stdout); 2 = input unusable.
"""

import json
import math
import re
import sys

WINDOW_RE = re.compile(
    r"window test=\[(\d+),(\d+)\): verdict=(\w+) "
    r"in_exp=([-+0-9.eEinf]+) best_params=(.*?) "
    r"oos_trades=(\d+) oos_exp=([-+0-9.eEinf]+) oos_stress2x=([-+0-9.eEinf]+)"
)
SUMMARY_RE = re.compile(r"wf: windows=(\d+) vacuous=(\d+) selected=(\d+)")


def parse_float(s):
    return float(s.replace("+", ""))


def parse_window_line(line, day):
    m = WINDOW_RE.search(line)
    if not m:
        return None
    test_start, test_end, verdict, in_exp, bp, oos_trades, oos_exp, oos_s2x = m.groups()
    params = None
    if bp != "None":
        # Rust Debug prints Some({...}) for the Option<BTreeMap>; strip the
        # wrapper — the map itself is valid JSON.
        inner = bp[5:-1] if bp.startswith("Some(") else bp
        params = json.loads(inner)
    return {
        "day": day,
        "test_start_ns": int(test_start),
        "test_end_ns": int(test_end),
        "verdict": verdict,
        "in_exp": None if verdict == "VACUOUS" else parse_float(in_exp),
        "best_params": params,
        "oos_trades": int(oos_trades),
        "oos_exp": parse_float(oos_exp),
        "oos_stress2x": parse_float(oos_s2x),
    }


def classify(w):
    """Exact window classification (procedure step 1)."""
    if w["verdict"] == "VACUOUS":
        return "vacuous"  # no selection made; contributes nothing
    if w["oos_trades"] == 0:
        return "oos-inconclusive"  # selection never exercised OOS
    return "gradeable"


def flip(w):
    """Exact flip definition: in_exp * oos_exp < 0 (strict opposite signs)."""
    return w["in_exp"] * w["oos_exp"] < 0


def grade(files, stress2x, day_trades):
    windows = []
    for day, path in files:
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                w = parse_window_line(line, day)
                if w is not None:
                    windows.append(w)

    # Sanity: every day's capture must have parsed windows (never grade on
    # an empty or mistyped input).
    parsed_days = {w["day"] for w in windows}
    for day, _ in files:
        if day not in parsed_days:
            print(f"ERROR: no window lines parsed from {day} ({_})", file=sys.stderr)
            return None

    # Per-window classification.
    rows = []
    for w in windows:
        cls = classify(w)
        flipped = flip(w) if cls == "gradeable" else False
        rows.append((w, cls, flipped))

    vacuous = sum(1 for _, c, _ in rows if c == "vacuous")
    inconclusive = sum(1 for _, c, _ in rows if c == "oos-inconclusive")
    gradeable = sum(1 for _, c, _ in rows if c == "gradeable")
    flips = sum(1 for _, c, f in rows if c == "gradeable" and f)

    # Criterion #3: OOS flips sign vs in-sample in >= 2 of 3 gradeable
    # windows. Anchor case G=3 needs F>=2 (literal wording); larger G scales
    # the same 2/3 rate: F >= ceil(2G/3).
    if gradeable >= 3:
        c3 = "FIRED" if flips >= math.ceil(2 * gradeable / 3) else "PASS"
    else:
        c3 = "NOT-GRADED"

    # Criterion #1: expectancy <= 0 in the 2x-cost column at G1, graded over
    # days that ACTUALLY TRADED (SIM-9 integrity: a 0-trade day scoring
    # exactly 0.0 is absence of evidence, never evidence of a losing edge).
    c1_inputs = {d: v for d, v in stress2x.items() if day_trades.get(d, 0) >= 1}
    c1_skipped = [d for d in stress2x if day_trades.get(d, 0) < 1]
    if not c1_inputs:
        c1 = "NOT-GRADED"
        c1_detail = "no --stress2x input from a day with trades>=1"
        if c1_skipped:
            c1_detail += f" (0-trade days excluded: {', '.join(c1_skipped)})"
    elif any(v <= 0.0 for v in c1_inputs.values()):
        c1 = "ENGAGED"
        c1_detail = ", ".join(f"{d}={v:+.2f}" for d, v in sorted(c1_inputs.items()))
        if c1_skipped:
            c1_detail += f" (0-trade days excluded: {', '.join(c1_skipped)})"
    else:
        c1 = "PASS"
        c1_detail = ", ".join(f"{d}={v:+.2f}" for d, v in sorted(c1_inputs.items()))
        if c1_skipped:
            c1_detail += f" (0-trade days excluded: {', '.join(c1_skipped)})"

    # Criterion #2: edge concentrates in < 3 distinct calendar windows.
    traded_days = sum(1 for v in day_trades.values() if v >= 1)
    corpus_days = len(files)
    if traded_days == 0:
        c2 = "NOT-GRADED"
        c2_detail = "no trades on any recorded day"
    elif corpus_days < 3:
        c2 = "NOT-GRADED"
        c2_detail = (
            f"corpus {corpus_days} day(s) < 3 - cannot distinguish harvest from fluke"
        )
    elif traded_days < 3:
        c2 = "FIRED"
        c2_detail = f"trades on {traded_days} distinct day(s) < 3"
    else:
        c2 = "PASS"
        c2_detail = f"trades on {traded_days} distinct days"

    killed = c1 == "ENGAGED" or c2 == "FIRED" or c3 == "FIRED"
    overall = "KILL" if killed else "NOT-YET-FALSIFIED"

    # Report.
    print(
        f"windows graded: {len(windows)} across days {', '.join(d for d, _ in files)}"
    )
    for w, cls, flipped in rows:
        sel = (
            f"in_exp={w['in_exp']:+.6f} best_params={w['best_params']}"
            if cls != "vacuous"
            else "(no selection)"
        )
        print(
            f"  [{w['day']} t={w['test_start_ns']},o={w['oos_trades']}) "
            f"{w['verdict']:<9} {cls:<17} {sel} "
            f"oos_exp={w['oos_exp']:+.6f} oos_stress2x={w['oos_stress2x']:+.6f}"
            f"{' FLIP' if flipped else ''}"
        )
    print(
        f"counts: vacuous={vacuous} oos_inconclusive={inconclusive} "
        f"gradeable={gradeable} flips={flips}"
    )
    print(f"criterion #1 (G1 2x-cost <= 0): {c1} [{c1_detail}]")
    print(f"criterion #2 (<3 distinct calendar windows): {c2} [{c2_detail}]")
    print(
        f"criterion #3 (flips >= ceil(2*gradeable/3), gradeable>=3): {c3} "
        f"[gradeable={gradeable}, flips={flips}]"
    )
    print(f"OVERALL: {overall}")
    return overall


def main(argv):
    files = []
    stress2x = {}
    day_trades = {}
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--stress2x":
            i += 1
            d, v = argv[i].split("=", 1)
            stress2x[d] = float(v)
        elif a == "--day-trades":
            i += 1
            d, v = argv[i].split("=", 1)
            day_trades[d] = int(v)
        else:
            d, p = a.split("=", 1)
            files.append((d, p))
        i += 1

    if not files:
        print(
            "usage: grade_wf.py DAY=PATH ... [--stress2x DAY=VALUE ...] "
            "[--day-trades DAY=N ...]",
            file=sys.stderr,
        )
        return 2

    result = grade(files, stress2x, day_trades)
    return 0 if result is not None else 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
