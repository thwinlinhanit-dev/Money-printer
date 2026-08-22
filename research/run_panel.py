"""CLI for the historical research panel (roadmap Phase 3.1).

Usage:
    py research/run_panel.py download [--universe <json>] [--symbols BTCUSDT]
        [--dates 2026-08-10..2026-08-12] [--panel-dir <dir>] [--base-url <url>]
        [--bucket daily|monthly]
    py research/run_panel.py verify --sample N [--seed 42] [--base-url <url>]
        [--panel-dir <dir>] [--universe <json>]
    py research/run_panel.py check [--panel-dir <dir>] [--universe <json>]

- ``download``  - fetch klines, sha256-verify, append manifest rows
  (idempotent: an identical sha256 entry is skipped); 404s are recorded as
  ``status=missing`` with a reason, never silently dropped.
- ``verify``    - seeded re-download of N manifest entries; ANY checksum or
  row-count mismatch exits 1 (the panel must rebuild byte-identical).
- ``check``     - every (symbol, day) in the universe window has a manifest
  entry; gaps exit 1.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import panel

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_UNIVERSE = REPO_ROOT / "research" / "panel_universe.json"
DEFAULT_PANEL = REPO_ROOT / "research" / "panel"


def _fail2(message: str) -> None:
    print(f"exit 2: {message}", file=sys.stderr)
    raise SystemExit(2)


def _cmd_download(args) -> int:
    uni = panel.load_universe(args.universe)
    panel_dir = Path(args.panel_dir)
    manifest = panel.load_manifest(panel_dir, uni.universe_version)
    symbols = set(args.symbols) if args.symbols else {s["symbol"] for s in uni.symbols}
    dates = panel.date_range(*args.dates.split("..")) if args.dates else None
    new_records: list[dict] = []
    skipped = 0
    for sym in sorted(uni.symbols, key=lambda s: s["symbol"]):
        if sym["symbol"] not in symbols:
            continue
        sym_dates = dates or panel.date_range(sym["start"], sym["end"])
        for day in sym_dates:
            key = (sym["venue"], sym["symbol"], day)
            if key in manifest and manifest[key].get("status") == "ok":
                skipped += 1
                continue
            try:
                stats = panel.fetch_kline(
                    args.base_url, sym["venue"], sym["symbol"], day
                )
            except panel.PanelError:
                new_records.append(
                    panel.membership_record(uni, sym["venue"], sym["symbol"], day, None)
                )
                continue
            rec = panel.membership_record(uni, sym["venue"], sym["symbol"], day, stats)
            new_records.append(rec)
    panel.append_manifest(panel_dir, uni.universe_version, new_records)
    ok_rows = [r for r in new_records if r["status"] == "ok"]
    missing = [r for r in new_records if r["status"] == "missing"]
    print(
        f"downloaded {len(ok_rows)} files, {len(missing)} missing on source, "
        f"{skipped} already in manifest"
    )
    for rec in missing:
        print(f"  MISSING {rec['date']} {rec['venue']}:{rec['symbol']}")
    return 0


def _cmd_verify(args) -> int:
    uni = panel.load_universe(args.universe)
    problems = panel.verify_sample(
        args.panel_dir, uni.universe_version, args.base_url, args.sample, args.seed
    )
    print(f"verify sample={args.sample} seed={args.seed}: {len(problems)} problems")
    for problem in problems:
        print(f"PROBLEM: {problem}")
    if problems:
        return 1
    print("panel rebuilds byte-identical from the manifest")
    return 0


def _cmd_check(args) -> int:
    uni = panel.load_universe(args.universe)
    problems = panel.check_complete(args.panel_dir, uni.universe_version, uni)
    print(f"universe {uni.universe_version}: {len(problems)} coverage gaps")
    for problem in problems:
        print(f"GAP: {problem}")
    if problems:
        return 1
    print("universe fully covered by the manifest")
    return 0


def _cmd_fetch(args) -> int:
    """Journal monthly klines (default 4h + daily) for the universe symbols.
    A month whose monthly bucket is missing (the current tail month is not
    yet published) falls back to the daily bucket per (symbol, day). Idempotent:
    a byte-identical zip is skipped."""
    uni = panel.load_universe(args.universe)
    panel_dir = Path(args.panel_dir)
    symbols = set(args.symbols) if args.symbols else {s["symbol"] for s in uni.symbols}
    intervals = args.intervals or ["4h", "1d"]
    months = [
        m
        for sym in uni.symbols
        if sym["symbol"] in symbols
        for m in panel.month_range(sym["start"][:7], sym["end"][:7])
    ]
    months = sorted(set(months))
    ledger = panel.load_kline_manifest(panel_dir, uni.universe_version)
    new_records: list[dict] = []
    skipped = 0
    for symbol in sorted(symbols):
        for interval in intervals:
            for year_month in months:
                key = (symbol, interval, year_month, None)
                if key in ledger and ledger[key].get("status") == "ok":
                    skipped += 1
                    continue
                try:
                    stats = panel.download_klines(
                        args.base_url, "binance", symbol, interval, year_month
                    )
                    rec = panel.kline_record(symbol, interval, year_month, stats)
                except panel.PanelError:
                    rec = panel.kline_record(symbol, interval, year_month, None)
                new_records.append(rec)
                if rec["status"] != "ok":
                    days = [
                        day
                        for sym in uni.symbols
                        if sym["symbol"] == symbol
                        and sym["start"][:7] <= year_month <= sym["end"][:7]
                        for day in panel.date_range(sym["start"], sym["end"])
                        if day[:7] == year_month
                    ]
                    for day in sorted(set(days)):
                        dkey = (symbol, interval, year_month, day)
                        if dkey in ledger and ledger[dkey].get("status") == "ok":
                            skipped += 1
                            continue
                        try:
                            dstats = panel.fetch_kline(
                                args.base_url, "binance", symbol, day, interval=interval
                            )
                            new_records.append(
                                panel.kline_record(
                                    symbol, interval, year_month, dstats, day=day
                                )
                            )
                        except panel.PanelError:
                            new_records.append(
                                panel.kline_record(
                                    symbol, interval, year_month, None, day=day
                                )
                            )
    panel.append_kline_manifest(panel_dir, uni.universe_version, new_records)
    ok_rows = [r for r in new_records if r["status"] == "ok"]
    missing = [r for r in new_records if r["status"] == "missing"]
    print(
        f"fetched {len(ok_rows)} klines ({','.join(intervals)}), "
        f"{len(missing)} missing on source, {skipped} already in ledger"
    )
    for rec in missing:
        print(f"  MISSING {rec['year_month']} {rec['interval']} {rec['symbol']}")
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="historical research panel (3.1)")
    p.add_argument("--universe", default=str(DEFAULT_UNIVERSE))
    p.add_argument("--panel-dir", default=str(DEFAULT_PANEL))
    p.add_argument("--base-url", default=panel.DEFAULT_BASE_URL)
    sub = p.add_subparsers(dest="command", required=True)
    d = sub.add_parser("download")
    d.add_argument("--symbols", nargs="*")
    d.add_argument("--dates", help="YYYY-MM-DD..YYYY-MM-DD")
    d.set_defaults(fn=_cmd_download)
    v = sub.add_parser("verify")
    v.add_argument("--sample", type=int, required=True)
    v.add_argument("--seed", type=int, default=42)
    v.set_defaults(fn=_cmd_verify)
    sub.add_parser("check").set_defaults(fn=_cmd_check)
    f = sub.add_parser("fetch")
    f.add_argument("--symbols", nargs="*")
    f.add_argument("--intervals", nargs="*", default=None)
    f.set_defaults(fn=_cmd_fetch)
    args = p.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
