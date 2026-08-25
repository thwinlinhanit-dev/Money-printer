# 044 — Per-Token AI Insight Agent

## Purpose

Generate AI-powered token summaries that synthesize options analytics (specs
037–040), whale positioning (specs 028/042), exchange flows (specs 034/043),
and market regime into trader-friendly narrative insights — the "AI Insight"
feature from iCrypto.ai, grounded in Freebuff's own recorded data with the
strict grounding contract from spec 010.

The agent reads structured feature exports and produces a 5–10 line summary:
what the data says, what's unusual, and what to watch. It never fabricates
numbers, never recommends trades, and never fetches external data.

## Scope

**In:** A new agent job in the research pipeline that generates per-token
insights, REST API endpoint for the analytics terminal (spec 041), Telegram
bot integration for on-demand queries, weekly scheduled insights for watched
assets.

**Out:** Trading recommendations, autonomous execution, external data fetch
(news/social — spec 010 decision), portfolio advice, real-time streaming of
AI output (batch only in v1).

## Design

### Architecture

```
Feature Store (Parquet) + Live Feature Engine
  │  Per asset: options Greeks, IV surface, flow, whale net, netflow velocity
  ▼
Insight Composer (Python, research/)
  │  Gather structured metrics into an InputBundle (spec 010 RES-6)
  │  Render into the prompt template (spec 010 RES-8)
  ▼
LLM Provider (mp-llm, spec 010)
  │  Claude/GPT/Gemini (configurable, same grounding contract)
  ▼
Insight Output
  ├──▶ REST API: GET /v1/insight?asset=btc  (JSON response)
  ├──▶ Terminal: cached in localStorage, refreshed daily
  ├──▶ Telegram: /insight btc command
  └──▶ Archive: journal/insights/{asset}/{date}.json (append-only, W-6)
```

### Insight Prompt Template

```markdown
# Token Insight: {asset}

## Current State
- Spot: ${spot} | 24h change: {change_24h}%
- ATM IV: {iv_atm}% | IV Regime: {iv_regime} | VRP: {vrp}%
- Net GEX: {net_gex} | Max Pain: ${max_pain}

## Options Activity
- Net premium flow (24h): {flow_npf}
- Block trades: {flow_block_count} ({flow_block_usd})
- Net delta-adjusted flow: {flow_net_delta}
- Call/Put ratio: {call_put_ratio}
- Whale net flow: {flow_whale_net}

## Whale Positioning
- Smart Money net delta: {smart_net_delta}
- Whale OI share: {whale_oi_ratio}%
- Cohort concentration (HHI): {concentration}

## Exchange Flows
- Netflow velocity (24h): {netflow_velocity} USDT/day
- Flow regime: {netflow_regime}
- Flow z-score: {netflow_zscore}

## Regime
- Trend: {regime_trend} | Vol: {regime_vol}

---

Summarize what these metrics mean for {asset} in 5-8 sentences.
Focus on: (1) what's unusual or extreme, (2) what the options positioning
implies for near-term direction, (3) what whale/flow activity suggests.
Use specific numbers from the data above. Do not recommend trades.
Mark any uncertainty explicitly.
```

### Insight Structure

```json
{
  "asset": "btc",
  "generated_at_ns": 1785000000000000000,
  "input_bundle_hash": "abc123def456",
  "provider": "anthropic",
  "model_id": "claude-opus-4-8",
  "prompt_version": "1.0",
  "summary": "Bitcoin's options market shows...",
  "metrics_snapshot": {
    "spot": 104500.0,
    "iv_atm": 0.482,
    "net_gex": -12500000,
    "flow_npf": 3200000,
    "smart_net_delta": 850000,
    "netflow_velocity": -25000000
  },
  "flags": ["extreme_gex", "smart_money_accumulating", "exchange_outflow"],
  "confidence": "high"
}
```

### Flags (Automated, Pre-LLM)

Before the LLM sees the data, the composer computes boolean flags from
metric thresholds. These are deterministic, not LLM-generated:

