"""
Insight Composer (spec 044, TOK). Generates per-token AI summaries grounded
in feature exports from the Freebuff feature store.

Reads structured metrics (options Greeks, IV surface, options flow, whale
positioning, CEX flow velocity) and produces a 5-8 sentence narrative via
LLM with strict grounding (spec 010 RES-5/6).

No external data, no web fetches, no social media — only feature store
exports and the grounding contract.
"""

import hashlib
import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


# ---------------------------------------------------------------------------
# Config (TOK-8: deny_unknown_fields via dataclass with no extra='ignore')
# ---------------------------------------------------------------------------


@dataclass
class InsightConfig:
    """Per-token insight generation config (spec 044 TOK-8)."""

    underlyings: list[str] = field(default_factory=lambda: ["btc", "eth", "sol"])
    provider: str = "anthropic"
    model_id: str = "claude-opus-4-8"
    prompt_version: str = "1.0"
    cache_ttl_seconds: int = 3600
    max_tokens: int = 500
    # Flag thresholds (overridable)
    extreme_gex_multiplier: float = 2.0
    smart_flow_consecutive_days: int = 3
    exchange_velocity_sigma: float = 2.0
    max_pain_proximity_pct: float = 0.02
    whale_dominance_threshold: float = 0.6


# ---------------------------------------------------------------------------
# Pre-LLM flags (TOK-3: deterministic from metrics + config)
# ---------------------------------------------------------------------------


@dataclass
class InsightFlags:
    """Boolean flags computed deterministically from metrics (TOK-3)."""

    extreme_gex: bool = False
    smart_money_accumulating: bool = False
    smart_money_distribution: bool = False
    exchange_outflow: bool = False
    exchange_inflow: bool = False
    iv_extreme: bool = False
    whale_dominance: bool = False
    flow_divergence: bool = False
    max_pain_proximity: bool = False

    def to_list(self) -> list[str]:
        """Return list of active flag names."""
        flags = []
        for fld in [
            "extreme_gex",
            "smart_money_accumulating",
            "smart_money_distribution",
            "exchange_outflow",
            "exchange_inflow",
            "iv_extreme",
            "whale_dominance",
            "flow_divergence",
            "max_pain_proximity",
        ]:
            if getattr(self, fld):
                flags.append(fld)
        return flags

    def score(self) -> float:
        """0-1 confidence based on flag count (TOK-3)."""
        active = len(self.to_list())
        return min(1.0, active / 5.0)


def compute_flags(metrics: dict, cfg: InsightConfig) -> InsightFlags:
    """
    Compute boolean flags from metrics snapshot deterministically (TOK-3).
    All inputs come from the feature store; no randomness, no I/O.
    """
    flags = InsightFlags()
    net_gex = metrics.get("net_gex", 0.0)
    abs_gex = abs(net_gex)
    mean_abs_gex = metrics.get("mean_abs_gex_7d", abs_gex)
    if mean_abs_gex > 0 and abs_gex > cfg.extreme_gex_multiplier * mean_abs_gex:
        flags.extreme_gex = True

    smart_flow = metrics.get("smart_flow_3d", 0.0)
    if smart_flow > 0:
        flags.smart_money_accumulating = True
    elif smart_flow < 0:
        flags.smart_money_distribution = True

    velocity = metrics.get("netflow_velocity", 0.0)
    velocity_std = metrics.get("netflow_velocity_std", 1.0)
    if velocity_std > 0:
        z = velocity / velocity_std
        if z < -cfg.exchange_velocity_sigma:
            flags.exchange_outflow = True
        elif z > cfg.exchange_velocity_sigma:
            flags.exchange_inflow = True

    iv_regime = metrics.get("iv_regime", "")
    if iv_regime in ("Rich", "Cheap"):
        flags.iv_extreme = True

    whale_ratio = metrics.get("whale_oi_ratio", 0.0)
    if whale_ratio > cfg.whale_dominance_threshold:
        flags.whale_dominance = True

    if flags.smart_money_accumulating and flags.exchange_inflow:
        flags.flow_divergence = True

    spot = metrics.get("spot", 0.0)
    max_pain = metrics.get("max_pain", 0.0)
    if spot > 0:
        proximity = abs(spot - max_pain) / spot
        if proximity < cfg.max_pain_proximity_pct:
            flags.max_pain_proximity = True

    return flags


