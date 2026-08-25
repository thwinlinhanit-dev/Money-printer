#!/usr/bin/env python3
"""
Runner for per-token AI insight generation (spec 044).

Usage:
    python run_insight.py                    # Generate insights for all underlyings
    python run_insight.py --asset btc        # Generate for BTC only
    python run_insight.py --schedule weekly   # Run weekly cron
"""

import argparse
import sys
from pathlib import Path

# Add parent to path for module imports
sys.path.insert(0, str(Path(__file__).parent))

from insight_composer import InsightComposer, InsightConfig


def main():
    parser = argparse.ArgumentParser(
        description="Per-token AI insight generator (spec 044)"
    )
    parser.add_argument(
        "--asset", type=str, default=None, help="Generate for specific asset"
    )
    parser.add_argument(
        "--data-dir",
        type=str,
        default="data/parquet",
        help="Feature store data directory",
    )
    parser.add_argument(
        "--provider", type=str, default="anthropic", help="LLM provider"
    )
    parser.add_argument("--model", type=str, default="claude-opus-4-8", help="Model ID")
    args = parser.parse_args()

    cfg = InsightConfig(provider=args.provider, model_id=args.model)
    composer = InsightComposer(cfg)

    assets = [args.asset] if args.asset else cfg.underlyings

    for asset in assets:
        print(f"Generating insight for {asset.upper()}...")
        insight = composer.compose(asset, data_dir=args.data_dir)
        print(f"  Confidence: {insight.confidence}")
        print(f"  Flags: {', '.join(insight.flags) or '(none)'}")
        print(f"  Summary: {insight.summary[:200]}...")
        print()

    print("Insights cached to journal/insights/")


if __name__ == "__main__":
    main()