| Flag | Condition | Meaning |
|---|---|---|
| `extreme_gex` | \|net_gex\| > 2× trailing 7d mean | Unusual gamma exposure |
| `smart_money_accumulating` | cohort.smart_flow > 0 for 3+ consecutive days | Smart money buying |
| `smart_money_distribution` | cohort.smart_flow < 0 for 3+ consecutive days | Smart money selling |
| `exchange_outflow` | netflow.velocity < −2σ for 24h | Significant exchange depletion |
| `exchange_inflow` | netflow.velocity > +2σ for 24h | Supply heading to market |
| `iv_extreme` | iv.regime ∈ {Rich, Cheap} | Volatility regime extreme |
| `whale_dominance` | whale_oi_ratio > 0.6 | Whales control >60% of OI |
| `flow_divergence` | smart_flow positive AND exchange_inflow | Contradictory signals |
| `max_pain_proximity` | \|spot − max_pain\| / spot < 0.02 | Price near max pain pin zone |

### REST API

```
GET /v1/insight?asset=btc
  → 200 { insight: { ... }, cached: bool, age_seconds: int }
  → 404 { error: "no insight available for asset" }
```

Response is cached for `cache_ttl_seconds` (default 3600). The terminal
(spec 041) fetches once on page load and shows a staleness indicator if
age > TTL.

### Telegram Bot

```
/insight btc    → returns the latest cached insight for BTC
/insight eth    → returns the latest cached insight for ETH
/insight        → returns insights for all watched assets
```

On-demand generation (not cached): if no cached insight exists or cache is
stale, the bot generates one synchronously and returns it.

### Scheduled Generation

Weekly cron (Sunday 08:00 UTC, after the cohort grading at 06:00):
generates insights for all configured `underlyings` (from
`features.toml [options_greeks].underlyings`). Each insight is archived
to `journal/insights/{asset}/{date}.json` (append-only, W-6).

## Requirements

- **TOK-1** The insight composer MUST read ONLY from structured feature
  exports (Parquet/JSON from the feature store). MUST NOT fetch external
  data, news, social media, or web pages (spec 010 grounding contract).

- **TOK-2** Every insight MUST carry an `InputBundle` with content hash
  (spec 010 RES-6). The prompt template MUST be versioned (spec 010 RES-8).
  The archive record MUST include {provider, model_id, prompt_version,
  bundle_hash, output, ts_ns} (spec 010 RES-6).

- **TOK-3** The composer MUST compute boolean flags deterministically from
  metric thresholds BEFORE passing data to the LLM. Flags are computed
  from (metrics, config) — no randomness (CONV-9). Flags MUST be included
  in both the prompt and the output.

- **TOK-4** The LLM output MUST be verified by `verify_grounded` (spec 010
  RES-5/6): every numeric claim in the summary must appear in the input
  bundle. Ungrounded numbers cause the insight to be rejected with a P3
  alert, never shipped.

- **TOK-5** The REST endpoint MUST serve cached insights with staleness
  metadata (`cached`, `age_seconds`). MUST NOT generate on every request
  (rate-limit the LLM). Cache TTL is configurable (default 3600s).

- **TOK-6** The insight MUST include the full `metrics_snapshot` (the raw
  numbers the LLM summarized) so the terminal can display them alongside
  the narrative. The narrative is supplementary; the numbers are primary.

- **TOK-7** On LLM failure (timeout, rate limit, ungrounded output), the
  system MUST return the last cached insight with `stale: true` and a P3
  alert (RES-5). MUST NOT return an empty response or fabricated summary.

- **TOK-8** Config MUST be TOML `serde(deny_unknown_fields)` (CONV-16).
  Parameters: `underlyings` (which assets to generate for), `provider`
  (LLM provider), `model_id`, `cache_ttl_seconds`, `flag_thresholds`
  (overridable flag conditions), `max_tokens` (default 500).

- **TOK-9** The insight archive MUST be append-only (W-6). Daily insights
  for each asset are stored in `journal/insights/{asset}/{date}.json`.
  Re-running the same day overwrites the daily file (latest wins, not
  append — same as spec 038 daily IV buckets).