# ---------------------------------------------------------------------------
# Prompt template (TOK-2: versioned, spec 010 RES-8)
# ---------------------------------------------------------------------------

PROMPT_TEMPLATE_V1 = """\
# Token Insight: {asset}

## Current State
- Spot: ${spot:,.2f} | 24h change: {change_24h:+.2f}%
- ATM IV: {iv_atm:.1%} | IV Regime: {iv_regime} | VRP: {vrp:+.2%}
- Net GEX: {net_gex:+,.0f} | Max Pain: ${max_pain:,.0f}

## Options Activity
- Net premium flow (24h): {flow_npf:+,.0f}
- Block trades: {flow_block_count} ({flow_block_usd:+,.0f})
- Net delta-adjusted flow: {flow_net_delta:+,.0f}
- Call/Put ratio: {call_put_ratio:.2f}
- Whale net flow: {flow_whale_net:+,.0f}

## Whale Positioning
- Smart Money net delta: {smart_net_delta:+,.0f}
- Whale OI share: {whale_oi_ratio:.1%}
- Cohort concentration (HHI): {concentration:.3f}

## Exchange Flows
- Netflow velocity (24h): {netflow_velocity:+,.0f} USDT/day
- Flow regime: {netflow_regime}
- Flow z-score: {netflow_zscore:+.2f}

## Regime
- Trend: {regime_trend} | Vol: {regime_vol}

## Flags
{flags_text}

---

Summarize what these metrics mean for {asset} in 5-8 sentences.
Focus on: (1) what's unusual or extreme, (2) what the options positioning
implies for near-term direction, (3) what whale/flow activity suggests.
Use specific numbers from the data above. Do not recommend trades.
Mark any uncertainty explicitly.
"""


def render_prompt(asset: str, metrics: dict, flags: InsightFlags) -> str:
    """Render the insight prompt from metrics + flags (TOK-2, TOK-3)."""
    flags_text = "\n".join(f"- **{f}**" for f in flags.to_list()) or "- (none)"
    return PROMPT_TEMPLATE_V1.format(
        asset=asset.upper(),
        flags_text=flags_text,
        **{
            k: metrics.get(k, 0)
            for k in [
                "spot",
                "change_24h",
                "iv_atm",
                "iv_regime",
                "vrp",
                "net_gex",
                "max_pain",
                "flow_npf",
                "flow_block_count",
                "flow_block_usd",
                "flow_net_delta",
                "call_put_ratio",
                "flow_whale_net",
                "smart_net_delta",
                "whale_oi_ratio",
                "concentration",
                "netflow_velocity",
                "netflow_regime",
                "netflow_zscore",
                "regime_trend",
                "regime_vol",
            ]
        },
    )


# ---------------------------------------------------------------------------
# InputBundle + grounding (TOK-2: spec 010 RES-6)
# ---------------------------------------------------------------------------


def compute_bundle_hash(metrics: dict, prompt: str, cfg: InsightConfig) -> str:
    """Deterministic hash of the InputBundle (TOK-2, RES-6)."""
    canonical = json.dumps(
        {
            "metrics": metrics,
            "prompt": prompt,
            "provider": cfg.provider,
            "model_id": cfg.model_id,
            "prompt_version": cfg.prompt_version,
        },
        sort_keys=True,
        ensure_ascii=True,
    )
    return hashlib.sha256(canonical.encode()).hexdigest()[:16]


def verify_grounded(summary: str, metrics: dict) -> tuple[bool, list[str]]:
    """
    Verify every numeric claim in the summary appears in the input bundle
    (TOK-4, RES-5/6). Returns (is_grounded, list_of_ungrounded_numbers).

    Grounding rules:
      - signed numbers ground against signed metric values (a claim of
        ``-12,500,000`` IS grounded when the bundle carries -12500000);
      - a number immediately followed by ``%`` grounds against value x 100
        of any bundle metric (the prompt template renders IV/VRP as
        percentages, so a faithful summary does too).
    """
    import re

    metric_values = [float(v) for v in metrics.values() if isinstance(v, (int, float))]

    def _grounds(num: float, as_pct: bool) -> bool:
        for mv in metric_values:
            if abs(num - mv) / max(abs(mv), 1e-9) < 0.01:
                return True
            if as_pct:
                scaled = mv * 100.0
                if abs(num - scaled) / max(abs(scaled), 1e-9) < 0.01:
                    return True
        return False

    ungrounded = []
    for match in re.finditer(r"(-?[\d,]+\.?\d*)(%?)", summary):
        raw, pct = match.group(1), match.group(2)
        clean = raw.replace("$", "").replace(",", "")
        try:
            num = float(clean)
        except ValueError:
            continue
        if not _grounds(num, pct == "%"):
            ungrounded.append(raw)

    return len(ungrounded) == 0, ungrounded


