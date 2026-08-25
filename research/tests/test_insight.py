"""Per-token AI insight tests (spec 044, TOK-1..TOK-10).

Hermetic: the LLM is always an injected callable; the feature store is a JSON
export in a tmp dir; the cache/archive live under monkeypatch.chdir(tmp_path).
The REST-layer staleness contract (tok_5) exercises termd._insight_payload.
"""

import json
import time
from pathlib import Path

import pytest

import insight_composer as ic


@pytest.fixture
def workspace(tmp_path, monkeypatch):
    """Isolated CWD (cache/journal land here) + a BTC feature export."""
    monkeypatch.chdir(tmp_path)
    data_dir = tmp_path / "exports"
    data_dir.mkdir()
    metrics = {
        "spot": 104500.0,
        "change_24h": 1.25,
        "iv_atm": 0.482,
        "iv_regime": "Rich",
        "vrp": 0.03,
        "net_gex": -12_500_000.0,
        "max_pain": 100_000.0,
        "flow_npf": 3_200_000.0,
        "flow_block_count": 7,
        "flow_block_usd": 950_000.0,
        "flow_net_delta": 210_000.0,
        "call_put_ratio": 1.2,
        "flow_whale_net": 150_000.0,
        "smart_net_delta": 850_000.0,
        "whale_oi_ratio": 0.61,
        "concentration": 0.31,
        "netflow_velocity": -25_000_000.0,
        "netflow_regime": "Outflow",
        "netflow_zscore": -2.4,
        "regime_trend": "Trend",
        "regime_vol": "High",
        "mean_abs_gex_7d": 5_000_000.0,
        "smart_flow_3d": 400_000.0,
        "netflow_velocity_std": 10_000_000.0,
    }
    (data_dir / "btc_features.json").write_text(json.dumps(metrics))
    return {"root": tmp_path, "data_dir": str(data_dir), "metrics": metrics}


# ---------------------------------------------------------------- tok_1 -----


def test_tok_1_composer_reads_only_feature_exports(workspace):
    """Metrics come exclusively from the structured export; the module has no
    network surface at all."""
    seen_prompts = []

    def llm(prompt, cfg):
        seen_prompts.append(prompt)
        return "No numbers here."

    insight = ic.InsightComposer(ic.InsightConfig(), llm_fn=llm).compose(
        "btc", data_dir=workspace["data_dir"]
    )
    assert insight.metrics_snapshot["spot"] == 104_500.0
    assert insight.metrics_snapshot["iv_regime"] == "Rich"
    # The prompt was rendered FROM those metrics (no other data entered).
    assert "104,500.00" in seen_prompts[0]

    src = Path(ic.__file__).read_text()
    for forbidden in ["requests", "urllib", "http.client", "socket", "urlopen"]:
        assert forbidden not in src, f"TOK-1: composer must never import {forbidden}"


# ---------------------------------------------------------------- tok_2 -----


def test_tok_2_input_bundle_hash_is_deterministic(workspace):
    cfg = ic.InsightConfig()
    m = workspace["metrics"]
    h1 = ic.compute_bundle_hash(m, "prompt-a", cfg)
    h2 = ic.compute_bundle_hash(dict(m), "prompt-a", cfg)
    assert h1 == h2, "same bundle → same hash"
    assert h1 != ic.compute_bundle_hash({**m, "spot": 1.0}, "prompt-a", cfg)
    assert h1 != ic.compute_bundle_hash(m, "prompt-b", cfg)


# ---------------------------------------------------------------- tok_3 -----


def test_tok_3_flags_computed_deterministically(workspace):
    m = {
        **workspace["metrics"],
        "net_gex": -30_000_000.0,  # |gex| > 2 × mean_abs_gex_7d
        "smart_flow_3d": 400_000.0,  # > 0 → accumulating
        "netflow_velocity": 300_000.0,  # z=+3σ → exchange_inflow (+divergence)
        "netflow_velocity_std": 100_000.0,
        "whale_oi_ratio": 0.61,  # > 0.6 → dominance
        "spot": 100_000.0,
        "max_pain": 99_900.0,  # |Δ|/spot < 2% → proximity
        "iv_regime": "Rich",
    }
    cfg = ic.InsightConfig()
    flags_a = ic.compute_flags(m, cfg)
    flags_b = ic.compute_flags(json.loads(json.dumps(m)), cfg)
    assert flags_a.to_list() == flags_b.to_list(), "golden determinism"
    assert flags_a.to_list() == [
        "extreme_gex",
        "smart_money_accumulating",
        "exchange_inflow",
        "iv_extreme",
        "whale_dominance",
        "flow_divergence",
        "max_pain_proximity",
    ]


# ---------------------------------------------------------------- tok_4 -----


def test_tok_4_verify_grounded_catches_ungrounded_numbers(workspace):
    m = {"spot": 104_500.0, "net_gex": -12_500_000.0}
    ok, ungrounded = ic.verify_grounded("Spot is $104,500 while GEX is -12500000.", m)
    assert ok and ungrounded == []
    ok, ungrounded = ic.verify_grounded(
        "Spot is $104,500 and open interest just hit 777,777.", m
    )
    assert not ok
    assert any("777777" in u.replace(",", "") for u in ungrounded)


# ---------------------------------------------------------------- tok_5 -----


