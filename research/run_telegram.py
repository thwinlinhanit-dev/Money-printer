"""Telegram bot production entrypoint (spec 044).

Wires `telegram_bot.run_polling` to the REAL Telegram long-poll transport.
Everything below is a thin adapter: the command logic itself lives in
`telegram_bot.py` and is tested hermetically (`tests/test_telegram.py`).

Usage:
    TELEGRAM_BOT_TOKEN=123:abc py -3.13 research/run_telegram.py
    TELEGRAM_BOT_TOKEN=... py -3.13 research/run_telegram.py --assets btc eth

The token comes ONLY from the environment (never committed, never logged).
Insights are composed from the default feature store (data/parquet) and
cached under journal/insights/ exactly like termd's /v1/insight endpoint.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.request
from typing import Any

from insight_composer import InsightComposer, InsightConfig
from telegram_bot import TELEGRAM_API, run_polling


def http_get_updates(url: str, timeout_s: int) -> list[dict[str, Any]]:
    """Long-poll getUpdates via urllib (the only network call in the loop)."""
    with urllib.request.urlopen(url, timeout=timeout_s + 10) as resp:  # noqa: S310
        payload = json.loads(resp.read())
    if not payload.get("ok"):
        raise RuntimeError(f"telegram getUpdates not ok: {payload}")
    return payload.get("result", [])


def http_send_message(token: str):
    """Bind the fixed token into a send_message(chat_id, text) callable."""

    def _send(chat_id: int, text: str) -> None:
        req = urllib.request.Request(  # noqa: S310
            f"{TELEGRAM_API}/bot{token}/sendMessage",
            data=json.dumps({"chat_id": chat_id, "text": text[:4096]}).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=30):  # noqa: S310
            return None

    return _send


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--assets",
        nargs="*",
        default=None,
        help="watched assets for bare /insight (default: InsightConfig underlyings)",
    )
    parser.add_argument(
        "--once",
        action="store_true",
        help="perform ONE long-poll cycle then exit (cron/systemd-timer mode)",
    )
    args = parser.parse_args(argv)

    token = os.environ.get("TELEGRAM_BOT_TOKEN")
    if not token:
        print("run_telegram: TELEGRAM_BOT_TOKEN not set", file=sys.stderr)
        return 2

    composer = InsightComposer(InsightConfig())
    default_assets = args.assets or composer.cfg.underlyings
    print(
        f"run_telegram: polling (assets={','.join(default_assets)}, "
        f"ttl={composer.cfg.cache_ttl_seconds}s, once={args.once})"
    )
    run_polling(
        token,
        composer,
        get_updates=http_get_updates,
        send_message=http_send_message(token),
        default_assets=default_assets,
        max_iterations=1 if args.once else None,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