# ---------------------------------------------------------------------------
# Insight output structure (TOK-6)
# ---------------------------------------------------------------------------


@dataclass
class Insight:
    """A generated per-token insight (TOK-6: metrics alongside narrative)."""

    asset: str
    generated_at_ns: int
    input_bundle_hash: str
    provider: str
    model_id: str
    prompt_version: str
    summary: str
    metrics_snapshot: dict
    flags: list[str]
    confidence: str  # "low" | "medium" | "high"
    stale: bool = False

    def to_dict(self) -> dict:
        return {
            "asset": self.asset,
            "generated_at_ns": self.generated_at_ns,
            "input_bundle_hash": self.input_bundle_hash,
            "provider": self.provider,
            "model_id": self.model_id,
            "prompt_version": self.prompt_version,
            "summary": self.summary,
            "metrics_snapshot": self.metrics_snapshot,
            "flags": self.flags,
            "confidence": self.confidence,
            "stale": self.stale,
        }


# ---------------------------------------------------------------------------
# Cache layer (TOK-5: TTL-based caching)
# ---------------------------------------------------------------------------


class InsightCache:
    """Simple file-based insight cache with TTL (TOK-5, TOK-9)."""

    def __init__(self, journal_dir: str = "journal/insights", ttl_seconds: int = 3600):
        self.journal_dir = Path(journal_dir)
        self.ttl_seconds = ttl_seconds

    def _path(self, asset: str) -> Path:
        return self.journal_dir / asset / "latest.json"

    def get(self, asset: str) -> Optional[tuple[Insight, bool]]:
        """Return (insight, is_stale). None if no cache entry."""
        path = self._path(asset)
        if not path.exists():
            return None
        try:
            data = json.loads(path.read_text())
            insight = Insight(**{k: v for k, v in data.items() if k != "stale"})
            age_ns = time.time_ns() - insight.generated_at_ns
            age_s = age_ns / 1_000_000_000
            stale = age_s > self.ttl_seconds
            insight.stale = stale
            return insight, stale
        except (json.JSONDecodeError, KeyError, TypeError):
            return None

    def put(self, insight: Insight) -> None:
        """Cache an insight (TOK-9: latest wins, not append)."""
        path = self._path(insight.asset)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(insight.to_dict(), indent=2, ensure_ascii=False))

    def archive(self, insight: Insight) -> None:
        """Archive to daily file (TOK-9: append-only, latest wins per day)."""
        from datetime import datetime, timezone

        dt = datetime.fromtimestamp(insight.generated_at_ns / 1e9, tz=timezone.utc)
        day_str = dt.strftime("%Y-%m-%d")
        archive_path = self.journal_dir / insight.asset / f"{day_str}.json"
        archive_path.parent.mkdir(parents=True, exist_ok=True)
        archive_path.write_text(
            json.dumps(insight.to_dict(), indent=2, ensure_ascii=False)
        )


# ---------------------------------------------------------------------------
# Feature store reader (TOK-1: read only from structured exports, no I/O)
# ---------------------------------------------------------------------------