- **TOK-10** Tests MUST verify: (a) the composer produces valid
  InputBundle with correct hash, (b) flag computation is deterministic,
  (c) `verify_grounded` catches ungrounded numbers, (d) the REST endpoint
  returns cached insights with correct staleness, (e) LLM failure degrades
  gracefully. No network in unit tests (CONV-23); LLM integration tests
  are behind `live-http` feature gate.

## Acceptance criteria

- [ ] `tok_1_composer_reads_only_feature_exports` — assert no file/network
  I/O in the composer path (mock the feature store).
- [ ] `tok_2_input_bundle_hash_is_deterministic` — same metrics → same hash.
- [ ] `tok_3_flags_computed_deterministically` — same metrics + thresholds
  → same flags (golden).
- [ ] `tok_4_verify_grounded_catches_ungrounded_numbers` — summary with
  invented number → rejected.
- [ ] `tok_5_rest_returns_cached_with_staleness` — second request within
  TTL returns cached=true.
- [ ] `tok_6_metrics_snapshot_included_in_output` — output contains the raw
  numbers alongside the narrative.
- [ ] `tok_7_llm_failure_returns_stale_cache` — mock LLM error → last
  cached insight with stale=true.
- [ ] `tok_8_check_config_rejects_unknown_fields` — TOML parse rejects
  typos.
- [ ] `tok_9_archive_is_append_only` — re-running same day updates the
  daily file, never creates duplicates.
- [ ] `tok_10_grounding_contract_end_to_end` — full pipeline: feature
  exports → composer → LLM (mocked) → verify_grounded → archive.

## Decisions

- 2026-08-23: New spec (iCrypto.ai "AI Insight" mapped to Freebuff's
  spec 010 LLM infrastructure + specs 037–040/042–043 feature data). The
  LLM provider abstraction and grounding contract already exist; this spec
  wires them to per-token analytics.

- 2026-08-23: Flags are pre-LLM, not LLM-generated. The LLM summarizes
  the flags and metrics; it does not compute them. This keeps the flag
  logic deterministic and testable (CONV-9), while the narrative is
  creative (appropriate for LLM).

- 2026-08-23: Weekly scheduled generation (not daily) in v1 — the
  underlying features (options flow, whale positioning) change slowly enough
  for swing horizons. Daily generation is a config toggle for v2.

- 2026-08-23: REST endpoint caches with TTL instead of generating
  on-demand — LLM calls are expensive and rate-limited. The terminal
  refreshes once per page load; Telegram gets the cache hit. On-demand
  generation is a v2 feature.

- 2026-08-23: The insight includes the raw `metrics_snapshot` (TOK-6)
  because the narrative is supplementary. A trader looks at the numbers
  first and reads the narrative for interpretation. If the LLM fails, the
  numbers are still useful.

- 2026-08-23: Archive uses `{asset}/{date}.json` (one file per day,
  latest wins) instead of append-only JSONL because the same asset may be
  regenerated multiple times per day (scheduled + on-demand). The latest
  version is the truth; history is the weekly schedule's archives.

## Open questions

- Should the insight include a confidence score? The LLM could self-assess
  how extreme the metrics are (all normal → low confidence; multiple flags
  → high confidence). Deferred — the boolean flags already capture
  extremity.

- Token-specific context: should the prompt include the asset's recent
  price history (e.g., 7d OHLCV) alongside derivatives data? This would
  help the LLM contextualize flow signals. Deferred to v2 — adds
  complexity to the InputBundle.

- 2026-08-24 (audit closeout): Status corrected from "implemented" to
  **implementing**. The deterministic composer core exists
  (`research/insight_composer.py`, `run_insight.py`) but ZERO `tok_*`
  acceptance tests exist, so CONV-21 cannot verify TOK-1..10. "Implemented"
  without tests is a PD-5/W-7 violation; the status flips back only when the
  test list lands (deterministic parts first: flags TOK-3, prompt TOK-2,
  grounding TOK-4, hash RES-6; LLM-dependent paths after).
