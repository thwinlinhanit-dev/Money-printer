"""Executable observation-store grading job (spec 054 REL-34).

Points the Python grading arm at the SAME Parquet observation store the Rust
engine writes — one artifact store, two consumers. Grades one identity (by
fingerprint) or every identity present, at the engine's horizons, on NET
outcomes (R-4). A store the reader cannot verify (schema drift, fingerprint
mismatch, identity mixing) is a failed job, not an optimistic grade.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from grading_job import (
    DEFAULT_HORIZONS_NS,
    run_all_observation_grading,
    run_observation_grading,
)
from observation_store import ObservationStoreError


def _horizons(raw: str | None) -> list[int]:
    if not raw:
        return list(DEFAULT_HORIZONS_NS)
    out: list[int] = []
    for tok in raw.split(","):
        t = tok.strip()
        out.append(int(t) if t.isdigit() else int(float(t) * 1_000_000_000))
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="grade observation-store identities (REL-34; net outcomes, R-4)"
    )
    parser.add_argument(
        "--obs-dir",
        type=Path,
        default=Path("data/observations"),
        help="observation store root (contains observations/<fingerprint>/)",
    )
    parser.add_argument(
        "--fingerprint",
        default=None,
        help="one identity fingerprint; omit to grade every identity present",
    )
    parser.add_argument("--out-dir", type=Path, default=Path("research/grades"))
    parser.add_argument(
        "--horizons",
        default=None,
        help="comma list of ns or 'm'-style tokens via float hours, e.g. 900e9 or 0.25,1,4,24 (hours)",
    )
    args = parser.parse_args(argv)
    try:
        horizons = _horizons(args.horizons)
        if args.fingerprint:
            path, ran = run_observation_grading(
                args.obs_dir, args.fingerprint, args.out_dir, horizons
            )
            results = [(args.fingerprint, path, ran)]
        else:
            results = run_all_observation_grading(args.obs_dir, args.out_dir, horizons)
    except (ObservationStoreError, ValueError) as e:
        print(f"observation grading refused: {e}", file=sys.stderr)
        return 1
    for fp, path, ran in results:
        print(f"{fp}: {'graded' if ran else 'already graded (no-op)'} -> {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