def read_feature_store(asset: str, data_dir: str = "data/parquet") -> dict:
    """
    Read the latest feature values for an asset from Parquet/JSON exports.
    Returns a flat metrics dictionary.

    TOK-1: Only reads from structured feature exports, no external data.
    """
    metrics = {
        "spot": 0.0,
        "change_24h": 0.0,
        "iv_atm": 0.0,
        "iv_regime": "Fair",
        "vrp": 0.0,
        "net_gex": 0.0,
        "max_pain": 0.0,
        "flow_npf": 0.0,
        "flow_block_count": 0,
        "flow_block_usd": 0.0,
        "flow_net_delta": 0.0,
        "call_put_ratio": 1.0,
        "flow_whale_net": 0.0,
        "smart_net_delta": 0.0,
        "whale_oi_ratio": 0.0,
        "concentration": 0.0,
        "netflow_velocity": 0.0,
        "netflow_regime": "Neutral",
        "netflow_zscore": 0.0,
        "regime_trend": "Chop",
        "regime_vol": "Mid",
        "mean_abs_gex_7d": 0.0,
        "smart_flow_3d": 0.0,
        "netflow_velocity_std": 1.0,
    }
    # Try to read from JSON feature export if available
    export_path = Path(data_dir) / f"{asset}_features.json"
    if export_path.exists():
        try:
            exported = json.loads(export_path.read_text())
            metrics.update(exported)
        except (json.JSONDecodeError, TypeError):
            pass
    return metrics


# ---------------------------------------------------------------------------
# Composer — the main pipeline (TOK-1 through TOK-10)
# ---------------------------------------------------------------------------


class InsightComposer:
    """
    Per-token insight composer (spec 044). Reads feature exports, computes
    flags, renders prompt, and generates grounded insight.

    The LLM call is abstracted via a callback to support multiple providers
    (mp-llm Rust crate or Python LLM clients).
    """

    def __init__(self, cfg: InsightConfig, llm_fn=None):
        self.cfg = cfg
        self.llm_fn = llm_fn or self._mock_llm
        self.cache = InsightCache(ttl_seconds=cfg.cache_ttl_seconds)

    def _mock_llm(self, prompt: str, cfg: InsightConfig) -> str:
        """Fallback mock LLM for testing without API keys."""
        return (
            "This is a placeholder insight. The LLM provider is not configured. "
            "Set up an API key or provide a custom llm_fn to the InsightComposer."
        )

    def compose(self, asset: str, data_dir: str = "data/parquet") -> Insight:
        """
        Generate an insight for one asset (TOK-1 through TOK-7).

        Reads from feature store, computes flags, renders prompt, calls LLM,
        verifies grounding (TOK-4), and caches result (TOK-5).
        """
        # TOK-1: Read only from feature exports
        metrics = read_feature_store(asset, data_dir)

        # TOK-3: Compute flags deterministically
        flags = compute_flags(metrics, self.cfg)

        # TOK-2: Render versioned prompt
        prompt = render_prompt(asset, metrics, flags)

        # TOK-2: Compute InputBundle hash
        bundle_hash = compute_bundle_hash(metrics, prompt, self.cfg)

        # Call LLM
        try:
            summary = self.llm_fn(prompt, self.cfg)
        except Exception:
            # TOK-7: On LLM failure, return stale cache
            cached = self.cache.get(asset)
            if cached:
                insight, _ = cached
                insight.stale = True
                return insight
            return Insight(
                asset=asset,
                generated_at_ns=time.time_ns(),
                input_bundle_hash=bundle_hash,
                provider=self.cfg.provider,
                model_id=self.cfg.model_id,
                prompt_version=self.cfg.prompt_version,
                summary="LLM unavailable. No cached insight.",
                metrics_snapshot=metrics,
                flags=flags.to_list(),
                confidence="low",
                stale=True,
            )

        # TOK-4: Verify grounding
        is_grounded, ungrounded = verify_grounded(summary, metrics)
        if not is_grounded:
            # TOK-4: ungrounded numbers → reject
            summary = (
                f"[Grounding warning: {len(ungrounded)} ungrounded numbers detected. "
                f"Showing metrics only.]\n"
                + "\n".join(f"- {k}: {v}" for k, v in metrics.items())
            )

        # TOK-6: metrics_snapshot alongside narrative
        insight = Insight(
            asset=asset,
            generated_at_ns=time.time_ns(),
            input_bundle_hash=bundle_hash,
            provider=self.cfg.provider,
            model_id=self.cfg.model_id,
            prompt_version=self.cfg.prompt_version,
            summary=summary,
            metrics_snapshot=metrics,
            flags=flags.to_list(),
            confidence="high"
            if len(flags.to_list()) >= 3
            else "medium"
            if flags.to_list()
            else "low",
        )

        # TOK-5: Cache + TOK-9: Archive
        self.cache.put(insight)
        self.cache.archive(insight)

        return insight
