"""Executable deterministic daily-brief job (RES-5/6/7).

The input JSON is the complete, auditable market-data bundle for the run.  It
is stored verbatim in the archive record with the prompt/model/validation
metadata; prose is human-readable only and never feeds an order path.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from brief import ArchiveRecord, InputBundle, archive_brief, generate_or_alert


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="render and archive a grounded daily brief")
    parser.add_argument("--input", required=True, type=Path, help="structured market-data JSON bundle")
    parser.add_argument("--archive-dir", type=Path, default=Path("journal/briefs"))
    parser.add_argument("--model", default=os.environ.get("MP_BRIEF_MODEL", "deterministic-template-v1"))
    parser.add_argument("--prompt-version", default="brief-v1")
    args = parser.parse_args(argv)

    try:
        inputs = json.loads(args.input.read_text(encoding="utf-8"))
        if not isinstance(inputs, dict):
            raise ValueError("input bundle must be a JSON object")
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"P3: brief input unavailable or invalid: {error}", file=sys.stderr)
        return 2

    output, alert = generate_or_alert(inputs, args.model, args.prompt_version)
    if alert or output is None:
        print(alert or "P3: brief generation failed", file=sys.stderr)
        return 2
    bundle = InputBundle.of(inputs)
    record = ArchiveRecord(
        bundle_hash=bundle.hash,
        prompt_version=args.prompt_version,
        model_id=args.model,
        output=output,
        input_bundle=bundle.canonical,
        validation_result="grounded",
    )
    name = datetime.now(timezone.utc).strftime("%Y-%m-%d.jsonl")
    archive_brief(args.archive_dir, record, name=name)
    print(output, end="")
    return 0


if __name__ == "__main__":  # pragma: no cover - CLI boundary
    raise SystemExit(main())
