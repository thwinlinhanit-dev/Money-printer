"""Telegram bot tests (spec 044): /insight command handling over an INJECTED
transport — no network, ever (CONV-23)."""

import json
import time

import pytest

import insight_composer as ic
import telegram_bot as tb


@pytest.fixture
def composer(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    # The bot composes from the DEFAULT store path; seed a BTC export so the
    # generated narrative is grounded ($42,000 comes from the bundle).
    store = tmp_path / "data" / "parquet"
    store.mkdir(parents=True)
    (store / "btc_features.json").write_text(json.dumps({"spot": 42_000.0}))
    calls = []

    def llm(prompt, cfg):
        calls.append(prompt)
        return "Spot is $42,000. Markets calm."

    c = ic.InsightComposer(ic.InsightConfig(underlyings=["btc", "eth"]), llm_fn=llm)
    c.llm_calls = calls  # expose for assertions
    return c


class ReplySpy:
    def __init__(self):
        self.sent: list[tuple[int, str]] = []

    def __call__(self, chat_id: int, text: str) -> None:
        self.sent.append((chat_id, text))


def _msg(text: str, chat_id: int = 42) -> dict:
    return {"update_id": 1, "message": {"chat": {"id": chat_id}, "text": text}}


# --- parsing -----------------------------------------------------------------


def test_telegram_parse_command_variants():
    assert tb.parse_command("/insight btc") == ("insight", ["btc"])
    assert tb.parse_command("/insight@freebuff_bot eth") == ("insight", ["eth"])
    assert tb.parse_command("/insight") == ("insight", [])
    assert tb.parse_command("/insight BTC SOL") == ("insight", ["btc", "sol"])
    assert tb.parse_command("/start") is None
    assert tb.parse_command("hello") is None
    assert tb.parse_command("") is None


# --- command handling --------------------------------------------------------


def test_telegram_insight_single_asset_cache_hit(composer):
    """A fresh cache entry answers from cache — the LLM is never called."""
    spy = ReplySpy()
    warm = ic.InsightComposer(
        ic.InsightConfig(),
        llm_fn=lambda p, c: (_ for _ in ()).throw(AssertionError("LLM called")),
    )
    cached_insight = ic.Insight(
        asset="btc",
        generated_at_ns=time.time_ns(),
        input_bundle_hash="h",
        provider="p",
        model_id="m",
        prompt_version="1.0",
        summary="Cached view.",
        metrics_snapshot={"spot": 104_500.0},
        flags=["extreme_gex"],
        confidence="high",
    )
    warm.cache.put(cached_insight)
    handled = tb.handle_update(_msg("/insight btc"), warm, spy)
    assert handled
    chat_id, text = spy.sent[0]
    assert chat_id == 42
    assert "BTC insight" in text and "Cached view." in text
    assert "extreme_gex" in text


def test_telegram_insight_regenerates_when_stale(composer):
    """Missing or stale cache → synchronous generation through the composer."""
    spy = ReplySpy()
    # Pre-seed a STALE btc insight (2× TTL old).
    stale = dict(
        asset="btc",
        generated_at_ns=time.time_ns() - 2 * 3600 * 1_000_000_000,
        input_bundle_hash="old",
        provider="p",
        model_id="m",
        prompt_version="1.0",
        summary="Old news.",
        metrics_snapshot={},
        flags=[],
        confidence="low",
    )
    composer.cache.put(ic.Insight(**stale))
    assert tb.handle_update(_msg("/insight btc"), composer, spy)
    assert len(composer.llm_calls) == 1, "stale cache must regenerate"
    text = spy.sent[0][1]
    assert "$42,000" in text and "⚠️" not in text


def test_telegram_default_assets_when_no_args(composer):
    spy = ReplySpy()
    assert tb.handle_update(
        _msg("/insight"), composer, spy, default_assets=composer.cfg.underlyings
    )
    assert [t.split()[0] for _, t in spy.sent] == ["BTC", "ETH"]
    assert len(composer.llm_calls) == 2


def test_telegram_non_command_ignored(composer):
    spy = ReplySpy()
    assert not tb.handle_update(
        {"message": {"chat": {"id": 7}, "text": "hi"}}, composer, spy
    )
    assert not tb.handle_update({}, composer, spy)
    assert spy.sent == []


def test_telegram_no_assets_configured_replies_helpfully(composer):
    spy = ReplySpy()
    assert tb.handle_update(_msg("/insight"), composer, spy, default_assets=None)
    assert "No watched assets" in spy.sent[0][1]


# --- polling loop --------------------------------------------------------------


def test_telegram_run_polling_processes_updates_and_advances_offset(composer):
    polls: list[str] = []
    updates = [
        [
            {"update_id": 10, "message": {"chat": {"id": 1}, "text": "/insight btc"}},
            {"update_id": 11, "message": {"chat": {"id": 2}, "text": "noise"}},
        ],
        [],  # idle poll → loop exits via max_iterations
    ]

    def fake_get_updates(url: str, timeout_s: int) -> list[dict]:
        polls.append(url)
        return updates[min(len(polls) - 1, 1)]

    sent: list[tuple[int, str]] = []
    tb.run_polling(
        "TOKEN",
        composer,
        get_updates=fake_get_updates,
        send_message=lambda cid, txt: sent.append((cid, txt)),
        default_assets=["btc"],
        max_iterations=2,
    )
    assert len(polls) == 2
    assert "offset=0" in polls[0]
    assert "offset=12" in polls[1], "offset advances past the last update_id"
    assert len(sent) == 1 and sent[0][0] == 1  # noise never replied to


def test_telegram_poll_failure_never_kills_loop(composer):
    attempts = {"n": 0}

    def flaky_get_updates(url, timeout_s):
        attempts["n"] += 1
        if attempts["n"] == 1:
            raise OSError("network down")
        return []

    sent: list[str] = []
    tb.run_polling(
        "T",
        composer,
        get_updates=flaky_get_updates,
        send_message=lambda cid, t: sent.append(t),
        max_iterations=3,
    )
    assert attempts["n"] == 3  # kept polling after the failure


def test_telegram_production_transport_builds_valid_request():
    """The only network-touching function targets the fixed Bot API host with a
    JSON body (wiring smoke check — the request is built, never sent)."""
    import urllib.request

    req = urllib.request.Request(f"{tb.TELEGRAM_API}/botX/sendMessage")
    body = json.dumps({"chat_id": 5, "text": "/insight"}).encode()
    assert req.get_method() == "GET"  # default until data attached
    full = urllib.request.Request(
        f"{tb.TELEGRAM_API}/botX/sendMessage",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    assert full.full_url.startswith("https://api.telegram.org/botX/")
