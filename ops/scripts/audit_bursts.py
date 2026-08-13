#!/usr/bin/env python3
"""Stale-burst + gap profiler for the Hyperliquid recordings (VPN on/off A-B).

Runs `mp-ops audit` for a date and prints burst windows plus coverage gaps.
Since 2026-08-12 the burst grouping lives in the Rust audit (`stale_bursts`
in `mp-ops audit` output, grouped at the same 90s gap) - this script just
renders it, falling back to local grouping only for old binaries/snapshots
that predate the field.  Day-over-day comparison makes the VPN/VPS A-B
verdict obvious (VPN-on days vs VPN-off days vs VPS days).

Usage:
  py -3 ops/scripts/audit_bursts.py -Date 20260810              # snapshot one day
  py -3 ops/scripts/audit_bursts.py -Compare 20260809 20260810  # side-by-side
  py -3 ops/scripts/audit_bursts.py -All                        # all snapshots on disk
  py -3 ops/scripts/audit_bursts.py -Timeline                   # per-hour burst/gap
                                                                # timeline across all
                                                                # snapshots (clustering)

Snapshots are written to data/scorecards/<date>_bursts.json (gitignored data).
"""

import argparse
import datetime
import json
import os
import subprocess
import sys

BIN = os.environ.get("MP_OPS_BIN", "target/release/mp-ops.exe")
ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
SCORECARDS = os.path.join(ROOT, "data", "scorecards")
BURST_GAP_S = 90  # must match storage::audit::STALE_BURST_GAP_NS (fallback only)


def iso(ns):
    return datetime.datetime.fromtimestamp(ns / 1e9, datetime.timezone.utc).strftime("%H:%M:%S")


def run_audit(date, symbol):
    cmd = [os.path.join(ROOT, BIN), "audit", "--date", date, "--venue", "hyperliquid", "--symbol", symbol]
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode != 0:
        return None
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        return None


def bursts(stales):
    # Fallback grouping for binaries/snapshots without Rust-side stale_bursts
    # (2026-08-12: mp-ops emits them; keep this only for cached old data).
    groups = []
    cur = []
    prev = None
    for s in sorted(x["start_ns"] for x in stales):
        if prev is not None and (s - prev) / 1e9 > BURST_GAP_S:
            groups.append(cur)
            cur = []
        cur.append(s)
        prev = s
    if cur:
        groups.append(cur)
    return [{"start": iso(g[0]), "end": iso(g[-1]), "count": len(g)} for g in groups]


def bursts_from_audit(d):
    """Prefer the Rust audit's grouped bursts; fall back to local grouping."""
    raw = d.get("stale_bursts")
    if raw:
        return [
            {"start": iso(b["start_ns"]), "end": iso(b["end_ns"]), "count": 0}
            for b in raw
        ]
    return bursts(d.get("stale_periods", []))


def snapshot_date(date):
    result = {
        "date": date,
        "generated_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "symbols": {},
    }
    for sym in ("BTC", "ETH"):
        d = run_audit(date, sym)
        if d is None:
            result["symbols"][sym] = {"error": "audit failed"}
            continue
        result["symbols"][sym] = {
            "event_count": d.get("event_count"),
            "coverage": d.get("coverage"),
            "stale_count": len(d.get("stale_periods", [])),
            "bursts": bursts_from_audit(d),
            "worst_gap_s": round((d.get("worst_gap_ns") or 0) / 1e9),
            "gaps": [{"start": iso(g["start_ns"]), "end": iso(g["end_ns"])} for g in d.get("gaps", [])],
        }
    return result


def dashed(date):
    return f"{date[:4]}-{date[4:6]}-{date[6:8]}"


def load_or_snapshot(date):
    path = os.path.join(SCORECARDS, f"{dashed(date)}_bursts.json")
    if os.path.exists(path):
        return json.load(open(path)), False
    snap = snapshot_date(date)
    os.makedirs(SCORECARDS, exist_ok=True)
    with open(path, "w") as f:
        json.dump(snap, f, indent=2)
    return snap, True


def hour_of(t):
    return int(t.split(":")[0])


def timeline(snaps):
    """Per-hour burst-start / gap-start counts across all snapshots."""
    burst_h = [0] * 24
    gap_h = [0] * 24
    days_with_burst = [set() for _ in range(24)]
    for snap in snaps:
        date = snap["date"]
        for sym, s in snap["symbols"].items():
            if "error" in s:
                continue
            for b in s.get("bursts", []):
                h = hour_of(b["start"])
                burst_h[h] += 1
                days_with_burst[h].add(date)
            for g in s.get("gaps", []):
                gap_h[hour_of(g["start"])] += 1
    print("Per-hour burst-START timeline (all snapshots, both symbols):")
    print("hour | bursts | days-with-bursts | gap-starts")
    for h in range(24):
        days = len(days_with_burst[h])
        print(f"{h:4d} | {burst_h[h]:5d} | {days:2d} day(s)        | {gap_h[h]}")
    peak = max(range(24), key=lambda h: burst_h[h])
    total = sum(burst_h)
    print()
    print(f"peak hour: {peak:02d}:00-{peak:02d}:59 ({burst_h[peak]}/{total} starts, "
          f"{100 * burst_h[peak] / total:.0f}%)")
    return 0


def print_day(snap):
    print(f"=== {dashed(snap['date'])} ===")
    for sym, s in snap["symbols"].items():
        if "error" in s:
            print(f"  {sym}: {s['error']}")
            continue
        print(f"  {sym}: events={s['event_count']} coverage={s['coverage']} stale={s['stale_count']} worst_gap={s.get('worst_gap_s', '?')}s")
        if s["bursts"]:
            # Per-burst counts only exist for fallback-grouped snapshots;
            # Rust-side bursts render as start-end windows.
            print("    bursts: " + "  ".join(
                f"{b['start']}-{b['end']}" + (f"({b['count']})" if b.get("count") else "")
                for b in s["bursts"]
            ))
        else:
            print("    bursts: none")
        print("    gaps: " + ("none" if not s["gaps"] else "  ".join(f"{g['start']}-{g['end']}" for g in s["gaps"])))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-Date", help="YYYYMMDD to snapshot and print")
    ap.add_argument("-Compare", nargs="+", help="YYYYMMDD dates to compare side-by-side")
    ap.add_argument("-All", action="store_true", help="print every snapshot on disk")
    ap.add_argument("-Timeline", action="store_true", help="per-hour burst/gap clustering across all snapshots")
    args = ap.parse_args()

    if args.Date:
        snap, fresh = load_or_snapshot(args.Date)
        print_day(snap)
        return 0
    if args.Compare:
        snaps = [load_or_snapshot(d)[0] for d in args.Compare]
        for snap in snaps:
            print_day(snap)
            print()
        return 0
    if args.All:
        for name in sorted(os.listdir(SCORECARDS)):
            if name.endswith("_bursts.json"):
                print_day(json.load(open(os.path.join(SCORECARDS, name))))
                print()
        return 0
    if args.Timeline:
        snaps = [
            json.load(open(os.path.join(SCORECARDS, name)))
            for name in sorted(os.listdir(SCORECARDS))
            if name.endswith("_bursts.json")
        ]
        if not snaps:
            print("No burst snapshots on disk; run -Date first.")
            return 1
        return timeline(snaps)
    ap.print_help()
    return 1


if __name__ == "__main__":
    sys.exit(main())
