"""Telegram bot for on-demand per-token insights (spec 044, scope item).

Commands:
    /insight btc    → latest cached insight for BTC (generated synchronously
                      only when the cache is missing or stale)
    /insight        → insights for every configured underlying

Transport is INJECTED: ``get_updates`` / ``send_message`` are plain callables,
so the whole command path runs hermetically in tests (CONV-23 — no network).
Only this module's ``default_transport`` helpers touch the Telegram HTTP API,
and they exist solely as the production wiring point.
"""

from __future__ import annotations

import json
import time
import urllib.request
from typing import Any, Callable

TELEGRAM_API = "https://api.telegram.org"


# ---------------------------------------------------------------------------
# Command parsing
# ---------------------------------------------------------------------------


def parse_command(text: str) -> tuple[str, list[str]] | None:
    """Parse ``/insight [asset ...]`` (with optional @botname suffix).

    Returns ``(command, args)`` or None when the message is not an
    /insight command.
    """
    if not text or not text.startswith("/"):
        return None
    parts = text.split()
    cmd = parts[0][1:].split("@", 1)[0].lower()
    if cmd != "insight":
        return None
    args = [a.lower() for a in parts[1:] if a]
    return "insight", args


# ---------------------------------------------------------------------------
# Reply formatting
# ---------------------------------------------------------------------------


def format_insight(insight: Any) -> str:
    """One insight → a compact Telegram reply block."""
    d = insight.to_dict() if hasattr(insight, "to_dict") else dict(insight)
    flags = ", ".join(d.get("flags") or []) or "none"
    marker = " ⚠️ stale" if d.get("stale") else ""
    snap = d.get("metrics_snapshot") or {}
    spot = snap.get("spot")
    lines = [f"{d['asset'].upper()} insight{marker} ({d.get('confidence', 'low')}):"]
    if isinstance(spot, (int, float)) and spot > 0:
        lines.append(f"Spot ${spot:,.0f}")
    lines += ["", d.get("summary", "").strip()]
    if flags != "none":
        lines += ["", f"Flags: {flags}"]
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Update handling (hermetic)
# ---------------------------------------------------------------------------


def handle_update(
    update: dict[str, Any],
    composer: Any,
    reply: Callable[[int, str], None],
    default_assets: list[str] | None = None,
) -> bool:
    """Process one Telegram update; calls ``reply(chat_id, text)`` at least
    once for /insight commands. Returns True when the update was handled."""
    msg = update.get("message") or {}
    chat_id = msg.get("chat", {}).get("id")
    parsed = parse_command(msg.get("text", ""))
    if chat_id is None or parsed is None:
        return False
    _, args = parsed
    assets = args or default_assets or []
    if not assets:
        reply(chat_id, "No watched assets configured.")
        return True
    for asset in assets:
        cached = composer.cache.get(asset)
        if cached is None or cached[1]:  # missing or older than TTL
            insight = composer.compose(asset)
        else:
            insight = cached[0]
        reply(chat_id, format_insight(insight))
    return True


def run_polling(
    token: str,
    composer: Any,
    get_updates: Callable[[str, int], list[dict]],
    send_message: Callable[[int, str], None],
    default_assets: list[str] | None = None,
    poll_timeout_s: int = 25,
    max_iterations: int | None = None,
) -> None:
    """Long-poll loop. ``get_updates(token, timeout_s) → updates`` and
    ``send_message(chat_id, text)`` are injected (tests fake them); offsets
    advance per Telegram semantics and failures never kill the loop."""
    offset = 0
    seen: set[int] = set()
    iterations = 0
    while max_iterations is None or iterations < max_iterations:
        iterations += 1
        try:
            updates = get_updates(
                f"{TELEGRAM_API}/bot{token}/getUpdates?timeout={poll_timeout_s}&offset={offset}",
                poll_timeout_s,
            )
        except Exception:  # noqa: BLE001 — a failed poll must not stop the bot
            time.sleep(1)
            continue
        for upd in updates:
            uid = upd.get("update_id")
            if uid is None or uid in seen:
                continue
            seen.add(uid)
            offset = max(offset, uid + 1)
            handle_update(upd, composer, send_message, default_assets)


# ---------------------------------------------------------------------------
# Production transport wiring (the ONLY network-touching code)
# ---------------------------------------------------------------------------


def send_message(token: str, chat_id: int, text: str) -> None:
    """POST one message via the Telegram Bot API (production wiring point)."""
    req = urllib.request.Request(  # noqa: S310 — fixed https API host
        f"{TELEGRAM_API}/bot{token}/sendMessage",
        data=json.dumps({"chat_id": chat_id, "text": text}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30):
        return None