def test_tok_5_rest_returns_cached_with_staleness(workspace):
    import termd

    cfg = ic.InsightConfig(cache_ttl_seconds=3600)
    fresh = ic.Insight(
        asset="btc",
        generated_at_ns=time.time_ns(),
        input_bundle_hash="h",
        provider="anthropic",
        model_id="m",
        prompt_version="1.0",
        summary="cached summary",
        metrics_snapshot={"spot": 1.0},
        flags=[],
        confidence="high",
    )
    cache = ic.InsightCache(ttl_seconds=cfg.cache_ttl_seconds)
    cache.put(fresh)

    out = termd._insight_payload("btc")
    assert out["cached"] is True and out["stale"] is False
    assert out["age_seconds"] < 5

    aged = dict(fresh.to_dict())
    aged["generated_at_ns"] = time.time_ns() - 2 * 3600 * 1_000_000_000
    cache.put(ic.Insight(**aged))
    out = termd._insight_payload("btc")
    assert out["cached"] is True and out["stale"] is True
    assert out["age_seconds"] >= 3600


# ---------------------------------------------------------------- tok_6 -----


def test_tok_6_metrics_snapshot_included_in_output(workspace):
    insight = ic.InsightComposer(
        ic.InsightConfig(), llm_fn=lambda p, c: "Narrative only."
    ).compose("btc", data_dir=workspace["data_dir"])
    d = insight.to_dict()
    assert d["metrics_snapshot"]["net_gex"] == workspace["metrics"]["net_gex"]
    assert d["summary"], "narrative present alongside the numbers"


# ---------------------------------------------------------------- tok_7 -----


def test_tok_7_llm_failure_returns_stale_cache(workspace):
    def boom(prompt, cfg):
        raise RuntimeError("rate limited")

    composer = ic.InsightComposer(ic.InsightConfig(), llm_fn=boom)

    # No cache yet → explicit degraded fallback, never fabricated content.
    fallback = composer.compose("eth", data_dir=workspace["data_dir"])
    assert fallback.stale is True and fallback.confidence == "low"
    assert "unavailable" in fallback.summary.lower()

    # With a warm cache → the LAST GOOD insight is served back marked stale.
    good = ic.InsightComposer(
        ic.InsightConfig(), llm_fn=lambda p, c: "All calm."
    ).compose("btc", data_dir=workspace["data_dir"])
    again = composer.compose("btc", data_dir=workspace["data_dir"])
    assert again.stale is True
    assert again.summary == good.summary
    assert again.input_bundle_hash == good.input_bundle_hash


# ---------------------------------------------------------------- tok_8 -----


def test_tok_8_check_config_rejects_unknown_fields():
    with pytest.raises(TypeError):
        ic.InsightConfig(not_a_real_field=1)


# ---------------------------------------------------------------- tok_9 -----


def test_tok_9_archive_is_latest_wins_per_day(workspace):
    def mk(ns, marker):
        return ic.Insight(
            asset="btc",
            generated_at_ns=ns,
            input_bundle_hash=f"h-{marker}",
            provider="p",
            model_id="m",
            prompt_version="1.0",
            summary=f"v{marker}",
            metrics_snapshot={},
            flags=[],
            confidence="low",
        )

    day_start = 1_785_000_000_000_000_000  # fixed UTC instant, one calendar day
    composer = ic.InsightComposer(ic.InsightConfig())
    for ns, marker in [(day_start, 1), (day_start + 3600 * 1_000_000_000, 2)]:
        ins = mk(ns, marker)
        composer.cache.put(ins)
        composer.cache.archive(ins)

    asset_dir = workspace["root"] / "journal" / "insights" / "btc"
    daily = [p for p in asset_dir.glob("*.json") if p.name != "latest.json"]
    assert len(daily) == 1, "same-day re-runs overwrite, never duplicate"
    body = json.loads(daily[0].read_text())
    assert body["input_bundle_hash"] == "h-2", "latest wins"
    assert (
        json.loads((asset_dir / "latest.json").read_text())["input_bundle_hash"]
        == "h-2"
    )


# --------------------------------------------------------------- tok_10 -----


def test_tok_10_grounding_contract_end_to_end(workspace):
    """exports → flags → versioned prompt → LLM(mock) → verify_grounded →
    archive, with the bundle hash pinned end-to-end."""
    m = workspace["metrics"]

    def grounded_llm(prompt, cfg):
        assert "# Token Insight: BTC" in prompt
        assert "Flags" in prompt
        return (
            f"Spot prints ${m['spot']:,.0f} with ATM IV at "
            f"{m['iv_atm']:.1%}; GEX is {m['net_gex']:,.0f}."
        )

    composer = ic.InsightComposer(ic.InsightConfig(), llm_fn=grounded_llm)
    insight = composer.compose("btc", data_dir=workspace["data_dir"])

    assert insight.flags  # deterministic pre-LLM flags travelled with the bundle
    ok, ungrounded = ic.verify_grounded(insight.summary, insight.metrics_snapshot)
    assert ok, f"shipped narrative must be grounded, offenders={ungrounded}"
    expected_hash = ic.compute_bundle_hash(
        insight.metrics_snapshot,
        ic.render_prompt(
            "btc",
            insight.metrics_snapshot,
            ic.compute_flags(insight.metrics_snapshot, composer.cfg),
        ),
        composer.cfg,
    )
    assert insight.input_bundle_hash == expected_hash
    archived = json.loads(
        (workspace["root"] / "journal" / "insights" / "btc" / "latest.json").read_text()
    )
    assert archived["input_bundle_hash"] == insight.input_bundle_hash
    assert archived["provider"] == composer.cfg.provider

    # Ungrounded LLM output is rejected before shipping (TOK-4).
    bad = ic.InsightComposer(
        ic.InsightConfig(),
        llm_fn=lambda p, c: "Liquidity of 55,555,555 USDT appeared.",
    ).compose("sol", data_dir=workspace["data_dir"])
    assert bad.summary.startswith("[Grounding warning:")
